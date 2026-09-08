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

    // rank_candidates に渡すセッション文脈（直前に変換・確定した漢字単語）。
    // 手動 seed ファイルへの書き込み（自動学習）は行わず、メモリ内セッションでのみ追跡する。
    let mut session_context: Vec<String> = Vec::new();
    let mut last_context_time: Option<Instant> = None;

    // 前回の通常変換で返却した保留中の文脈候補: (見出し語, 漢字単語, 時刻)
    // 候補選択中（同一見出し語の再Lookup）は確定とみなさず、
    // 次の「異なる通常変換見出し語」が入力された時点で直前単語が真に確定されたとみなして昇格させる。
    let mut pending_context: Option<(String, String, Instant)> = None;

    // macSKK の補完（Completion）クエリやプレビューによる文脈汚染を防止するためのトラッキング
    let mut recent_completions: HashMap<String, Instant> = HashMap::new();
    let mut last_completion: Option<(String, Instant)> = None;
    let mut last_lookup_time: Option<Instant> = None;

    // macSKK の入力開始時（1文字目）プレビューLookup検出用: (見出し語, 時刻)
    // macSKK はキー入力開始時（1文字目）に、完全一致補完候補を取得するため自動的に
    // 1文字 Lookup（例: "う", "か", "て"）を送信してくる（UserDict.swift:98）。
    // この 1回目の 1文字 Lookup に候補を返してしまうと、macSKK がそれを補完候補としてセットし、
    // 0.5秒後にキー入力があった瞬間に「雨」「蚊」「手」などが勝手に確定（addFixedText）されてしまう。
    // そのため 1回目の 1文字 Lookup には直ちに 4\n を返して補完候補展開を抑止する。
    // ユーザーが Space を押して 1文字漢字（「手」「木」等）を通常変換した場合は同一見出し語で
    // 再度 Lookup が届くため、通常変換としてバックエンドへ照会し、1文字漢字の変換を可能にする。
    let mut last_single_char_preview: Option<(String, Instant)> = None;

    loop {
        line.clear();

        let n = reader.read_until(b'\n', &mut line).await?;

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
                let now = Instant::now();
                let midashi_str = decode_midashi(midashi);

                // 前回の文脈更新から60秒以上経過している場合は、古い文脈を安全に消去する
                if let Some(t) = last_context_time
                    && now.duration_since(t) > Duration::from_secs(60) {
                        session_context.clear();
                        last_context_time = None;
                        pending_context = None;
                    }

                // 有効期限（3秒）の切れた古い補完情報を掃除
                recent_completions.retain(|_, t| now.duration_since(*t) < Duration::from_secs(3));

                let is_okuri = is_okuri_midashi(&midashi_str);

                // 直近 2 秒以内の最新 Completion prefix と完全に一致するか判定
                // （一致する場合は、ユーザーが補完候補ではなくその prefix のまま確定・変換しようとしている通常変換。
                // ただし、4 と 1 の間隔が 20ms 未満の場合は macSKK による補完候補選択の自動バースト送信なので通常変換とはみなさない）
                let is_same_as_prefix = last_completion
                    .as_ref()
                    .map(|(prefix, time)| {
                        let elapsed = now.duration_since(*time);
                        elapsed >= Duration::from_millis(20)
                            && elapsed < Duration::from_millis(2000)
                            && &midashi_str == prefix
                    })
                    .unwrap_or(false);

                // --- macSKK の補完クエリ（裏で自動送信される展開 Lookup）の除外判定 ---
                // 1. 直近 3 秒以内に返した補完候補一覧に含まれているか
                let is_in_recent_completions = !is_same_as_prefix
                    && !is_okuri
                    && recent_completions.contains_key(&midashi_str);

                // 2. 直近の最新 Completion prefix より長い単語か（ローカル辞書由来の補完候補プレビュー）
                let is_prefix_completion = !is_okuri
                    && last_completion
                        .as_ref()
                        .map(|(prefix, time)| {
                            now.duration_since(*time) < Duration::from_millis(2000)
                                && !prefix.is_empty()
                                && midashi_str.starts_with(prefix)
                                && midashi_str != *prefix
                        })
                        .unwrap_or(false);

                // 3. 補完直後の極短時間（20ms 未満）バースト、または連続バースト
                let is_rapid_burst = !is_okuri
                    && last_completion
                        .as_ref()
                        .map(|(_, time)| now.duration_since(*time) < Duration::from_millis(20))
                        .unwrap_or(false)
                    || (!is_same_as_prefix
                        && !is_okuri
                        && last_completion
                            .as_ref()
                            .map(|(_, time)| now.duration_since(*time) < Duration::from_millis(2000))
                            .unwrap_or(false)
                        && last_lookup_time
                            .map(|t| now.duration_since(t) < Duration::from_millis(200))
                            .unwrap_or(false));

                let is_completion_refer = is_in_recent_completions
                    || is_prefix_completion
                    || is_rapid_burst;

                last_lookup_time = Some(now);

                // macSKK が裏で補完候補を展開するために自動送信してくる Lookup に対しては、
                // 即座に 4（未検出）を返して補完展開を抑止する。
                // これにより macSKK 側で「展開候補（completion = .candidates）」が生成されず、
                // 補完確定時間制限（約0.3秒）による勝手な確定（addFixedText）を 100% 物理的に防止する。
                if is_completion_refer {
                    debug!(
                        midashi = %midashi_str,
                        in_recent = is_in_recent_completions,
                        is_prefix = is_prefix_completion,
                        is_burst = is_rapid_burst,
                        "suppressed completion refer lookup to prevent unintended commit"
                    );
                    writer.write_all(b"4\n").await?;
                    writer.flush().await?;
                    continue;
                }

                // macSKK の入力開始時（1文字目）プレビューLookup の抑止:
                // 非ASCII 1文字の見出し語（例: "う", "か", "て" 等）は、キー入力開始時に macSKK が
                // 補完候補取得のため自動送信してくる（UserDict.swift:98）。
                // この 1回目の Lookup で候補を返すと 0.5秒後のキー入力で誤爆確定（「雨」「手」「蚊」等）するため、
                // 1回目は直ちに 4\n を返して補完誤爆確定を完全に防止する。
                // ユーザーが Space を押して 1文字漢字（「手」「木」等）を通常変換した場合は同一見出し語で
                // 再度 Lookup が届くため、通常変換としてバックエンドへ照会し、1文字漢字の変換を可能にする。
                let is_single_char = !is_okuri
                    && midashi_str.chars().count() == 1
                    && midashi_str.chars().next().map(|c| !c.is_ascii()).unwrap_or(false);

                if is_single_char {
                    let is_consecutive = pending_context
                        .as_ref()
                        .map(|(prev, _, _)| prev == &midashi_str)
                        .unwrap_or(false);

                    let is_second_press = last_single_char_preview
                        .as_ref()
                        .map(|(prev, time)| prev == &midashi_str && now.duration_since(*time) < Duration::from_secs(3))
                        .unwrap_or(false);

                    if !is_consecutive && !is_second_press {
                        debug!(
                            midashi = %midashi_str,
                            "suppressed 1-char preview lookup with 4 to prevent unintended commit"
                        );
                        last_single_char_preview = Some((midashi_str.clone(), now));
                        writer.write_all(b"4\n").await?;
                        writer.flush().await?;
                        continue;
                    } else {
                        debug!(
                            midashi = %midashi_str,
                            is_consecutive,
                            is_second_press,
                            "allowed 1-char lookup as explicit conversion"
                        );
                        last_single_char_preview = Some((midashi_str.clone(), now));
                    }
                } else {
                    last_single_char_preview = None;
                }

                // 遅延確定方式（Deferred Context Commit）:
                // 補完クエリでない通常変換で、かつ前回の見出し語と異なる新しい単語が始まった場合、
                // 前回の単語が確定されたとみなして session_context に昇格させる。
                if let Some((prev_midashi, prev_word, _)) = pending_context.take() {
                    if prev_midashi != midashi_str {
                        session_context.clear();
                        session_context.push(prev_word);
                        last_context_time = Some(now);
                        debug!(
                            context = ?session_context,
                            new_midashi = %midashi_str,
                            "deferred context committed"
                        );
                    } else {
                        // 同一見出し語での連続Lookup（次候補送り中）なので保留を継続
                        pending_context = Some((prev_midashi, prev_word, now));
                    }
                }

                let (response, _hit) = lookup_with_fallback(&proxy, &request).await;
                let final_response = if is_found(&response) {
                    let cands = parse_candidates(&response);
                    if !cands.is_empty() {
                        // seed ファイルの頻度・文脈データに基づいて候補を並び替える。
                        let ranked = {
                            let guard = proxy.predictor.lock().await;
                            guard.rank_candidates(&session_context, &midashi_str, &cands)
                        };

                        // 補完クエリやプレビューでない通常変換の場合、
                        // 返却第1候補が漢字を含んでいれば pending_context に保留する（まだ確定とはみなさない）
                        if !is_completion_refer
                            && let Some(top_cand) = ranked.first() {
                                let clean = clean_candidate(top_cand);
                                if contains_kanji(clean) {
                                    pending_context = Some((midashi_str.clone(), clean.to_string(), now));
                                } else {
                                    pending_context = None;
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
                let now = Instant::now();
                last_single_char_preview = None;
                if let Some(t) = last_context_time
                    && now.duration_since(t) > Duration::from_secs(60) {
                        session_context.clear();
                        last_context_time = None;
                    }

                let prefix_str = decode_midashi(prefix_bytes);
                let (response, completions) = completion_with_aggregation(&proxy, &request, &session_context).await;
                last_completion = Some((prefix_str.clone(), now));

                // 3秒以上経過した古い補完候補を削除し、最新を追加
                recent_completions.retain(|_, t| now.duration_since(*t) < Duration::from_secs(3));
                for cand in completions {
                    let clean = clean_candidate(&cand);
                    if !clean.is_empty() && clean != prefix_str {
                        recent_completions.insert(clean.to_string(), now);
                    }
                    if cand != prefix_str {
                        recent_completions.insert(cand, now);
                    }
                }

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

async fn completion_with_aggregation(proxy: &Proxy, request: &Request, context: &[String]) -> (Vec<u8>, Vec<String>) {
    let midashi_str = match request {
        Request::Completion(midashi) => decode_midashi(midashi),
        _ => String::new(),
    };

    // 補完はタイピング中に毎文字飛ぶため、高速応答（150ms以内）が最優先。
    // まずローカルの azooKey (5~20ms) を照会し、候補があれば即座に返却してタイピング詰まりを防ぐ。
    let primary_timeout = Duration::from_millis(150);
    let primary_res = proxy.primary.query_with_timeout(request, primary_timeout).await;

    let primary_cands = match primary_res {
        Ok(resp) if is_found(&resp) => parse_candidates(&resp),
        _ => Vec::new(),
    };

    let fallback_cands = if primary_cands.is_empty() {
        let fallback_timeout = Duration::from_millis(150);
        match proxy.fallback.query_with_timeout(request, fallback_timeout).await {
            Ok(resp) if is_found(&resp) => parse_candidates(&resp),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };

    if primary_cands.is_empty() && fallback_cands.is_empty() {
        return (b"4\n".to_vec(), Vec::new());
    }

    let merged = if fallback_cands.is_empty() {
        primary_cands
    } else {
        merge_candidates(&primary_cands, &fallback_cands)
    };

    let ranked = {
        let guard = proxy.predictor.lock().await;
        guard.rank_candidates(context, &midashi_str, &merged)
    };

    debug!(
        primary_count = merged.len(),
        ranked_count = ranked.len(),
        "completion candidates ranked"
    );

    (format_candidates_response(&ranked), ranked)
}

/// 候補文字列から注釈（`;` 以降）を除去した本体文字列を返す。
fn clean_candidate(cand: &str) -> &str {
    cand.split(';').next().unwrap_or(cand).trim()
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
/// 送りあり見出しは平仮名等の非ASCII文字の末尾に送りブロック用のアルファベット英字が付く。
fn is_okuri_midashi(midashi: &str) -> bool {
    let mut chars = midashi.chars().rev();
    match (chars.next(), chars.next()) {
        (Some(last), Some(prev)) => last.is_ascii_alphabetic() && !prev.is_ascii(),
        _ => false,
    }
}
