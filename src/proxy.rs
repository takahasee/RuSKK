use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, error, info, warn};

use crate::backend::Backend;
use crate::encoding::{decode_midashi, format_candidates_response, merge_candidates, parse_candidates};
use crate::frequency::SharedPredictor;
use crate::protocol::{is_found, parse_request, Request};

const MAX_LINE_BYTES: u64 = 8192;

pub struct Proxy {
    pub listen: String,
    pub primary: Backend,
    pub fallback: Backend,
    pub predictor: SharedPredictor,
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
    let mut session_context: Vec<String> = Vec::new();

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
                writer.write_all(b"skk-proxy/0.1.0 ").await?;
            }
            Request::Host => {
                let host = format!("skk-proxy/{}: ", proxy.listen);
                writer.write_all(host.as_bytes()).await?;
            }
            Request::Lookup(ref midashi) => {
                let midashi_str = decode_midashi(midashi);
                let (response, hit) = lookup_with_fallback(&proxy, &request).await;
                let final_response = if is_found(&response) {
                    let cands = parse_candidates(&response);
                    if !cands.is_empty() {
                        // yaskkserv2（Fallback）ヒット時のみベイズ推定で候補を並び替え。
                        // azooKey（Primary）ヒット時は azooKey の候補順序を維持する。
                        let ranked = if hit == BackendHit::Fallback {
                            let guard = proxy.predictor.lock().await;
                            guard.rank_candidates(&session_context, &midashi_str, &cands)
                        } else {
                            cands
                        };

                        if let Some(top_cand) = ranked.first() {
                            // yaskkserv2（Fallback）ヒット時のみ履歴ファイルに学習データを記録。
                            // azooKey の日常単語で履歴を汚染しない。
                            if hit == BackendHit::Fallback {
                                info!(midashi = %midashi_str, top_cand = %top_cand, context = ?session_context, "recorded yaskkserv2 selection to history");
                                let mut guard = proxy.predictor.lock().await;
                                guard.observe(&session_context, &midashi_str, top_cand);
                            }

                            // 直前の漢字文脈を1単語のみ保持
                            session_context.clear();
                            session_context.push(top_cand.clone());
                        }
                        format_candidates_response(&ranked)
                    } else {
                        response
                    }
                } else {
                    response
                };
                writer.write_all(&final_response).await?;
            }
            Request::Completion(_) => {
                let response = completion_with_aggregation(&proxy, &request, &session_context).await;
                writer.write_all(&response).await?;
            }
        }
        writer.flush().await?;
    }

    Ok(())
}

/// どのバックエンドが応答したかを示す列挙型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendHit {
    Primary,
    Fallback,
    None,
}

/// Primary（azooKey）を先に照会し、ミス時に Fallback（yaskkserv2）を照会する。
/// 戻り値として (レスポンスバイト列, ヒットしたバックエンド) を返す。
async fn lookup_with_fallback(proxy: &Proxy, request: &Request) -> (Vec<u8>, BackendHit) {
    match proxy.primary.query(request).await {
        Ok(response) if is_found(&response) => {
            debug!(backend = %proxy.primary.name, "hit");
            return (response, BackendHit::Primary);
        }
        Ok(_) => {
            debug!(
                backend = %proxy.primary.name,
                "miss, trying fallback"
            );
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
            let hit = if is_found(&response) {
                debug!(backend = %proxy.fallback.name, "hit");
                BackendHit::Fallback
            } else {
                debug!(backend = %proxy.fallback.name, "miss");
                BackendHit::None
            };
            (response, hit)
        }
        Err(err) => {
            warn!(
                backend = %proxy.fallback.name,
                error = %err,
                "fallback failed"
            );
            (b"4\n".to_vec(), BackendHit::None)
        }
    }
}

async fn completion_with_aggregation(proxy: &Proxy, request: &Request, context: &[String]) -> Vec<u8> {
    let midashi_str = match request {
        Request::Completion(midashi) => decode_midashi(midashi),
        _ => String::new(),
    };

    let primary_fut = proxy.primary.query(request);
    let fallback_fut = proxy.fallback.query(request);

    let (primary_res, fallback_res) = tokio::join!(primary_fut, fallback_fut);

    let primary_cands = match primary_res {
        Ok(resp) if is_found(&resp) => parse_candidates(&resp),
        _ => Vec::new(),
    };

    let fallback_cands = match fallback_res {
        Ok(resp) if is_found(&resp) => parse_candidates(&resp),
        _ => Vec::new(),
    };

    if primary_cands.is_empty() && fallback_cands.is_empty() {
        return b"4\n".to_vec();
    }

    let merged = merge_candidates(&primary_cands, &fallback_cands);
    let ranked = {
        let guard = proxy.predictor.lock().await;
        guard.rank_candidates(context, &midashi_str, &merged)
    };

    debug!(
        primary_count = primary_cands.len(),
        fallback_count = fallback_cands.len(),
        merged_count = merged.len(),
        "completion candidates aggregated and ranked"
    );

    format_candidates_response(&ranked)
}

