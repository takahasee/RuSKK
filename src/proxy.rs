use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::backend::Backend;
use crate::encoding::{
    contains_bracket_block, decode_euc_or_utf8, extract_first_candidate,
    format_candidates_response_str, has_multiple_candidates, parse_candidates_borrowed,
};
use crate::frequency::SharedPredictor;
use crate::protocol::{Request, is_found, parse_request};

const MAX_LINE_BYTES: u64 = 8192;

pub struct Proxy {
    pub listen: String,
    pub primary: Backend,
    pub fallback: Backend,
    pub predictor: SharedPredictor,
    pub lookup_cache: Arc<crate::cache::LookupCache>,
    pub okuri_expansion: bool,
    pub context_ranking: bool,
    pub annotation_priority: bool,
}

/// 文脈の有効期限（確定から次の変換開始までの最大間隔）
const CONTEXT_TTL: Duration = Duration::from_secs(60);

/// 確定待ち（保留中）の直前単語
#[derive(Debug, Clone)]
struct PendingContext {
    midashi: String,
    word: Arc<str>,
    time: Instant,
    /// 同一見出し語の Lookup を受信した回数。
    /// macSKK は ▽ 入力開始時に先頭 1 文字を機械的に Lookup し、Space 押下時に再度 Lookup するため、
    /// 2 回以上であれば「ユーザーが実際に変換した」とみなせる。
    hits: u32,
}

/// Apple Silicon (M1〜M4 / A18 Pro) の 128 バイトキャッシュラインに整合させ、
/// マルチコア並行アクセス時の偽共有（False Sharing）によるキャッシュバウンスをハードウェアレベルでゼロ化
#[derive(Debug, Default)]
#[repr(align(128))]
struct SharedContextState {
    session_context: Option<(Arc<str>, Instant)>,
    pending_context: Option<PendingContext>,
}

impl SharedContextState {
    /// 有効期限（CONTEXT_TTL）内の確定文脈を取得する。期限切れの場合はクリアする。
    fn current_context(&mut self, now: Instant) -> Option<Arc<str>> {
        if let Some((_, time)) = self.session_context
            && now.duration_since(time) > CONTEXT_TTL
        {
            self.session_context = None;
        }
        self.session_context.as_ref().map(|(w, _)| Arc::clone(w))
    }

    /// 新しい手動 Lookup の開始時に呼び出し、保留中の単語を確定（昇格）するか判定する。
    /// 戻り値は並び替えに使う文脈のスナップショット。
    fn on_lookup_start(&mut self, midashi: &str, now: Instant) -> Option<Arc<str>> {
        if let Some(mut pending) = self.pending_context.take() {
            if pending.midashi == midashi {
                // 同一見出し語での再 Lookup（Space による変換開始・次候補送り）なので保留を継続
                pending.hits = pending.hits.saturating_add(1);
                self.pending_context = Some(pending);
            } else if is_input_extension(midashi, &pending.midashi)
                && pending.hits
                    < if pending.midashi.chars().count() == 1 {
                        3
                    } else {
                        2
                    }
            {
                // 「き」→「きr」「に」→「にく」「さ」→「さい」のように入力が伸長しただけの場合は、
                // macSKK の ▽ 開始時・再入力時の機械的 Lookup であり未確定のため、昇格せずに破棄する。
                // 特に 1 文字見出し（「き」「さ」等）は「▽き (1) → Space確定 (2) → 次の▽き (3)」で
                // 3 回 Lookup されて初めて「1文字単語として確定後に次の同頭文字語を入力した」とみなせる。
                debug!(
                    discarded = %pending.word,
                    prev_midashi = %pending.midashi,
                    new_midashi = %midashi,
                    "discarded pending context (input extension)"
                );
            } else if now.duration_since(pending.time) <= CONTEXT_TTL {
                // 見出し語が変わったため、前回の単語が確定したと判定して昇格
                self.session_context = Some((pending.word, pending.time));
                debug!(
                    context = ?self.session_context,
                    new_midashi = %midashi,
                    "promoted pending context"
                );
            } else {
                self.session_context = None;
            }
        }
        self.current_context(now)
    }

    /// Lookup の応答後に呼び出し、返却第1候補を次回のための保留単語として記録する。
    fn on_lookup_result(&mut self, midashi: &str, word: Arc<str>, now: Instant) {
        match self.pending_context {
            Some(ref mut pending) if pending.midashi == midashi => {
                pending.word = word;
                pending.time = now;
            }
            _ => {
                self.pending_context = Some(PendingContext {
                    midashi: midashi.to_string(),
                    word,
                    time: now,
                    hits: 1,
                });
            }
        }
    }
}

/// `new` が `prev` を前方一致で伸長した入力か（例: "き" → "きr", "に" → "にく"）
fn is_input_extension(new: &str, prev: &str) -> bool {
    new.len() > prev.len() && new.starts_with(prev)
}

impl Proxy {
    pub async fn run(self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.listen).await?;
        info!(listen = %self.listen, "ruskkserv listening");
        info!(
            primary = %self.primary.name,
            primary_addr = %self.primary.addr,
            fallback = %self.fallback.name,
            fallback_addr = %self.fallback.addr,
            "backends configured"
        );

        let proxy = Arc::new(self);
        let shared_context = Arc::new(std::sync::Mutex::new(SharedContextState::default()));

        // Primary バックエンド（azooKey）およびシステム辞書をバックグラウンドでウォームアップ
        // （macOS の App Nap による初回スリープを解除し、通常変換・補完エンジンを先行初期化する）
        {
            let warmup_proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                // 1. 通常変換（Lookup）モデルのウォームアップ
                let req_lookup = Request::Lookup("あ".as_bytes());
                let _ = warmup_proxy
                    .primary
                    .query_with_timeout(&req_lookup, Duration::from_secs(3))
                    .await;

                // 2. 補完（Completion）エンジンのウォームアップ（五十音の代表文字を照会してトライ木等をメモリ展開）
                const WARMUP_CHARS: &[&[u8]] = &[
                    "あ".as_bytes(),
                    "か".as_bytes(),
                    "さ".as_bytes(),
                    "た".as_bytes(),
                    "な".as_bytes(),
                    "は".as_bytes(),
                    "ま".as_bytes(),
                    "や".as_bytes(),
                    "ら".as_bytes(),
                    "わ".as_bytes(),
                ];
                for &kana in WARMUP_CHARS {
                    let req_comp = Request::Completion(kana);
                    let _ = warmup_proxy
                        .primary
                        .query_with_timeout(&req_comp, Duration::from_millis(500))
                        .await;
                }
                tracing::debug!("primary backend lookup and completion warmup complete");

                // 3. システム辞書キャッシュのバックグラウンド先行ウォームアップ
                // （起動直後の初弾 Lookup における FFI キャッシュミス 160ms を完全解消）
                if warmup_proxy.annotation_priority {
                    let words_to_warmup: Vec<String> = {
                        let guard = warmup_proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                        guard.frequencies.keys().take(64).cloned().collect()
                    };
                    tokio::task::spawn_blocking(move || {
                        for word in words_to_warmup {
                            let _ = crate::system_dict::is_annotated_or_in_dict(&word, None);
                        }
                        tracing::debug!("system dictionary cache warmup complete");
                    });
                }
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
            warn!(
                ?peer,
                len = line.len(),
                "request line too long or missing newline"
            );
            break;
        }

        // Allow CR LF.
        if line.ends_with(b"\r\n") {
            line.truncate(line.len() - 2);
        } else if line.ends_with(b"\n") {
            line.truncate(line.len() - 1);
        }

        debug!(?peer, raw_line = %decode_euc_or_utf8(&line), "received request from client");

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
                const VERSION_RESP: &[u8] =
                    concat!("ruskkserv/", env!("CARGO_PKG_VERSION"), " ").as_bytes();
                writer.write_all(VERSION_RESP).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Host => {
                let host = format!("ruskkserv/{}: ", proxy.listen);
                writer.write_all(host.as_bytes()).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Lookup(midashi) => {
                let midashi_str = decode_euc_or_utf8(midashi);
                let midashi_kind = crate::okuri::classify_midashi(&midashi_str);
                let is_affix = !matches!(midashi_kind, crate::okuri::MidashiKind::Normal(_));
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
                    // 手動での変換において前回の単語が確定されたかを判定して session_context に昇格する。
                    if proxy.context_ranking && !is_completion_burst {
                        ctx.on_lookup_start(&midashi_str, now)
                    } else {
                        ctx.current_context(now)
                    }
                };

                let ctx_ref = if proxy.context_ranking && !is_affix {
                    if let Some(ref c) = session_ctx_snapshot {
                        std::slice::from_ref(c)
                    } else {
                        &[]
                    }
                } else {
                    &[]
                };

                let lookup_start = Instant::now();
                let overall_deadline = Duration::from_millis(850);
                let mut okuri_handled = false;
                let mut final_response = None;
                let mut top_candidate_for_context: Option<String> = None;

                let lookup_key = crate::cache::LookupKey::new(
                    ctx_ref.first().map(|a| a.as_ref()),
                    &midashi_str,
                );

                // 【ゼロレイテンシ・キャッシュ参照】
                // 過去に同一文脈・同一見出し語で変換済みの場合は、TCP 往復・辞書判定・整列を完全バイパスし即答
                if let Some(cached) = proxy.lookup_cache.get(&lookup_key) {
                    debug!(midashi = %midashi_str, "lookup cache hit");
                    if proxy.context_ranking
                        && !is_completion_burst
                        && let Some(ref top) = cached.top_candidate
                    {
                        let clean = crate::frequency::clean_candidate(top);
                        if contains_kanji(clean) {
                            let mut ctx =
                                shared_context.lock().unwrap_or_else(|e| e.into_inner());
                            ctx.on_lookup_result(&midashi_str, Arc::from(clean), now);
                        }
                    }
                    writer.write_all(&cached.response).await?;
                    conn_last_response_time = Some(Instant::now());
                    writer.flush().await?;
                    continue;
                }

                // 接頭語（例: "さい>", "ちょう>", "ご>"）および接尾語（例: ">せい", ">ひ", ">てき", ">わたs"）の専用処理。
                // SKK の仕様上、接頭語・接尾語は `>` を含む見出し語として辞書に登録されており、候補文字列には `>` を含めない。
                // Primary (azooKey) は SKK の `>` 接辞構文を解釈せず文字通りの記号 `>` を候補に付与してしまうため、
                // 接頭語・接尾語は本来の SKK 辞書サーバーである Fallback (yaskkserv2) を最優先で照会する。
                if let crate::okuri::MidashiKind::Prefix(inner)
                | crate::okuri::MidashiKind::Suffix(inner) = midashi_kind
                {
                    let resp_to_send = lookup_affix(
                        &proxy,
                        &request,
                        &midashi_str,
                        inner,
                        lookup_start,
                        overall_deadline,
                    )
                    .await;
                    debug!(?peer, kind = ?midashi_kind, resp = ?String::from_utf8_lossy(&resp_to_send), "sending affix response to client");
                    if is_found(&resp_to_send) {
                        proxy.lookup_cache.insert(
                            lookup_key,
                            crate::cache::CachedLookup {
                                response: resp_to_send.clone(),
                                top_candidate: None,
                            },
                        );
                    }
                    writer.write_all(&resp_to_send).await?;
                    conn_last_response_time = Some(Instant::now());
                    writer.flush().await?;
                    continue;
                }

                // 送りあり見出し（例: "かk" -> "かく", "交ぜGk" -> "交ぜがき", "まぜがk" -> "まぜがき", "かきおこs" -> "かきおこす"）の活用復元試行
                if proxy.okuri_expansion {
                    let variations = crate::okuri::expand_okuri_variations(&midashi_str);
                    let remaining_overall = overall_deadline.saturating_sub(lookup_start.elapsed());
                    let max_total_okuri = Duration::from_millis(600).min(remaining_overall);
                    let okuri_timeout = proxy
                        .primary
                        .timeout
                        .min(Duration::from_millis(300))
                        .min(max_total_okuri);

                    // Primary バックエンドのコネクションプールを活用し、全活用形を完全並行照会
                    let mut handles = Vec::with_capacity(variations.len());
                    for (idx, var) in variations.into_iter().enumerate() {
                        let primary = proxy.primary.clone();
                        handles.push(tokio::spawn(async move {
                            let okuri_req = Request::Lookup(var.query_midashi.as_bytes());
                            let res = primary.query_with_timeout(&okuri_req, okuri_timeout).await;
                            (idx, res, var)
                        }));
                    }

                    let mut query_results = Vec::with_capacity(handles.len());
                    for handle in handles {
                        if let Ok(res) = handle.await {
                            query_results.push(res);
                        }
                    }
                    // 活用の優先順位（order_suffixes_for_stem）を保持するため、元のインデックス順に整列
                    query_results.sort_by_key(|(idx, _, _)| *idx);

                    let mut responses = Vec::with_capacity(query_results.len());
                    for (_, res, var) in query_results {
                        if let Ok(okuri_resp) = res && is_found(&okuri_resp) {
                            responses.push((okuri_resp, var));
                        }
                    }

                    let mut all_stem_cands = Vec::new();
                    for (resp, var) in &responses {
                        let raw_cands = parse_candidates_borrowed(resp);
                        let stem_cands = crate::okuri::extract_stem_candidates_borrowed(
                            &raw_cands,
                            &var.okuri_suffix,
                            &var.query_midashi,
                        );

                        debug!(
                            midashi = %midashi_str,
                            query = %var.query_midashi,
                            suffix = %var.okuri_suffix,
                            resp = ?String::from_utf8_lossy(resp),
                            stems = ?stem_cands,
                            "okuri variation checked"
                        );

                        for stem in stem_cands {
                            if !all_stem_cands.contains(&stem) {
                                 all_stem_cands.push(stem);
                            }
                        }
                    }

                    if !all_stem_cands.is_empty() {
                        let mut merged_cands: Vec<&str> = all_stem_cands;

                        // 元の見出し語（midashi_str）で Fallback (yaskkserv2 / 巨大SKK辞書) を照会し、
                        // 辞書が本来持っている全候補（異体字・専門用語・注釈付き）を末尾に重複排除してマージする。
                        // これにより azooKey の賢い促音・活用語幹が最優先となりつつ、
                        // 辞書の全候補も漏れなく提供され、変換候補数が少なくなる現象を根本的に解消する。
                        let remaining_fb = overall_deadline.saturating_sub(lookup_start.elapsed());
                        let fb_timeout = proxy.fallback.timeout.min(Duration::from_millis(200)).min(remaining_fb);
                        let fallback_resp_opt = if !fb_timeout.is_zero() {
                            proxy
                                .fallback
                                .query_with_timeout(&Request::Lookup(midashi_str.as_bytes()), fb_timeout)
                                .await
                                .ok()
                        } else {
                            None
                        };

                        if let Some(ref fallback_resp) = fallback_resp_opt
                            && is_found(fallback_resp)
                        {
                            let fb_cands =
                                crate::encoding::parse_candidates_unpacked_borrowed(fallback_resp);
                            for cand in fb_cands {
                                let clean = crate::frequency::clean_candidate(cand);
                                if !clean.is_empty()
                                    && !merged_cands
                                        .iter()
                                        .any(|&c| crate::frequency::clean_candidate(c) == clean)
                                {
                                    merged_cands.push(cand);
                                }
                            }
                        }

                        let norm_midashi = crate::okuri::normalize_okuri_midashi_key(&midashi_str);
                        let ranked = {
                            let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                            guard.rank_candidates_borrowed(
                                ctx_ref,
                                &norm_midashi,
                                &merged_cands,
                                proxy.annotation_priority,
                            )
                        };
                        if let Some(&top) = ranked.first() {
                            top_candidate_for_context = Some(top.to_string());
                        }
                        final_response = Some(format_candidates_response_str(&ranked));
                        okuri_handled = true;
                    }
                }

                // 送りなし見出し、または送り復元が無効／失敗した場合は従来通りの照会
                let resp_to_send = if okuri_handled && let Some(resp) = final_response {
                    resp
                } else {
                    let (mut response, hit) =
                        lookup_with_fallback(&proxy, &request, lookup_start, overall_deadline).await;
                    if is_found(&response) {
                        // Primary (azooKey) ヒット時でも、以下のいずれかに該当する場合は
                        // Fallback (yaskkserv2) の候補も照会・マージして欠落を防ぐ:
                        // 1. frequencies に登録された最高スコア候補が Primary 返却候補に存在しない場合（例: "みぎや" -> "→"）
                        // 2. 送りなし見出しに対して Primary の第1候補が辞書未掲載の送り仮名断片だった場合（例: "がそ" -> "貸そ", "げそ" -> "解そ"）
                        // 3. 補完連射でない通常変換で、送りなし見出しに対する Primary の第1候補がシステム国語辞典未掲載の合成語だった場合
                        //    （例: "せつびご" -> "設備後"［名詞+接尾語の誤結合］）
                        if hit == BackendHit::Primary && !contains_bracket_block(&response) {
                            let remaining_fb =
                                overall_deadline.saturating_sub(lookup_start.elapsed());
                            let mut first_is_unverified_fragment = false;
                            let mut first_not_in_dict = false;
                            if proxy.annotation_priority
                                && crate::okuri::parse_okuri_midashi_extended(&midashi_str)
                                    .is_none()
                                && let Some(first) = extract_first_candidate(&response)
                            {
                                let not_in_dict = !crate::system_dict::is_annotated_or_in_dict(
                                    first,
                                    Some(&midashi_str),
                                );
                                if not_in_dict {
                                    first_is_unverified_fragment =
                                        crate::frequency::is_unverified_okuri_fragment(
                                            crate::frequency::clean_candidate(first),
                                        );
                                    if !is_completion_burst {
                                        first_not_in_dict = true;
                                    }
                                }
                            }

                            let missing_top_freq = {
                                let guard =
                                    proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                                // frequencies にエントリがある見出し語のみスライス借用で照合
                                guard.should_rank(&[] as &[&str], &midashi_str) && {
                                    let cands = parse_candidates_borrowed(&response);
                                    guard.is_top_freq_candidate_missing(&midashi_str, &cands)
                                }
                            };

                            if first_is_unverified_fragment || missing_top_freq || first_not_in_dict
                            {
                                let cap_ms = if first_is_unverified_fragment || missing_top_freq {
                                    Duration::from_millis(200)
                                } else {
                                    // ローカルバイナリ辞書（<2ms）のヒット回収を目的とし、Google Suggest タイムアウト待ちを抑制
                                    Duration::from_millis(40)
                                };
                                let fb_timeout =
                                    proxy.fallback.timeout.min(cap_ms).min(remaining_fb);
                                if !fb_timeout.is_zero()
                                    && let Ok(fallback_resp) = proxy
                                        .fallback
                                        .query_with_timeout(&request, fb_timeout)
                                        .await
                                    && is_found(&fallback_resp)
                                {
                                    let mut merged_cands: Vec<&str> =
                                        parse_candidates_borrowed(&response);
                                    let fb_cands =
                                        crate::encoding::parse_candidates_unpacked_borrowed(&fallback_resp);
                                    for cand in fb_cands {
                                        let clean = crate::frequency::clean_candidate(cand);
                                        if !clean.is_empty()
                                            && !merged_cands.iter().any(|&c| {
                                                crate::frequency::clean_candidate(c) == clean
                                            })
                                        {
                                            merged_cands.push(cand);
                                        }
                                    }
                                    response = format_candidates_response_str(&merged_cands);
                                }
                            }
                        }

                        // 【ファストパス】単一候補（候補数 <= 1）の場合は並び替えの余地がないため完全ゼロコピー直結。
                        // また、角括弧ブロック（例: `/[っ/思/]/`）を含むレスポンスは単純ソートで構造が破壊されるため並び替えをスキップ。
                        // 複数候補（角括弧なし）の場合、annotation_priority 有効時または頻度・文脈ルール存在時のみ借用パースして整列する。
                        let should_rank = has_multiple_candidates(&response)
                            && !contains_bracket_block(&response)
                            && (proxy.annotation_priority || {
                                let guard =
                                    proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                                guard.should_rank(ctx_ref, &midashi_str)
                            });

                        if should_rank {
                            // ゼロコピーで借用スライスパースして並び替え
                            let cands = parse_candidates_borrowed(&response);
                            if !cands.is_empty() {
                                let ranked = {
                                    let guard =
                                        proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                                    guard.rank_candidates_borrowed(
                                        ctx_ref,
                                        &midashi_str,
                                        &cands,
                                        proxy.annotation_priority,
                                    )
                                };
                                if let Some(&top) = ranked.first() {
                                    top_candidate_for_context = Some(top.to_string());
                                }
                                format_candidates_response_str(&ranked)
                            } else {
                                response
                            }
                        } else {
                            // 単一候補・角括弧ブロック含有・並び替え対象外（annotation_priority 無効かつルールなし）:
                            // パース・アロケーション完全スキップ（ゼロコピー直結）
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
                        ctx.on_lookup_result(&midashi_str, Arc::from(clean), now);
                    }
                }

                if is_found(&resp_to_send) {
                    proxy.lookup_cache.insert(
                        lookup_key,
                        crate::cache::CachedLookup {
                            response: resp_to_send.clone(),
                            top_candidate: top_candidate_for_context.clone(),
                        },
                    );
                }

                debug!(?peer, resp = ?String::from_utf8_lossy(&resp_to_send), "sending response to client");
                writer.write_all(&resp_to_send).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Completion(prefix) => {
                let start = Instant::now();
                let overall_deadline = Duration::from_millis(850);
                let prefix_str = decode_euc_or_utf8(prefix);
                let is_affix_comp = !matches!(
                    crate::okuri::classify_midashi(&prefix_str),
                    crate::okuri::MidashiKind::Normal(_)
                );
                let (response, hit) = if is_affix_comp {
                    // 接尾語補完（例: `4>せ `）などは SKK 辞書を持つ Fallback (yaskkserv2) へ直接照会
                    lookup_fallback_only(&proxy, &request, start, overall_deadline).await
                } else {
                    lookup_with_fallback(&proxy, &request, start, overall_deadline).await
                };
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

/// 接頭語（`midashi>`）および接尾語（`>midashi`）の変換照会処理。
/// SKK 辞書を持つ Fallback (`yaskkserv2`) を最優先で照会し、ミス時のみ `>` を除去した内部読みで
/// Primary (`azooKey`) を照会したうえで候補内の余分な `>` をサニタイズする。
async fn lookup_affix(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    inner_reading: &str,
    start: Instant,
    overall_deadline: Duration,
) -> Vec<u8> {
    let (mut response, hit) = lookup_fallback_only(proxy, request, start, overall_deadline).await;

    if hit == BackendHit::Fallback && is_found(&response) {
        if contains_bracket_block(&response) {
            let unpacked = crate::encoding::parse_candidates_unpacked(&response);
            let refs: Vec<&str> = unpacked.iter().map(|s| s.as_str()).collect();
            response = format_candidates_response_str(&refs);
        }
        if has_multiple_candidates(&response) {
            let should_rank = {
                let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                guard.should_rank(&[] as &[&str], midashi_str)
            };
            if should_rank {
                let cands = parse_candidates_borrowed(&response);
                if !cands.is_empty() {
                    let ranked = {
                        let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
                        guard.rank_candidates_borrowed(&[] as &[&str], midashi_str, &cands, false)
                    };
                    response = format_candidates_response_str(&ranked);
                }
            }
        }
        return response;
    }

    // Fallback (yaskkserv2) に接頭語・接尾語エントリがない場合のみ、`>` を除いた読みで Primary を照会
    let remaining = overall_deadline.saturating_sub(start.elapsed());
    let primary_timeout = proxy.primary.timeout.min(remaining);
    if !primary_timeout.is_zero() {
        let stripped_req = Request::Lookup(inner_reading.as_bytes());
        if let Ok(primary_resp) = proxy
            .primary
            .query_with_timeout(&stripped_req, primary_timeout)
            .await
            && is_found(&primary_resp)
        {
            let raw_cands = parse_candidates_borrowed(&primary_resp);
            let mut sanitized: Vec<String> = Vec::with_capacity(raw_cands.len());
            for cand in raw_cands {
                let cleaned = cand.trim_matches(['>', '?']);
                let word = crate::frequency::clean_candidate(cleaned);
                if !word.is_empty()
                    && !sanitized
                        .iter()
                        .any(|c| crate::frequency::clean_candidate(c) == word)
                {
                    sanitized.push(cleaned.to_string());
                }
            }
            if !sanitized.is_empty() {
                let refs: Vec<&str> = sanitized.iter().map(|s| s.as_str()).collect();
                return format_candidates_response_str(&refs);
            }
        }
    }

    response
}

/// Fallback（yaskkserv2）のみを照会するヘルパー（接頭語・接尾語の変換および補完用）。
async fn lookup_fallback_only(
    proxy: &Proxy,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, BackendHit) {
    let elapsed = start.elapsed();
    let fallback_timeout = proxy
        .fallback
        .timeout
        .min(overall_deadline.saturating_sub(elapsed));
    if fallback_timeout.is_zero() {
        return (b"4\n".to_vec(), BackendHit::None);
    }

    match proxy
        .fallback
        .query_with_timeout(request, fallback_timeout)
        .await
    {
        Ok(response) => {
            let hit = if is_found(&response) {
                BackendHit::Fallback
            } else {
                BackendHit::None
            };
            (response, hit)
        }
        Err(err) => {
            warn!(
                backend = %proxy.fallback.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "fallback failed on affix query"
            );
            (b"4\n".to_vec(), BackendHit::None)
        }
    }
}

/// Primary（azooKey）を先に照会し、ミス時に Fallback（yaskkserv2）を照会する。
/// 全体デッドライン（850ms）から動的タイムアウトを計算し、macSKK の 1.0秒制限を超えないようにする。
/// 戻り値として (レスポンスバイト列, ヒットしたバックエンド) を返す。
async fn lookup_with_fallback(
    proxy: &Proxy,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, BackendHit) {
    let elapsed = start.elapsed();
    let remaining_overall = overall_deadline.saturating_sub(elapsed);
    if remaining_overall.is_zero() {
        debug!("overall deadline reached before lookup, returning not found immediately");
        return (b"4\n".to_vec(), BackendHit::None);
    }

    let primary_timeout = proxy.primary.timeout.min(remaining_overall);

    if !primary_timeout.is_zero() {
        match proxy
            .primary
            .query_with_timeout(request, primary_timeout)
            .await
        {
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
    }

    let elapsed = start.elapsed();
    let fallback_timeout = proxy
        .fallback
        .timeout
        .min(overall_deadline.saturating_sub(elapsed));
    if fallback_timeout.is_zero() {
        debug!("overall deadline reached before fallback, returning not found immediately");
        return (b"4\n".to_vec(), BackendHit::None);
    }

    match proxy
        .fallback
        .query_with_timeout(request, fallback_timeout)
        .await
    {
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
    s.chars().any(crate::okuri::is_kanji)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 回の Lookup（変換結果の記録まで）をシミュレートし、並び替えに使われた文脈を返す
    fn lookup(state: &mut SharedContextState, midashi: &str, top: &str) -> Option<String> {
        let now = Instant::now();
        let ctx = state.on_lookup_start(midashi, now);
        state.on_lookup_result(midashi, Arc::from(top), now);
        ctx.map(|c| c.to_string())
    }

    #[test]
    fn mechanical_first_char_lookup_does_not_override_context() {
        // 実際の macSKK のシーケンス: 「にく」Space → 確定 → 「KiR」入力
        // （▽き の時点で「き」が機械的に Lookup され、続いて「きr」が Lookup される）
        let mut s = SharedContextState::default();
        lookup(&mut s, "に", "二"); // ▽に 開始時の機械的 Lookup
        lookup(&mut s, "にく", "肉"); // Space
        lookup(&mut s, "き", "木"); // ▽き 開始時の機械的 Lookup
        let ctx = lookup(&mut s, "きr", "着");
        assert_eq!(ctx.as_deref(), Some("肉"));
    }

    #[test]
    fn confirmed_single_kana_word_is_kept_even_if_next_extends_it() {
        // 「き」Space →「木」確定 →「KiR」入力（き の Lookup が 3 回: 機械的・Space・機械的）
        let mut s = SharedContextState::default();
        lookup(&mut s, "き", "木");
        lookup(&mut s, "き", "木");
        lookup(&mut s, "き", "木");
        let ctx = lookup(&mut s, "きr", "着");
        assert_eq!(ctx.as_deref(), Some("木"));
    }

    #[test]
    fn unrelated_midashi_promotes_pending() {
        let mut s = SharedContextState::default();
        lookup(&mut s, "ふく", "服");
        let ctx = lookup(&mut s, "きr", "着");
        assert_eq!(ctx.as_deref(), Some("服"));
    }

    #[test]
    fn expired_session_context_is_cleared_even_after_input_extension() {
        let mut s = SharedContextState::default();
        let t0 = Instant::now();
        // 1. 「にく」を変換
        s.on_lookup_start("にく", t0);
        s.on_lookup_result("にく", Arc::from("肉"), t0);

        // 2. 10秒後に ▽き（先頭1文字の機械的 Lookup）-> 「肉」が session_context に昇格
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(s.on_lookup_start("き", t1).as_deref(), Some("肉"));
        s.on_lookup_result("き", Arc::from("木"), t1);

        // 3. 最初の確定から61秒後（TTL 60秒超過）に「きr」を入力
        // 「き」->「きr」は入力伸長として破棄され、かつ「肉」もTTL超過で消去されること
        let t2 = t0 + Duration::from_secs(61);
        assert_eq!(s.on_lookup_start("きr", t2), None);
    }

    #[test]
    fn repeated_single_char_mechanical_lookup_does_not_promote_context() {
        // ▽さ 開始 → キャンセル → 再び ▽さい 入力のように先頭 1 文字「さ」の機械的 Lookup が 2 回届いた後
        // 「さい」が Lookup された場合でも、「差」が確定文脈として誤昇格しないこと
        let mut s = SharedContextState::default();
        lookup(&mut s, "さ", "差");
        lookup(&mut s, "さ", "差");
        let ctx = lookup(&mut s, "さい", "際");
        assert_eq!(ctx, None);
    }
}
