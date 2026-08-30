use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use crate::backend::Backend;
use crate::protocol::{is_found, parse_request, Request};

const MAX_LINE_BYTES: u64 = 8192;

pub struct Proxy {
    pub listen: String,
    pub primary: Backend,
    pub fallback: Backend,
}

impl Proxy {
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.listen).await?;
        info!(listen = %self.listen, "skk-proxy listening");
        info!(
            primary = %self.primary.name,
            primary_addr = %self.primary.addr,
            fallback = %self.fallback.name,
            fallback_addr = %self.fallback.addr,
            "backends configured"
        );

        let proxy = Arc::new(self);

        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    debug!(%peer, "client connected");
                    let proxy = Arc::clone(&proxy);
                    tokio::spawn(async move {
                        if let Err(err) = handle_client(proxy, stream).await {
                            debug!(%peer, error = %err, "client session ended");
                        }
                    });
                }
                Err(err) => {
                    error!(error = %err, "accept failed");
                }
            }
        }
    }
}

async fn handle_client(proxy: Arc<Proxy>, stream: TcpStream) -> anyhow::Result<()> {
    let peer = stream.peer_addr().ok();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();

    loop {
        line.clear();
        let n = (&mut reader)
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut line)
            .await?;
        if n == 0 {
            break;
        }

        if !line.ends_with(b"\n") {
            warn!(?peer, len = line.len(), "request line too long or missing newline");
            break;
        }

        // Allow CR LF.
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
        } else if line.ends_with(b"\n") {
            line.truncate(line.len() - 1);
        }

        let request = match parse_request(&line) {
            Ok(req) => req,
            Err(err) => {
                warn!(?peer, error = %err, "invalid request");
                break;
            }
        };

        match request {
            Request::End => {
                debug!(?peer, "client end");
                break;
            }
            Request::Version => {
                writer
                    .write_all(b"skk-proxy/0.1.0 ")
                    .await?;
            }
            Request::Host => {
                let host = format!("skk-proxy/{}: ", proxy.listen);
                writer.write_all(host.as_bytes()).await?;
            }
            Request::Lookup(_) | Request::Completion(_) => {
                let response = lookup_with_fallback(&proxy, &request).await;
                writer.write_all(&response).await?;
            }
        }
        writer.flush().await?;
    }

    Ok(())
}

async fn lookup_with_fallback(proxy: &Proxy, request: &Request) -> Vec<u8> {
    match proxy.primary.query(request).await {
        Ok(response) if is_found(&response) => {
            debug!(backend = %proxy.primary.name, "hit");
            return response;
        }
        Ok(response) => {
            debug!(
                backend = %proxy.primary.name,
                "miss, trying fallback"
            );
            let _ = response; // not-found; fall through
        }
        Err(err) => {
            warn!(
                backend = %proxy.primary.name,
                error = %err,
                "primary failed, trying fallback"
            );
        }
    }

    match proxy.fallback.query(request).await {
        Ok(response) => {
            if is_found(&response) {
                debug!(backend = %proxy.fallback.name, "hit");
            } else {
                debug!(backend = %proxy.fallback.name, "miss");
            }
            response
        }
        Err(err) => {
            warn!(
                backend = %proxy.fallback.name,
                error = %err,
                "fallback failed"
            );
            b"4\n".to_vec()
        }
    }
}
