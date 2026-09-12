use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::backend::Backend;
use crate::encoding::{
    decode_midashi, extract_first_candidate, format_candidates_response_str,
    parse_candidates_borrowed,
};
use crate::frequency::SharedPredictor;
use crate::protocol::{is_found, parse_request, Request};

const MAX_LINE_BYTES: u64 = 8192;

pub struct Proxy {
    pub listen: String,
    pub primary: Backend,
    pub fallback: Backend,
    pub predictor: SharedPredictor,
    pub okuri_expansion: bool,
    pub context_ranking: bool,
}

#[derive(Debug, Default)]
struct SharedContextState {
    session_context: Vec<String>,
    pending_context: Option<(String, String, Instant)>,
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
        let shared_context = Arc::new(std::sync::Mutex::new(SharedContextState::default()));

        // Primary バックエンド（azooKey）をバックグラウンドでウォームアップ
        // （macOS の App Nap による初回スリープを解除し、モデルロードを先行完了させる）
        {
            let warmup_primary = proxy.primary.clone();
            tokio::spawn(async move {
                let req = Request::Lookup(b"\xca\xa1".to_vec()); // EUC-JP "あ"
                let _ = warmup_primary.query_with_timeout(&req, Duration::from_secs(3)).await;
                tracing::debug!("primary backend warmup complete");
            });
        }

        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    debug!(%peer, "client connected");
                    let _ = stream.set_nodelay(true);
                    let proxy = Arc::clone(&proxy);
                    let shared_context = Arc::clone(&shared_context);
                    tokio::spawn(async move {
                        if let Err(err) = handle_client(proxy, shared_context, stream).await {
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

async fn handle_client(
    proxy: Arc<Proxy>,
    shared_context: Arc<std::sync::Mutex<SharedContextState>>,
    stream: TcpStream,
) -> anyhow::Result<()> {
    let peer = stream.peer_addr().ok();
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::with_capacity(128);
    let mut conn_last_response_time: Option<Instant> = None;

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

        debug!(?peer, raw_line = ?String::from_utf8_lossy(&line), "received request from client");

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
                conn_last_response_time = Some(Instant::now());
            }
            Request::Host => {
                let host = format!("ruskk/{}: ", proxy.listen);
                writer.write_all(host.as_bytes()).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Lookup(ref midashi) => {
                let midashi_str = decode_midashi(midashi);
                let now = Instant::now();

                // 同一 TCP コネクション内での直前レスポンスからの経過時間を判定。
                // 50ms 未満の超高速な連続送信は、macSKK の補完候補展開に伴う機械的連射スキャンであるため、
                // 文脈確定（pending_context の昇格および更新）の対象外として保護する。
                let is_completion_burst = conn_last_response_time
                    .map(|t| now.duration_since(t) < Duration::from_millis(50))
                    .unwrap_or(false);

                // 複数 TCP 接続を跨いで文脈を安全に共有・判定する
                let session_ctx_snapshot = {
                    let mut ctx = shared_context.lock().unwrap_or_else(|e| e.into_inner());

                    // 文脈連動（context_ranking）が有効で、かつ補完連射でない場合、
                    // 手動での通常変換において前回の単語が確定されたかを判定して session_context に昇格する。
                    if proxy.context_ranking
                        && !is_completion_burst
                        && let Some((prev_midashi, prev_word, time)) = ctx.pending_context.take()
                    {
                        let is_same_midashi = prev_midashi == midashi_str;

                        if is_same_midashi {
                            // 同一見出し語での連続Lookup（次候補送り中、Space連打）なので保留を継続
                            ctx.pending_context = Some((prev_midashi, prev_word, time));
                        } else {
                            // 見出し語が変わったため、前回の単語が確定したと判定して昇格
                            if now.duration_since(time) <= Duration::from_secs(60) {
                                ctx.session_context.clear();
                                ctx.session_context.push(prev_word);
                                debug!(
                                    context = ?ctx.session_context,
                                    new_midashi = %midashi_str,
                                    "promoted pending context"
                                );
                            } else {
                                ctx.session_context.clear();
                            }
                        }
                    }

                    ctx.session_context.clone()
                };

                let ctx_ref = if proxy.context_ranking {
                    &session_ctx_snapshot[..]
                } else {
                    &[]
                };

                let mut okuri_handled = false;
                let mut final_response = None;
                let mut top_candidate_for_context: Option<String> = None;

                // 送りあり見出し（例: "かk" -> "かく", "交ぜGk" -> "交ぜがき", "まぜがk" -> "まぜがき"）の活用復元試行
                if proxy.okuri_expansion {
                    let variations = crate::okuri::expand_okuri_variations(&midashi_str);
                    let mut found_response = None;

                    for var in variations {
                        let okuri_req = Request::Lookup(var.query_midashi.as_bytes().to_vec());
                        let (okuri_resp, _hit) = lookup_with_fallback(&proxy, &okuri_req).await;

                        if is_found(&okuri_resp) {
                            let raw_cands = parse_candidates_borrowed(&okuri_resp);
                            let stem_cands = crate::okuri::extract_stem_candidates_borrowed(
                                &raw_cands,
                                var.okuri_suffix,
                                &var.query_midashi,
                            );

                            debug!(
                                midashi = %midashi_str,
                                query = %var.query_midashi,
                                suffix = %var.okuri_suffix,
                                resp = ?String::from_utf8_lossy(&okuri_resp),
                                stems = ?stem_cands,
                                "okuri variation checked"
                            );

                            if !stem_cands.is_empty() {
                                let ranked = {
                                    let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                                    guard.rank_candidates_borrowed(ctx_ref, &midashi_str, &stem_cands)
                                };
                                if let Some(&top) = ranked.first() {
                                    top_candidate_for_context = Some(top.to_string());
                                }
                                found_response = Some(format_candidates_response_str(&ranked));
                                break;
                            }
                        }
                    }

                    if let Some(resp) = found_response {
                        final_response = Some(resp);
                        okuri_handled = true;
                    }
                }

                // 送りなし見出し、または送り復元が無効／失敗した場合は従来通りの照会
                let resp_to_send = if okuri_handled && let Some(resp) = final_response {
                    resp
                } else {
                    let (response, _hit) = lookup_with_fallback(&proxy, &request).await;
                    if is_found(&response) {
                        // 【ファストパス】並び替えルールがあるか高速判定
                        let should_rank = {
                            let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                            guard.should_rank(ctx_ref, &midashi_str)
                        };

                        if should_rank {
                            // 並び替えルールがある場合のみ、ゼロコピーで借用パースして並び替え
                            let cands = parse_candidates_borrowed(&response);
                            if !cands.is_empty() {
                                let ranked = {
                                    let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                                    guard.rank_candidates_borrowed(ctx_ref, &midashi_str, &cands)
                                };
                                if let Some(&top) = ranked.first() {
                                    top_candidate_for_context = Some(top.to_string());
                                }
                                format_candidates_response_str(&ranked)
                            } else {
                                response
                            }
                        } else {
                            // 並び替えルールがない大部分の単語: パース・アロケーション完全スキップ（ゼロコピー直結！）
                            if let Some(top) = extract_first_candidate(&response) {
                                top_candidate_for_context = Some(top.to_string());
                            }
                            response
                        }
                    } else {
                        response
                    }
                };

                // 文脈連動が有効で、補完連射でなく、返却第1候補が漢字を含む場合のみ次回のための直前単語として保留する。
                if proxy.context_ranking
                    && !is_completion_burst
                    && let Some(ref top) = top_candidate_for_context
                {
                    let clean = crate::frequency::clean_candidate(top);
                    if contains_kanji(clean) {
                        let mut ctx = shared_context.lock().unwrap_or_else(|e| e.into_inner());
                        ctx.pending_context = Some((midashi_str.to_string(), clean.to_string(), now));
                    }
                }

                debug!(?peer, resp = ?String::from_utf8_lossy(&resp_to_send), "sending response to client");
                writer.write_all(&resp_to_send).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Completion(_) => {
                let (response, hit) = lookup_with_fallback(&proxy, &request).await;
                debug!(?peer, ?hit, resp = ?String::from_utf8_lossy(&response), "completion upstream response");
                writer.write_all(&response).await?;
                conn_last_response_time = Some(Instant::now());
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
    // macSKK の 1.0秒 (1000ms) 制限を絶対に超えないよう、全体デッドラインを 850ms に制限
    let overall_deadline = Duration::from_millis(850);
    let primary_timeout = proxy.primary.timeout.min(Duration::from_millis(600));

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
        overall_deadline - elapsed
    } else {
        debug!("overall deadline reached before fallback, returning not found immediately");
        return (b"4\n".to_vec(), BackendHit::None);
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

/// 文字列にCJK漢字が含まれているかを判定する。
/// 文脈候補（session_context / pending_context）には漢字を含む単語のみを積む。
fn contains_kanji(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'   // CJK統合漢字
            | '\u{3400}'..='\u{4DBF}' // CJK統合漢字拡張A
            | '\u{F900}'..='\u{FAFF}' // CJK互換漢字
        )
    })
}
