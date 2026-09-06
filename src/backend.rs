use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::encoding::response_euc_to_utf8;
use crate::protocol::{encode_request, encode_request_for_encoding, Request};

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
    pub async fn query(&self, request: &Request) -> Result<Vec<u8>, BackendError> {
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
            "query upstream"
        );

        let result = timeout(self.timeout, self.query_inner(&wire)).await;
        match result {
            Ok(Ok(raw)) => {
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
            Ok(Err(err)) => {
                warn!(backend = %self.name, error = %err, "upstream error");
                Err(err)
            }
            Err(_) => {
                warn!(
                    backend = %self.name,
                    timeout_ms = self.timeout.as_millis(),
                    "upstream timeout"
                );
                Err(BackendError::Timeout {
                    backend: self.name.clone(),
                    timeout: self.timeout,
                })
            }
        }
    }

    async fn query_inner(&self, wire: &[u8]) -> Result<Vec<u8>, BackendError> {
        let mut stream = TcpStream::connect(self.addr)
            .await
            .map_err(|source| BackendError::Connect {
                backend: self.name.clone(),
                source,
            })?;

        stream
            .write_all(wire)
            .await
            .map_err(|source| BackendError::Io {
                backend: self.name.clone(),
                source,
            })?;

        // skkserv responses are a single line ending with LF.
        let mut buf = Vec::with_capacity(4096);
        let mut byte = [0u8; 1];
        loop {
            let n = stream
                .read(&mut byte)
                .await
                .map_err(|source| BackendError::Io {
                    backend: self.name.clone(),
                    source,
                })?;
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
            // Safety cap against runaway responses.
            if buf.len() > 64 * 1024 {
                break;
            }
        }

        if buf.is_empty() {
            return Err(BackendError::EmptyResponse {
                backend: self.name.clone(),
            });
        }
        Ok(buf)
    }
}

fn ensure_trailing_newline(mut response: Vec<u8>) -> Vec<u8> {
    if !response.ends_with(b"\n") {
        response.push(b'\n');
    }
    response
}
