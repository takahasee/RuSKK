use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::encoding::decode_euc_response_to_utf8;
use crate::protocol::{Request, encode_request, encode_request_for_encoding};

const MAX_BACKEND_RESPONSE_BYTES: usize = 65536; // 64KB
const DEFAULT_MAX_POOL_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamEncoding {
    /// Request: UTF-8, Response: UTF-8
    Utf8,
    /// Request: EUC-JP, Response: UTF-8 (azoo-key-skkserv)
    EucJpRequestUtf8Response,
    /// Request: EUC-JP, Response: EUC-JP (yaskkserv2)
    EucJp,
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub name: Arc<str>,
    pub addr: SocketAddr,
    pub encoding: UpstreamEncoding,
    pub timeout: Duration,
    pool: Arc<Mutex<Vec<BufReader<TcpStream>>>>,
    max_pool_size: usize,
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("{backend}: connect failed: {source}")]
    Connect {
        backend: String,
        source: std::io::Error,
    },
    #[error("{backend}: timed out after {timeout:?}")]
    Timeout { backend: String, timeout: Duration },
    #[error("{backend}: I/O error: {source}")]
    Io {
        backend: String,
        source: std::io::Error,
    },
    #[error("{backend}: empty response")]
    EmptyResponse { backend: String },
}

impl Backend {
    pub fn new(
        name: impl Into<Arc<str>>,
        addr: SocketAddr,
        encoding: UpstreamEncoding,
        timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            addr,
            encoding,
            timeout,
            pool: Arc::new(Mutex::new(Vec::with_capacity(DEFAULT_MAX_POOL_SIZE))),
            max_pool_size: DEFAULT_MAX_POOL_SIZE,
        }
    }

    /// 新規 TCP 接続を確立する
    async fn connect_fresh(&self) -> Result<BufReader<TcpStream>, BackendError> {
        match TcpStream::connect(self.addr).await {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                Ok(BufReader::new(stream))
            }
            Err(source) => Err(BackendError::Connect {
                backend: self.name.to_string(),
                source,
            }),
        }
    }

    /// アイドル状態の接続をプールから取得するか、新規接続を作成する。
    /// 戻り値の bool はプール由来の再利用接続（true）か新規接続（false）かを示す。
    async fn acquire_conn(&self) -> Result<(BufReader<TcpStream>, bool), BackendError> {
        {
            let mut guard = self.pool.lock().await;
            if let Some(stream) = guard.pop() {
                return Ok((stream, true));
            }
        }
        self.connect_fresh().await.map(|s| (s, false))
    }

    /// 正常終了した接続をプールに返却する。
    /// BufReader に未読の余剰データが残っている場合は、次回以降の応答デシンクを防ぐため破棄する。
    async fn release_conn(&self, stream: BufReader<TcpStream>) {
        if !stream.buffer().is_empty() {
            warn!(
                backend = %self.name,
                buffered = stream.buffer().len(),
                "discarding connection with unread trailing bytes"
            );
            return;
        }
        let mut guard = self.pool.lock().await;
        if guard.len() < self.max_pool_size {
            guard.push(stream);
        }
    }

    /// 接続プールをクリアする
    pub async fn clear_pool(&self) {
        let mut guard = self.pool.lock().await;
        guard.clear();
    }

    pub async fn query(&self, request: &Request<'_>) -> Result<Vec<u8>, BackendError> {
        self.query_with_timeout(request, self.timeout).await
    }

    pub async fn query_with_timeout(
        &self,
        request: &Request<'_>,
        timeout_duration: Duration,
    ) -> Result<Vec<u8>, BackendError> {
        // EUC-JP リクエストを必要とするバックエンドは見出し語を EUC-JP にエンコード
        let wire = match self.encoding {
            UpstreamEncoding::EucJp | UpstreamEncoding::EucJpRequestUtf8Response => {
                encode_request_for_encoding(request)
            }
            UpstreamEncoding::Utf8 => encode_request(request),
        };
        debug!(
            backend = %self.name,
            addr = %self.addr,
            request_len = wire.len(),
            timeout_ms = timeout_duration.as_millis(),
            "query upstream"
        );

        let result = self.query_inner(&wire, timeout_duration).await;
        match result {
            Ok(raw) => {
                let response = match self.encoding {
                    UpstreamEncoding::Utf8 | UpstreamEncoding::EucJpRequestUtf8Response => {
                        ensure_trailing_newline(raw)
                    }
                    UpstreamEncoding::EucJp => {
                        ensure_trailing_newline(decode_euc_response_to_utf8(&raw))
                    }
                };
                debug!(
                    backend = %self.name,
                    response_len = response.len(),
                    "upstream ok"
                );
                Ok(response)
            }
            Err(err) => {
                warn!(backend = %self.name, error = %err, "upstream error");
                Err(err)
            }
        }
    }

    async fn query_inner(
        &self,
        wire: &[u8],
        timeout_duration: Duration,
    ) -> Result<Vec<u8>, BackendError> {
        let res = timeout(timeout_duration, async {
            let mut last_err = None;
            let mut force_fresh = false;
            for attempt in 0..2 {
                let (mut reader, from_pool) = if force_fresh {
                    (self.connect_fresh().await?, false)
                } else {
                    self.acquire_conn().await?
                };

                match self.exchange_once(&mut reader, wire, attempt).await {
                    Ok(buf) => {
                        // 正常に送受信完了した接続のみプールへ返却
                        self.release_conn(reader).await;
                        return Ok(buf);
                    }
                    Err((err, false)) => return Err(err),
                    Err((err, true)) => {
                        if !from_pool {
                            // 最初から新規接続で失敗した場合は無駄な 2 回目リトライを行わず即返す
                            return Err(err);
                        }
                        // スリープや再起動でプール内の複数ソケットが同時に失効している可能性があるため、
                        // プールを一括破棄し、再試行は必ず新規接続で行う
                        self.clear_pool().await;
                        force_fresh = true;
                        last_err = Some(err);
                    }
                }
            }

            Err(last_err.unwrap_or_else(|| BackendError::EmptyResponse {
                backend: self.name.to_string(),
            }))
        })
        .await;

        match res {
            Ok(inner) => inner,
            Err(_) => Err(BackendError::Timeout {
                backend: self.name.to_string(),
                timeout: timeout_duration,
            }),
        }
    }

    async fn exchange_once(
        &self,
        reader: &mut BufReader<TcpStream>,
        wire: &[u8],
        attempt: usize,
    ) -> Result<Vec<u8>, (BackendError, bool)> {
        if let Err(source) = reader.get_mut().write_all(wire).await {
            debug!(backend = %self.name, attempt, error = %source, "write failed on connection, resetting");
            return Err((
                BackendError::Io {
                    backend: self.name.to_string(),
                    source,
                },
                true,
            ));
        }
        if let Err(source) = reader.get_mut().flush().await {
            debug!(backend = %self.name, attempt, error = %source, "flush failed on connection, resetting");
            return Err((
                BackendError::Io {
                    backend: self.name.to_string(),
                    source,
                },
                true,
            ));
        }

        let mut buf = Vec::with_capacity(1024);
        let mut take_reader = reader.take(MAX_BACKEND_RESPONSE_BYTES as u64 + 1);
        match take_reader.read_until(b'\n', &mut buf).await {
            Ok(n) if n > 0 && !buf.is_empty() => {
                if buf.len() > MAX_BACKEND_RESPONSE_BYTES {
                    warn!(backend = %self.name, len = buf.len(), "backend response exceeds max bytes");
                    return Err((
                        BackendError::EmptyResponse {
                            backend: self.name.to_string(),
                        },
                        false,
                    ));
                }
                if !buf.ends_with(b"\n") {
                    debug!(backend = %self.name, attempt, len = buf.len(), "backend response missing newline (EOF), resetting");
                    return Err((
                        BackendError::EmptyResponse {
                            backend: self.name.to_string(),
                        },
                        true,
                    ));
                }
                Ok(buf)
            }
            Ok(_) => {
                debug!(backend = %self.name, attempt, "empty response on persistent connection, resetting");
                Err((
                    BackendError::EmptyResponse {
                        backend: self.name.to_string(),
                    },
                    true,
                ))
            }
            Err(source) => {
                debug!(backend = %self.name, attempt, error = %source, "read error on connection, resetting");
                Err((
                    BackendError::Io {
                        backend: self.name.to_string(),
                        source,
                    },
                    true,
                ))
            }
        }
    }
}

fn ensure_trailing_newline(mut response: Vec<u8>) -> Vec<u8> {
    if !response.ends_with(b"\n") {
        response.push(b'\n');
    }
    response
}
