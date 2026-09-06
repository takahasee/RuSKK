use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
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
    let mut session_context: Vec<String> = Vec::new();
    // skk-bayesian.el の pending 機構に相当。
    // Lookup 時点では observe() せず、次のリクエスト到着時または無入力タイムアウト時に確定とみなして学習する。
    let mut pending: Option<PendingObservation> = None;

    // macSKK の補完（Completion）クエリを検出・除外するためのトラッキング
    let mut recent_completions: HashMap<String, Instant> = HashMap::new();
    let mut recent_completion_prefixes: Vec<(String, Instant)> = Vec::new();
    let mut last_completion_time: Option<Instant> = None;
    let mut last_lookup_time: Option<Instant> = None;

    loop {
        line.clear();

        // macSKK は TCP 接続を切断せず使い回すため、
        // ユーザーが入力・確定後に放置した場合に備えて 3.0 秒無入力で自動 flush する。
        let timeout_duration = if pending.is_some() {
            Duration::from_millis(3000)
        } else {
            Duration::from_secs(3600)
        };

        let n = tokio::select! {
            res = reader.read_until(b'\n', &mut line) => {
                res?
            }
            _ = tokio::time::sleep(timeout_duration), if pending.is_some() => {
                flush_pending(&proxy, &mut pending).await;
                continue;
            }
        };

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
                flush_pending(&proxy, &mut pending).await;
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
                let now = Instant::now();
                let midashi_str = decode_midashi(midashi);

                // 有効期限（5秒）の切れた古い補完情報を掃除
                recent_completions.retain(|_, t| now.duration_since(*t) < Duration::from_secs(5));
                recent_completion_prefixes.retain(|(_, t)| now.duration_since(*t) < Duration::from_secs(5));

                // --- macSKK の補完クエリ（裏で自動送信される展開 Lookup）の除外判定 ---
                // 1. 直近 5 秒以内に返した補完候補一覧に含まれているか（skkserv返却の補完候補）
                let is_in_recent_completions = recent_completions.contains_key(&midashi_str);

                // 2. 直近 5 秒以内の Completion prefix に前方一致し、かつ prefix より長い単語か（ローカル辞書由来の補完候補）
                // ※送りあり変換（末尾が英字）はユーザーの通常変換なので除外
                let is_okuri = is_okuri_midashi(&midashi_str);
                let is_prefix_completion = !is_okuri
                    && recent_completion_prefixes.iter().any(|(prefix, _)| {
                        midashi_str.starts_with(prefix) && midashi_str != *prefix
                    });

                // 3. 直近 3 秒以内に Completion があり、かつ 200ms 未満のバースト Lookup か
                let is_rapid_burst = last_completion_time
                    .map(|t| now.duration_since(t) < Duration::from_secs(3))
                    .unwrap_or(false)
                    && last_lookup_time
                        .map(|t| now.duration_since(t) < Duration::from_millis(200))
                        .unwrap_or(false);

                // 4. macSKK がキー入力開始時（1文字目）に補完パネルを出すために自動送信してくる1文字Lookup
                // （例: "か", "れ", "に", "ほ" 等）。送りあり（例: "きr"）は2文字なので除外されない。
                let is_single_char_preview = midashi_str.chars().count() == 1
                    && midashi_str.chars().next().map(|c| !c.is_ascii()).unwrap_or(false);

                let is_completion_refer = is_in_recent_completions
                    || is_prefix_completion
                    || is_rapid_burst
                    || is_single_char_preview;
                last_lookup_time = Some(now);

                if is_completion_refer {
                    debug!(
                        midashi = %midashi_str,
                        in_recent = is_in_recent_completions,
                        is_prefix = is_prefix_completion,
                        is_burst = is_rapid_burst,
                        single_char = is_single_char_preview,
                        "ignored completion refer lookup from learning"
                    );
                } else {
                    // もし今回の見出し語が直前の保留見出し語を延長したもの（例: "か" -> "かって"）なら、
                    // 直前の保留は入力途中の文字に過ぎないため、確定（flush）せずに破棄する。
                    if let Some(ref p) = pending {
                        if midashi_str.starts_with(&p.midashi) && midashi_str != p.midashi {
                            debug!(prev = %p.midashi, curr = %midashi_str, "discarding typing-in-progress pending without flush");
                            pending = None;
                        }
                    }
                    // 通常変換（ユーザーによる明示的な変換）が来た時だけ、直前の確定候補を flush する
                    flush_pending(&proxy, &mut pending).await;
                }

                let (response, hit) = lookup_with_fallback(&proxy, &request).await;
                let final_response = if is_found(&response) {
                    let cands = parse_candidates(&response);
                    if !cands.is_empty() {
                        // 候補を文脈データで並び替える。
                        // azooKey・yaskkserv2 どちらのヒットでも文脈スコアを適用する。
                        let ranked = {
                            let guard = proxy.predictor.lock().await;
                            guard.rank_candidates(&session_context, &midashi_str, &cands)
                        };

                        if let Some(top_cand) = ranked.first() {
                            // 補完クエリではない通常変換の場合のみ、確定候補として保留し、直前文脈を更新する
                            if !is_completion_refer {
                                let backend_name = match hit {
                                    BackendHit::Primary => proxy.primary.name.clone(),
                                    BackendHit::Fallback => proxy.fallback.name.clone(),
                                    BackendHit::None => "none".to_string(),
                                };
                                pending = Some(PendingObservation {
                                    backend: backend_name,
                                    midashi: midashi_str.clone(),
                                    context: session_context.clone(),
                                    top_candidate: top_cand.clone(),
                                });

                                // 直前の文脈として漢字圏の文字を含む単語のみ保持する。
                                if contains_kanji(top_cand) {
                                    session_context.clear();
                                    session_context.push(top_cand.clone());
                                }
                            }
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
            Request::Completion(ref prefix_bytes) => {
                let prefix_str = decode_midashi(prefix_bytes);
                let (response, completions) = completion_with_aggregation(&proxy, &request, &session_context).await;
                let now = Instant::now();
                last_completion_time = Some(now);

                // 5秒以上経過した古いプレフィックスを削除し、最新を追加
                recent_completion_prefixes.retain(|(_, t)| now.duration_since(*t) < Duration::from_secs(5));
                if !prefix_str.is_empty() {
                    recent_completion_prefixes.push((prefix_str, now));
                }

                // 5秒以上経過した古い補完候補を削除し、最新を追加
                recent_completions.retain(|_, t| now.duration_since(*t) < Duration::from_secs(5));
                for cand in completions {
                    let clean = cand.split(';').next().unwrap_or(&cand).trim().to_string();
                    if !clean.is_empty() {
                        recent_completions.insert(clean, now);
                    }
                    recent_completions.insert(cand, now);
                }
                writer.write_all(&response).await?;
            }
        }
        writer.flush().await?;
    }

    // クライアント切断時（EOF等）にも残っている確定候補を flush して確実に保存する。
    flush_pending(&proxy, &mut pending).await;

    Ok(())
}

/// 保留中の確定候補を学習データに反映し、ファイルへ保存する。
async fn flush_pending(proxy: &Proxy, pending: &mut Option<PendingObservation>) {
    if let Some(obs) = pending.take() {
        let mut guard = proxy.predictor.lock().await;
        guard.observe(&obs.context, &obs.midashi, &obs.top_candidate);
        if let Err(e) = guard.save() {
            warn!(error = %e, "failed to save frequency data after observe");
        }
        info!(
            backend = %obs.backend,
            midashi = %obs.midashi,
            top_cand = %obs.top_candidate,
            context = ?obs.context,
            "flushed pending observation"
        );
    }
}

/// 次のリクエスト到着まで保留する学習データ（skk-bayesian.el の pending 機構に相当）
#[derive(Debug)]
struct PendingObservation {
    backend: String,
    midashi: String,
    context: Vec<String>,
    top_candidate: String,
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

async fn completion_with_aggregation(proxy: &Proxy, request: &Request, context: &[String]) -> (Vec<u8>, Vec<String>) {
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
        return (b"4\n".to_vec(), Vec::new());
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

    (format_candidates_response(&ranked), ranked)
}

/// 文字列にCJK漢字（U+4E00–U+9FFF など）が含まれるか判定する。
/// session_context には漢字を含む単語のみ積む（ひらがな・英数字のみは除外）。
fn contains_kanji(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'   // CJK統合漢字
            | '\u{3400}'..='\u{4DBF}' // CJK統合漢字拡張A
            | '\u{F900}'..='\u{FAFF}' // CJK互換漢字
        )
    })
}

/// SKKの送りあり見出し（例: "きr", "おくr", "たべr" 等）か判定する。
/// 送りあり見出しは平仮名等の非ASCII文字の末尾に送りブロック用のアルファベット英字（小文字等）が付く。
/// ローマ字見出し（例: "hontou", "fuku"）のように直前も英字の場合は送りありとはみなさない。
fn is_okuri_midashi(midashi: &str) -> bool {
    let mut chars = midashi.chars().rev();
    match (chars.next(), chars.next()) {
        (Some(last), Some(prev)) => last.is_ascii_alphabetic() && !prev.is_ascii(),
        _ => false,
    }
}

