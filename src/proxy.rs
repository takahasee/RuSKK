use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::backend::Backend;
use crate::encoding::{decode_midashi, format_candidates_response, parse_candidates};
use crate::frequency::SharedPredictor;
use crate::protocol::{is_found, parse_request, Request};

const MAX_LINE_BYTES: u64 = 8192;

pub struct Proxy {
    pub listen: String,
    pub primary: Backend,
    pub fallback: Backend,
    pub predictor: SharedPredictor,
    pub okuri_expansion: bool,
}

impl Proxy {
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.listen).await?;
        info!(listen = %self.listen, "ruskk listening");
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

        // 8KB を超える改行なし不正データによるメモリ枯渇 (OOM DoS) を防ぐため、take で上限を設定
        let mut take_reader = (&mut reader).take(MAX_LINE_BYTES + 1);
        let n = take_reader.read_until(b'\n', &mut line).await?;

        if n == 0 {
            break;
        }

        if line.len() > MAX_LINE_BYTES as usize || !line.ends_with(b"\n") {
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
                writer.write_all(b"ruskk/0.1.0 ").await?;
            }
            Request::Host => {
                let host = format!("ruskk/{}: ", proxy.listen);
                writer.write_all(host.as_bytes()).await?;
            }
            Request::Lookup(ref midashi) => {
                let midashi_str = decode_midashi(midashi);

                let mut okuri_handled = false;
                let mut final_response = None;

                // 送りあり見出し（例: "かk" -> "かく", "きr" -> "きる"）の活用復元試行
                if proxy.okuri_expansion
                    && let Some((full_kana, okuri_suffix)) =
                        crate::okuri::expand_okuri_to_full_kana(&midashi_str)
                {
                    let okuri_req = Request::Lookup(full_kana.as_bytes().to_vec());
                        let (okuri_resp, _hit) = lookup_with_fallback(&proxy, &okuri_req).await;

                        if is_found(&okuri_resp) {
                            let raw_cands = parse_candidates(&okuri_resp);
                            let stem_cands = crate::okuri::extract_stem_candidates(&raw_cands, okuri_suffix);

                            if !stem_cands.is_empty() {
                                debug!(
                                    midashi = %midashi_str,
                                    full_kana = %full_kana,
                                    stems_count = stem_cands.len(),
                                    "okuri expansion resolved candidates"
                                );
                                let ranked = {
                                    let guard = proxy.predictor.lock().await;
                                    guard.rank_candidates(&[], &midashi_str, &stem_cands)
                                };
                                final_response = Some(format_candidates_response(&ranked));
                                okuri_handled = true;
                            }
                        }
                    }

                // 送りなし見出し、または送り復元が無効／失敗した場合は従来通りの照会
                let resp_to_send = if okuri_handled && let Some(resp) = final_response {
                    resp
                } else {
                    let (response, _hit) = lookup_with_fallback(&proxy, &request).await;
                    if is_found(&response) {
                        let cands = parse_candidates(&response);
                        if !cands.is_empty() {
                            // seed ファイルの頻度データに基づいて候補を並び替える。
                            // サーバー側での自動確定推測（pending_context等）は一切行わず、
                            // ユーザーが手動で確定する純粋な通常変換として返却する。
                            let ranked = {
                                let guard = proxy.predictor.lock().await;
                                guard.rank_candidates(&[], &midashi_str, &cands)
                            };
                            format_candidates_response(&ranked)
                        } else {
                            response
                        }
                    } else {
                        response
                    }
                };
                writer.write_all(&resp_to_send).await?;
            }
            Request::Completion(_) => {
                // 補完クエリに対しては常に「候補なし (4\n)」を返す。
                // macSKK が補完候補を取得すると、入力停止後（約0.3秒〜0.5秒）に
                // 候補を勝手にテキストに確定出力（addFixedText）してしまうため、
                // 補完候補の返却を停止し、勝手な自動確定を 100% 物理的に防止する。
                // 確定はユーザーが Space で通常変換（Lookup）して手動で行う。
                writer.write_all(b"4\n").await?;
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
/// 全体デッドライン（950ms）から動的タイムアウトを計算し、macSKK の 1.0秒制限を超えないようにする。
/// 戻り値として (レスポンスバイト列, ヒットしたバックエンド) を返す。
async fn lookup_with_fallback(proxy: &Proxy, request: &Request) -> (Vec<u8>, BackendHit) {
    let start = Instant::now();
    let overall_deadline = Duration::from_millis(950);
    let primary_timeout = proxy.primary.timeout.min(Duration::from_millis(300));

    match proxy.primary.query_with_timeout(request, primary_timeout).await {
        Ok(response) if is_found(&response) => {
            debug!(
                backend = %proxy.primary.name,
                elapsed_ms = start.elapsed().as_millis(),
                "hit"
            );
            return (response, BackendHit::Primary);
        }
        Ok(_) => {
            debug!(
                backend = %proxy.primary.name,
                elapsed_ms = start.elapsed().as_millis(),
                "miss, trying fallback"
            );
        }
        Err(err) => {
            warn!(
                backend = %proxy.primary.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "primary failed, trying fallback"
            );
        }
    }

    let elapsed = start.elapsed();
    let fallback_timeout = if overall_deadline > elapsed {
        (overall_deadline - elapsed).max(Duration::from_millis(200))
    } else {
        Duration::from_millis(200)
    };

    match proxy.fallback.query_with_timeout(request, fallback_timeout).await {
        Ok(response) => {
            let hit = if is_found(&response) {
                debug!(
                    backend = %proxy.fallback.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "hit"
                );
                BackendHit::Fallback
            } else {
                debug!(
                    backend = %proxy.fallback.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "miss"
                );
                BackendHit::None
            };
            (response, hit)
        }
        Err(err) => {
            warn!(
                backend = %proxy.fallback.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "fallback failed"
            );
            (b"4\n".to_vec(), BackendHit::None)
        }
    }
}

