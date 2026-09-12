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
    pub context_ranking: bool,
}

#[derive(Debug, Default)]
struct SharedContextState {
    session_context: Vec<String>,
    pending_context: Option<(String, String, Instant)>,
    last_response_time: Option<Instant>,
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
        let shared_context = Arc::new(tokio::sync::Mutex::new(SharedContextState::default()));

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
    shared_context: Arc<tokio::sync::Mutex<SharedContextState>>,
    stream: TcpStream,
) -> anyhow::Result<()> {
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
                let now = Instant::now();

                // 複数 TCP 接続を跨いで文脈を安全に共有・判定する
                let (is_completion_scan, session_ctx_snapshot) = {
                    let mut ctx = shared_context.lock().await;

                    // macSKK の自動補完スキャンの検出:
                    // 直前のレスポンス返却から 60ms 未満の超短時間で届いた場合（macSKK のローカル辞書補完連射）。
                    let is_completion_scan = ctx
                        .last_response_time
                        .map(|t| now.duration_since(t) < Duration::from_millis(60))
                        .unwrap_or(false);

                    // 文脈連動（context_ranking）が有効な場合、
                    // 手動での通常変換（補完スキャンでない）かつ見出し語が変わった時点で、
                    // 前回の単語が確定されたとみなして session_context に昇格する。
                    if proxy.context_ranking {
                        if is_completion_scan {
                            debug!(
                                midashi = %midashi_str,
                                "ignored rapid completion scan from context promotion"
                            );
                        } else if let Some((prev_midashi, prev_word, time)) = ctx.pending_context.take() {
                            if prev_midashi != midashi_str {
                                // 60秒以内の入力のみ文脈として保持
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
                            } else {
                                // 同一見出し語での連続Lookup（次候補送り中、Space連打）なので保留を継続
                                ctx.pending_context = Some((prev_midashi, prev_word, time));
                            }
                        }
                    }

                    (is_completion_scan, ctx.session_context.clone())
                };

                let ctx_ref = if proxy.context_ranking {
                    &session_ctx_snapshot[..]
                } else {
                    &[]
                };

                let mut okuri_handled = false;
                let mut final_response = None;
                let mut top_candidate_for_context = None;

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
                                guard.rank_candidates(ctx_ref, &midashi_str, &stem_cands)
                            };
                            if let Some(top) = ranked.first() {
                                top_candidate_for_context = Some(top.clone());
                            }
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
                            let ranked = {
                                let guard = proxy.predictor.lock().await;
                                guard.rank_candidates(ctx_ref, &midashi_str, &cands)
                            };
                            if let Some(top) = ranked.first() {
                                top_candidate_for_context = Some(top.clone());
                            }
                            format_candidates_response(&ranked)
                        } else {
                            response
                        }
                    } else {
                        response
                    }
                };

                // 通常の手動変換（補完スキャンでない）であり、かつ返却第1候補が漢字を含む場合のみ保留する。
                // macSKK の内部補完スキャンで返した単語は pending_context に入れず、前回の正当な保留を保護する。
                if proxy.context_ranking
                    && !is_completion_scan
                    && let Some(ref top) = top_candidate_for_context
                {
                    let clean = crate::frequency::clean_candidate(top);
                    if contains_kanji(clean) {
                        let mut ctx = shared_context.lock().await;
                        ctx.pending_context = Some((midashi_str.clone(), clean.to_string(), now));
                    }
                }

                writer.write_all(&resp_to_send).await?;
            }
            Request::Completion(_) => {
                writer.write_all(b"4\n").await?;
            }
        }
        writer.flush().await?;
        {
            let mut ctx = shared_context.lock().await;
            ctx.last_response_time = Some(Instant::now());
        }
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
    let overall_deadline = (proxy.primary.timeout + proxy.fallback.timeout).max(Duration::from_millis(1500));
    let primary_timeout = proxy.primary.timeout;

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


