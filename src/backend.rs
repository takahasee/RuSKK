use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::encoding::response_euc_to_utf8;
use crate::protocol::{encode_request, encode_request_for_encoding, Request};

const MAX_BACKEND_RESPONSE_BYTES: usize = 65536; // 64KB

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
    pub name: String,
    pub addr: SocketAddr,
    pub encoding: UpstreamEncoding,
    pub timeout: Duration,
    pub connection: Arc<Mutex<Option<BufReader<TcpStream>>>>,
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
        name: impl Into<String>,
        addr: SocketAddr,
        encoding: UpstreamEncoding,
        timeout: Duration,
    ) -> Self {
        Self {
            name: name.into(),
            addr,
            encoding,
            timeout,
            connection: Arc::new(Mutex::new(None)),
        }
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
                        ensure_trailing_newline(response_euc_to_utf8(&raw))
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
        let mut guard = self.connection.lock().await;

        let res = timeout(timeout_duration, async {
            let mut last_err = None;
            for attempt in 0..2 {
                if guard.is_none() {
                    match TcpStream::connect(self.addr).await {
                        Ok(stream) => {
                            let _ = stream.set_nodelay(true);
                            *guard = Some(BufReader::new(stream));
                        }
                        Err(source) => {
                            return Err(BackendError::Connect {
                                backend: self.name.clone(),
                                source,
                            });
                        }
                    }
                }

                let reader = guard.as_mut().unwrap();

                // 接続にリクエストを送信し、確実にフラッシュ
                if let Err(source) = reader.get_mut().write_all(wire).await {
                    debug!(backend = %self.name, attempt, error = %source, "write failed on connection, resetting");
                    *guard = None;
                    last_err = Some(BackendError::Io {
                        backend: self.name.clone(),
                        source,
                    });
                    continue;
                }
                if let Err(source) = reader.get_mut().flush().await {
                    debug!(backend = %self.name, attempt, error = %source, "flush failed on connection, resetting");
                    *guard = None;
                    last_err = Some(BackendError::Io {
                        backend: self.name.clone(),
                        source,
                    });
                    continue;
                }

                let mut buf = Vec::with_capacity(1024);
                let mut take_reader = reader.take(MAX_BACKEND_RESPONSE_BYTES as u64 + 1);
                match take_reader.read_until(b'\n', &mut buf).await {
                    Ok(n) if n > 0 && !buf.is_empty() => {
                        if buf.len() > MAX_BACKEND_RESPONSE_BYTES || !buf.ends_with(b"\n") {
                            warn!(backend = %self.name, len = buf.len(), "backend response exceeds max bytes or missing newline");
                            *guard = None;
                            return Err(BackendError::EmptyResponse {
                                backend: self.name.clone(),
                            });
                        }
                        return Ok(buf);
                    }
                    Ok(_) => {
                        debug!(backend = %self.name, attempt, "empty response on persistent connection, resetting");
                        *guard = None;
                        last_err = Some(BackendError::EmptyResponse {
                            backend: self.name.clone(),
                        });
                        continue;
                    }
                    Err(source) => {
                        debug!(backend = %self.name, attempt, error = %source, "read error on connection, resetting");
                        *guard = None;
                        last_err = Some(BackendError::Io {
                            backend: self.name.clone(),
                            source,
                        });
                        continue;
                    }
                }
            }

            Err(last_err.unwrap_or_else(|| BackendError::EmptyResponse {
                backend: self.name.clone(),
            }))
        })
        .await;

        match res {
            Ok(inner) => inner,
            Err(_) => {
                // タイムアウト時は途中の送受信で接続状態が不整合になるため、確実に破棄
                *guard = None;
                Err(BackendError::Timeout {
                    backend: self.name.clone(),
                    timeout: timeout_duration,
                })
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
