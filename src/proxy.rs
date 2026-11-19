use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::Instant;
use tracing::{debug, error, info, trace, warn};

use crate::backend::Backend;
use crate::config::BackendOrder;
use crate::encoding::{
    contains_okuri_bracket_block, decode_euc_or_utf8, extract_first_candidate,
    format_candidates_response_str, has_multiple_candidates, parse_candidates_borrowed,
};
use crate::frequency::SharedPredictor;
use crate::protocol::{Request, is_found, parse_request};

const MAX_LINE_BYTES: u64 = 8192;
/// 同時接続クライアントの上限数（ローカル DoS / FD 枯渇対策）
const MAX_CONCURRENT_CLIENTS: usize = 64;
/// クライアント接続の無通信アイドルタイムアウト
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// 1 回の Lookup / Completion 全体に許容する上限（macSKK の 1.0 秒制限に安全マージンを持たせる）
const OVERALL_LOOKUP_DEADLINE: Duration = Duration::from_millis(850);
/// 同期システム辞書 FFI 照会に許すデッドライン手前の安全マージン
const FFI_DEADLINE_MARGIN: Duration = Duration::from_millis(80);
/// 1 回のコールド FFI 照会を開始する前に必要な最小残り時間（これ未満なら同期 FFI をスキップ）
const MIN_COLD_FFI_REMAINING: Duration = Duration::from_millis(25);
/// 初期化済みの通常リクエスト内で同期コールド FFI 照会に費やす時間の上限
const MAX_COLD_FFI_BUDGET: Duration = Duration::from_millis(450);
/// 先頭バックエンド応答がこの時間を超えたら、裏で2番目バックエンドのヘッジ照会を並列起動する
const PRIMARY_HEDGE_DELAY: Duration = Duration::from_millis(200);

pub struct Proxy {
    pub listen: String,
    pub azookey: Backend,
    pub yaskkserv2: Backend,
    pub backend_order: BackendOrder,
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
                trace!(
                    discarded = ?pending.word,
                    prev_midashi = ?pending.midashi,
                    new_midashi = ?midashi,
                    "discarded pending context (input extension)"
                );
            } else if now.duration_since(pending.time) <= CONTEXT_TTL {
                // 見出し語が変わったため、前回の単語が確定したと判定して昇格
                self.session_context = Some((pending.word, pending.time));
                trace!(
                    context = ?self.session_context,
                    new_midashi = ?midashi,
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
        info!(listen = %self.listen, backend_order = ?self.backend_order, "ruskkserv listening");
        info!(
            yaskkserv2_addr = %self.yaskkserv2.addr,
            azookey_addr = %self.azookey.addr,
            "backends configured"
        );

        let proxy = Arc::new(self);
        let shared_context = Arc::new(std::sync::Mutex::new(SharedContextState::default()));

        // バックエンドおよびシステム辞書をバックグラウンドで並行ウォームアップ
        // （macOS の App Nap 解除・補完モデル展開と CoreServices 辞書インデックス初期化・接続プール確立を同時進行させる）
        if proxy.annotation_priority {
            const REPRESENTATIVE_WORDS: &[&str] = &["日本", "時間", "切る", "着る"];
            crate::system_dict::enqueue_prewarm(
                REPRESENTATIVE_WORDS.iter().map(|&w| (w.to_string(), None)),
            );
        }
        {
            let warmup_proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                let req_lookup = Request::Lookup("あ".as_bytes());
                let _ = warmup_proxy
                    .yaskkserv2
                    .query_with_timeout(&req_lookup, Duration::from_millis(500))
                    .await;
                debug!("yaskkserv2 connection warmup complete");
            });
        }
        {
            let warmup_proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                // 1. azoo-key-skkserv 通常変換（Lookup）モデルのウォームアップ
                let req_lookup = Request::Lookup("あ".as_bytes());
                if warmup_proxy
                    .azookey
                    .query_with_timeout(&req_lookup, Duration::from_secs(3))
                    .await
                    .is_ok()
                {
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
                            .azookey
                            .query_with_timeout(&req_comp, Duration::from_millis(500))
                            .await;
                    }
                    debug!("azookey backend lookup and completion warmup complete");
                } else {
                    debug!("azookey backend unavailable at startup warmup, skipping completion warmup");
                }
            });
        }

        let conn_limit = Arc::new(Semaphore::new(MAX_CONCURRENT_CLIENTS));

        loop {
            let Ok(permit) = Arc::clone(&conn_limit).acquire_owned().await else {
                break;
            };
            match listener.accept().await {
                Ok((stream, peer)) => {
                    debug!(%peer, "client connected");
                    let _ = stream.set_nodelay(true);
                    let proxy = Arc::clone(&proxy);
                    let shared_context = Arc::clone(&shared_context);
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(err) = handle_client(proxy, shared_context, stream).await {
                            debug!(%peer, error = %err, "client session ended");
                        }
                    });
                }
                Err(err) => {
                    drop(permit);
                    error!(error = %err, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        Ok(())
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

        // 8KB を超える改行なし不正データによるメモリ枯渇 (OOM DoS) を防ぐため、take とアイドルタイムアウトを設定
        let mut take_reader = (&mut reader).take(MAX_LINE_BYTES + 1);
        let n = match tokio::time::timeout(
            CLIENT_IDLE_TIMEOUT,
            take_reader.read_until(b'\n', &mut line),
        )
        .await
        {
            Ok(res) => res?,
            Err(_) => {
                debug!(?peer, "client idle timeout, closing connection");
                break;
            }
        };

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

        trace!(?peer, raw_line = ?decode_euc_or_utf8(&line), "received request from client");

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
                if midashi.is_empty() {
                    writer.write_all(b"4\n").await?;
                    conn_last_response_time = Some(Instant::now());
                    continue;
                }

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
                let overall_deadline = OVERALL_LOOKUP_DEADLINE;
                let ctx_key = ctx_ref.first().map(|a| a.as_ref());

                // 【ゼロレイテンシ・キャッシュ参照】
                // 過去に同一文脈・同一見出し語で変換済みの場合は、キー確保・TCP 往復・辞書判定・整列を完全バイパスし即答
                if let Some(cached) = proxy.lookup_cache.get_by_ref(ctx_key, &midashi_str) {
                    trace!(midashi = ?midashi_str, "lookup cache hit");
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
                    continue;
                }

                // 接頭語（例: "さい>", "ちょう>", "ご>"）および接尾語（例: ">せい", ">ひ", ">てき", ">わたs"）の専用処理。
                // SKK の仕様上、接頭語・接尾語は `>` を含む見出し語として辞書に登録されており、候補文字列には `>` を含めない。
                // yaskkserv2 を最優先で照会し、ミス時のみ `>` を除いた読みで azoo-key-skkserv を照会する。
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
                    let resp_arc: Arc<[u8]> = Arc::from(resp_to_send);
                    trace!(?peer, kind = ?midashi_kind, resp = ?String::from_utf8_lossy(&resp_arc), "sending affix response to client");
                    if is_found(&resp_arc) {
                        proxy.lookup_cache.insert(
                            crate::cache::LookupKey::new(ctx_key, &midashi_str),
                            crate::cache::CachedLookup {
                                response: Arc::clone(&resp_arc),
                                top_candidate: None,
                            },
                        );
                    }
                    writer.write_all(&resp_arc).await?;
                    conn_last_response_time = Some(Instant::now());
                    continue;
                }

                let outcome = match proxy.backend_order {
                    BackendOrder::Yaskkserv2First => {
                        handle_lookup_yaskkserv2_first(
                            &proxy,
                            &request,
                            &midashi_str,
                            ctx_ref,
                            lookup_start,
                            overall_deadline,
                        )
                        .await
                    }
                    BackendOrder::AzookeyFirst => {
                        handle_lookup_azookey_first(
                            &proxy,
                            &request,
                            &midashi_str,
                            ctx_ref,
                            lookup_start,
                            overall_deadline,
                            is_completion_burst,
                        )
                        .await
                    }
                };

                // 文脈連動が有効で、補完連射でなく、返却第1候補が漢字を含む場合のみ次回のための直前単語として保留する。
                if proxy.context_ranking
                    && !is_completion_burst
                    && let Some(ref top) = outcome.top_candidate
                {
                    let clean = crate::frequency::clean_candidate(top);
                    if contains_kanji(clean) {
                        let mut ctx = shared_context.lock().unwrap_or_else(|e| e.into_inner());
                        ctx.on_lookup_result(&midashi_str, Arc::from(clean), now);
                    }
                }

                let resp_arc: Arc<[u8]> = Arc::from(outcome.response);
                if !outcome.skip_cache && is_found(&resp_arc) {
                    proxy.lookup_cache.insert(
                        crate::cache::LookupKey::new(ctx_key, &midashi_str),
                        crate::cache::CachedLookup {
                            response: Arc::clone(&resp_arc),
                            top_candidate: outcome.top_candidate.map(Arc::from),
                        },
                    );
                }

                trace!(?peer, resp = ?String::from_utf8_lossy(&resp_arc), "sending response to client");
                writer.write_all(&resp_arc).await?;
                conn_last_response_time = Some(Instant::now());
            }
            Request::Completion(prefix) => {
                if prefix.is_empty() {
                    writer.write_all(b"4\n").await?;
                    conn_last_response_time = Some(Instant::now());
                    continue;
                }
                let start = Instant::now();
                let overall_deadline = OVERALL_LOOKUP_DEADLINE;
                let prefix_str = decode_euc_or_utf8(prefix);
                let is_affix_comp = !matches!(
                    crate::okuri::classify_midashi(&prefix_str),
                    crate::okuri::MidashiKind::Normal(_)
                );
                let (response, hit) = if is_affix_comp {
                    // 接尾語補完（例: `4>せ `）などは SKK 辞書を持つ yaskkserv2 へ直接照会
                    let (resp, ok) = lookup_single_backend(
                        &proxy.yaskkserv2,
                        &request,
                        start,
                        overall_deadline,
                    )
                    .await;
                    (
                        resp,
                        if ok {
                            BackendHit::Yaskkserv2
                        } else {
                            BackendHit::None
                        },
                    )
                } else {
                    match proxy.backend_order {
                        BackendOrder::Yaskkserv2First => {
                            lookup_yaskkserv2_first(&proxy, &request, start, overall_deadline).await
                        }
                        BackendOrder::AzookeyFirst => {
                            lookup_azookey_first(&proxy, &request, start, overall_deadline).await
                        }
                    }
                };
                trace!(?peer, ?hit, resp = ?String::from_utf8_lossy(&response), "completion upstream response");
                writer.write_all(&response).await?;
                conn_last_response_time = Some(Instant::now());
            }
        }
    }

    Ok(())
}

struct LookupOutcome {
    response: Vec<u8>,
    top_candidate: Option<String>,
    skip_cache: bool,
}

/// 候補スライスに対し、必要に応じてシステム辞書／頻度／文脈共起による並び替えとキャッシュ前登録を行い、レスポンスを生成する。
fn rank_candidates_to_response(
    proxy: &Proxy,
    ctx_ref: &[Arc<str>],
    lookup_start: Instant,
    overall_deadline: Duration,
    rank_midashi: &str,
    cands: &[&str],
    first_cold_skipped: bool,
) -> LookupOutcome {
    if cands.is_empty() {
        return LookupOutcome {
            response: b"4\n".to_vec(),
            top_candidate: None,
            skip_cache: false,
        };
    }

    let should_rank = cands.len() > 1
        && (proxy.annotation_priority || {
            let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
            guard.should_rank(ctx_ref, rank_midashi)
        });

    if should_rank {
        let ffi_deadline = compute_ffi_deadline(
            lookup_start.elapsed(),
            overall_deadline,
            FFI_DEADLINE_MARGIN,
        );
        let (ranked, had_skipped) = rank_with_budget_and_prewarm(
            &proxy.predictor,
            ctx_ref,
            rank_midashi,
            cands,
            proxy.annotation_priority,
            ffi_deadline,
        );
        let top_candidate = ranked.first().map(|&t| t.to_string());
        LookupOutcome {
            response: format_candidates_response_str(&ranked),
            top_candidate,
            skip_cache: had_skipped || first_cold_skipped,
        }
    } else {
        if first_cold_skipped
            && proxy.annotation_priority
            && let Some(&first) = cands.first()
        {
            crate::system_dict::enqueue_prewarm([(
                first.to_string(),
                Some(rank_midashi.to_string()),
            )]);
        }
        let top_candidate = cands.first().map(|&t| t.to_string());
        LookupOutcome {
            response: format_candidates_response_str(cands),
            top_candidate,
            skip_cache: first_cold_skipped,
        }
    }
}

/// 生レスポンスバイト列から、角括弧ブロック展開または通常パースを行い、ランキング・整形結果を返す。
#[allow(clippy::too_many_arguments)]
fn finalize_raw_response(
    proxy: &Proxy,
    ctx_ref: &[Arc<str>],
    lookup_start: Instant,
    overall_deadline: Duration,
    rank_midashi: &str,
    response: Vec<u8>,
    first_cold_skipped: bool,
) -> LookupOutcome {
    if !is_found(&response) {
        return LookupOutcome {
            response,
            top_candidate: None,
            skip_cache: false,
        };
    }

    if contains_okuri_bracket_block(&response) {
        let unpacked = crate::encoding::parse_candidates_unpacked_borrowed(&response);
        if unpacked.is_empty() {
            return LookupOutcome {
                response,
                top_candidate: None,
                skip_cache: false,
            };
        }
        return rank_candidates_to_response(
            proxy,
            ctx_ref,
            lookup_start,
            overall_deadline,
            rank_midashi,
            &unpacked,
            first_cold_skipped,
        );
    }

    let should_rank = has_multiple_candidates(&response)
        && (proxy.annotation_priority || {
            let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
            guard.should_rank(ctx_ref, rank_midashi)
        });

    if should_rank {
        let cands = parse_candidates_borrowed(&response);
        if !cands.is_empty() {
            return rank_candidates_to_response(
                proxy,
                ctx_ref,
                lookup_start,
                overall_deadline,
                rank_midashi,
                &cands,
                first_cold_skipped,
            );
        }
    }

    if first_cold_skipped
        && proxy.annotation_priority
        && let Some(first) = extract_first_candidate(&response)
    {
        crate::system_dict::enqueue_prewarm([(
            first.to_string(),
            Some(rank_midashi.to_string()),
        )]);
    }
    let top_candidate = extract_first_candidate(&response).map(|s| s.to_string());
    LookupOutcome {
        response,
        top_candidate,
        skip_cache: first_cold_skipped,
    }
}

/// `azoo-key-skkserv` に対して送りあり見出しの活用バリエーションを並行照会し、
/// 抽出された重複なしの語幹候補リストを返す。語幹が 1 件も得られなければ `None`。
async fn expand_azookey_okuri(
    azookey: &Backend,
    midashi_str: &str,
    lookup_start: Instant,
    overall_deadline: Duration,
) -> Option<Vec<String>> {
    let variations = crate::okuri::expand_okuri_variations(midashi_str);
    if variations.is_empty() {
        return None;
    }

    let remaining_overall = overall_deadline.saturating_sub(lookup_start.elapsed());
    let max_total_okuri = Duration::from_millis(600).min(remaining_overall);
    let okuri_timeout = azookey
        .timeout
        .min(Duration::from_millis(300))
        .min(max_total_okuri);
    if okuri_timeout.is_zero() {
        return None;
    }

    // azoo-key-skkserv のコネクションプールを活用し、全活用形を完全並行照会
    let mut handles = Vec::with_capacity(variations.len());
    for (idx, var) in variations.into_iter().enumerate() {
        let backend = azookey.clone();
        handles.push(tokio::spawn(async move {
            let okuri_req = Request::Lookup(var.query_midashi.as_bytes());
            let res = backend.query_with_timeout(&okuri_req, okuri_timeout).await;
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

    let mut all_stem_cands: Vec<String> = Vec::new();
    for (_, res, var) in query_results {
        if let Ok(okuri_resp) = res
            && is_found(&okuri_resp)
        {
            let raw_cands = parse_candidates_borrowed(&okuri_resp);
            let stem_cands = crate::okuri::extract_stem_candidates_borrowed(
                &raw_cands,
                &var.okuri_suffix,
                &var.query_midashi,
            );

            trace!(
                midashi = ?midashi_str,
                query = ?var.query_midashi,
                suffix = ?var.okuri_suffix,
                resp = ?String::from_utf8_lossy(&okuri_resp),
                stems = ?stem_cands,
                "okuri variation checked"
            );

            for stem in stem_cands {
                if !all_stem_cands.iter().any(|existing| existing == stem) {
                    all_stem_cands.push(stem.to_string());
                }
            }
        }
    }

    if all_stem_cands.is_empty() {
        None
    } else {
        Some(all_stem_cands)
    }
}

/// `yaskkserv2` 優先モード時に `azoo-key-skkserv` へフォールバックするヘルパー。
/// 送りあり展開が有効かつ対象見出し語であれば活用バリエーション展開を試し、
/// それ以外または未ヒット時は通常の見出し語照会へフォールバックする。
async fn query_azookey_fallback(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    is_okuri_candidate: bool,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, Option<Vec<String>>, BackendHit) {
    if is_okuri_candidate
        && let Some(stems) =
            expand_azookey_okuri(&proxy.azookey, midashi_str, start, overall_deadline).await
    {
        let refs: Vec<&str> = stems.iter().map(|s| s.as_str()).collect();
        let resp = format_candidates_response_str(&refs);
        return (resp, Some(stems), BackendHit::Azookey);
    }

    let remaining = overall_deadline.saturating_sub(start.elapsed());
    let azoo_timeout = proxy.azookey.timeout.min(remaining);
    if azoo_timeout.is_zero() {
        return (b"4\n".to_vec(), None, BackendHit::None);
    }

    match proxy.azookey.query_with_timeout(request, azoo_timeout).await {
        Ok(response) => {
            let hit = if is_found(&response) {
                debug!(
                    backend = %proxy.azookey.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "hit"
                );
                BackendHit::Azookey
            } else {
                debug!(
                    backend = %proxy.azookey.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "miss"
                );
                BackendHit::None
            };
            (response, None, hit)
        }
        Err(err) => {
            warn!(
                backend = %proxy.azookey.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "azookey fallback failed"
            );
            (b"4\n".to_vec(), None, BackendHit::None)
        }
    }
}

/// `yaskkserv2` 優先モードで Lookup を処理し、200ms 超過時の `azoo-key-skkserv` ヘッジ並列起動も行う。
async fn lookup_yaskkserv2_then_azookey(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    is_okuri_candidate: bool,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, Option<Vec<String>>, BackendHit) {
    let remaining_overall = overall_deadline.saturating_sub(start.elapsed());
    if remaining_overall.is_zero() {
        debug!("overall deadline reached before lookup, returning not found immediately");
        return (b"4\n".to_vec(), None, BackendHit::None);
    }

    let yaskk_timeout = proxy.yaskkserv2.timeout.min(remaining_overall);
    if !yaskk_timeout.is_zero() {
        let mut yaskk_fut =
            std::pin::pin!(proxy.yaskkserv2.query_with_timeout(request, yaskk_timeout));

        let hedge_fired = if remaining_overall > PRIMARY_HEDGE_DELAY
            && yaskk_timeout > PRIMARY_HEDGE_DELAY
        {
            tokio::select! {
                res = &mut yaskk_fut => {
                    if let Some(resp) = interpret_backend_result(&proxy.yaskkserv2, start, res) {
                        return (resp, None, BackendHit::Yaskkserv2);
                    }
                    false
                }
                _ = tokio::time::sleep(PRIMARY_HEDGE_DELAY) => true,
            }
        } else {
            if let Some(resp) =
                interpret_backend_result(&proxy.yaskkserv2, start, yaskk_fut.as_mut().await)
            {
                return (resp, None, BackendHit::Yaskkserv2);
            }
            false
        };

        if hedge_fired {
            let remaining_azoo = overall_deadline.saturating_sub(start.elapsed());
            if !remaining_azoo.is_zero() {
                debug!(
                    elapsed_ms = start.elapsed().as_millis(),
                    "yaskkserv2 slow, starting hedged azookey query"
                );
                let mut azoo_fut = std::pin::pin!(query_azookey_fallback(
                    proxy,
                    request,
                    midashi_str,
                    is_okuri_candidate,
                    start,
                    overall_deadline,
                ));
                let mut azoo_opt = None;

                let yaskk_res = loop {
                    if azoo_opt.is_none() {
                        tokio::select! {
                            res = &mut yaskk_fut => break res,
                            fb = &mut azoo_fut => azoo_opt = Some(fb),
                        }
                    } else {
                        break yaskk_fut.as_mut().await;
                    }
                };

                if let Some(resp) = interpret_backend_result(&proxy.yaskkserv2, start, yaskk_res) {
                    return (resp, None, BackendHit::Yaskkserv2);
                }
                return match azoo_opt {
                    Some(res) => res,
                    None => azoo_fut.await,
                };
            }
            if let Some(resp) =
                interpret_backend_result(&proxy.yaskkserv2, start, yaskk_fut.as_mut().await)
            {
                return (resp, None, BackendHit::Yaskkserv2);
            }
        }
    }

    query_azookey_fallback(
        proxy,
        request,
        midashi_str,
        is_okuri_candidate,
        start,
        overall_deadline,
    )
    .await
}

/// `BackendOrder::Yaskkserv2First` の通常変換パス：
/// 1. `yaskkserv2` を優先照会（200ms 超過時は `azoo-key-skkserv` をヘッジ並列起動）
/// 2. `yaskkserv2` ヒット時は必要に応じて角括弧ブロック展開＋文脈／頻度／システム辞書優先整列
/// 3. `yaskkserv2` ミス／失敗時は `azoo-key-skkserv`（送りあり展開含む）へフォールバック
async fn handle_lookup_yaskkserv2_first(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    ctx_ref: &[Arc<str>],
    lookup_start: Instant,
    overall_deadline: Duration,
) -> LookupOutcome {
    let is_okuri_candidate =
        proxy.okuri_expansion && !crate::okuri::expand_okuri_variations(midashi_str).is_empty();

    let (response, okuri_stems_opt, hit) = lookup_yaskkserv2_then_azookey(
        proxy,
        request,
        midashi_str,
        is_okuri_candidate,
        lookup_start,
        overall_deadline,
    )
    .await;

    if !is_found(&response) {
        return LookupOutcome {
            response,
            top_candidate: None,
            skip_cache: false,
        };
    }

    if hit == BackendHit::Azookey
        && let Some(stems) = okuri_stems_opt
    {
        let stem_refs: Vec<&str> = stems.iter().map(|s| s.as_str()).collect();
        let norm_midashi = crate::okuri::normalize_okuri_midashi_key(midashi_str);
        return rank_candidates_to_response(
            proxy,
            ctx_ref,
            lookup_start,
            overall_deadline,
            &norm_midashi,
            &stem_refs,
            false,
        );
    }

    finalize_raw_response(
        proxy,
        ctx_ref,
        lookup_start,
        overall_deadline,
        midashi_str,
        response,
        false,
    )
}

/// `BackendOrder::AzookeyFirst`（従来順・ロールバックモード）の通常変換パス
async fn handle_lookup_azookey_first(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    ctx_ref: &[Arc<str>],
    lookup_start: Instant,
    overall_deadline: Duration,
    is_completion_burst: bool,
) -> LookupOutcome {
    // 送りあり見出し（例: "かk" -> "かく", "交ぜGk" -> "交ぜがき"）の活用復元試行
    if proxy.okuri_expansion
        && !crate::okuri::expand_okuri_variations(midashi_str).is_empty()
    {
        let remaining_overall = overall_deadline.saturating_sub(lookup_start.elapsed());
        let fb_timeout = proxy
            .yaskkserv2
            .timeout
            .min(Duration::from_millis(200))
            .min(remaining_overall);
        let fb_handle = if !fb_timeout.is_zero() {
            let yaskk = proxy.yaskkserv2.clone();
            let req_midashi = midashi_str.to_string();
            Some(tokio::spawn(async move {
                let req = Request::Lookup(req_midashi.as_bytes());
                yaskk.query_with_timeout(&req, fb_timeout).await.ok()
            }))
        } else {
            None
        };

        if let Some(stems) =
            expand_azookey_okuri(&proxy.azookey, midashi_str, lookup_start, overall_deadline).await
        {
            let mut merged_cands: Vec<&str> = stems.iter().map(|s| s.as_str()).collect();

            // 元の見出し語で並列照会した yaskkserv2 の全候補（異体字・専門用語・注釈付き）を末尾に重複排除マージ
            let fallback_resp_opt = if let Some(handle) = fb_handle {
                handle.await.ok().flatten()
            } else {
                None
            };

            if let Some(ref fallback_resp) = fallback_resp_opt
                && is_found(fallback_resp)
            {
                let fb_cands =
                    crate::encoding::parse_candidates_unpacked_borrowed(fallback_resp);
                append_unique_by_clean(&mut merged_cands, fb_cands);
            }

            let norm_midashi = crate::okuri::normalize_okuri_midashi_key(midashi_str);
            return rank_candidates_to_response(
                proxy,
                ctx_ref,
                lookup_start,
                overall_deadline,
                &norm_midashi,
                &merged_cands,
                false,
            );
        } else if let Some(handle) = fb_handle {
            // 送り展開で語幹が得られなかった場合は通常パスへ委ねるため並列 yaskkserv2 をキャンセル
            handle.abort();
        }
    }

    // 送りなし見出し、または送り復元が無効／失敗した場合は azooKey 先・yaskkserv2 フォールバック照会
    let (response, hit) =
        lookup_azookey_first(proxy, request, lookup_start, overall_deadline).await;
    if !is_found(&response) {
        return LookupOutcome {
            response,
            top_candidate: None,
            skip_cache: false,
        };
    }

    let has_okuri_bracket = contains_okuri_bracket_block(&response);
    let mut fallback_resp_buf: Option<Vec<u8>> = None;
    let mut cands_buf: Option<Vec<&str>> = None;
    let mut first_cold_skipped = false;

    // azoo-key-skkserv ヒット時でも、以下のいずれかに該当する場合は
    // yaskkserv2 の候補も照会・マージして欠落を防ぐ:
    // 1. frequencies に登録された最高スコア候補が azooKey 返却候補に存在しない場合（例: "みぎや" -> "→"）
    // 2. 送りなし見出しに対して azooKey の第1候補が辞書未掲載の送り仮名断片だった場合（例: "がそ" -> "貸そ", "げそ" -> "解そ"）
    // 3. 補完連射でない通常変換で、送りなし見出しに対する azooKey の第1候補がシステム国語辞典未掲載の合成語だった場合
    if hit == BackendHit::Azookey && !has_okuri_bracket {
        let mut first_is_unverified_fragment = false;
        let mut first_not_in_dict = false;
        if proxy.annotation_priority
            && crate::okuri::parse_okuri_midashi_extended(midashi_str).is_none()
            && let Some(first) = extract_first_candidate(&response)
        {
            let first_check_deadline = compute_ffi_deadline(
                lookup_start.elapsed(),
                overall_deadline,
                FFI_DEADLINE_MARGIN + Duration::from_millis(30),
            );
            let (in_dict, cold_skipped) =
                crate::system_dict::is_annotated_or_in_dict_parsed_budgeted(
                    first,
                    None,
                    true,
                    Some(first_check_deadline),
                );
            if cold_skipped {
                // 後段のランキング／prewarm 処理へ合流させ、二重起動と FFI 競合を防止する
                first_cold_skipped = true;
            } else if !in_dict {
                first_is_unverified_fragment = crate::frequency::is_unverified_okuri_fragment(
                    crate::frequency::clean_candidate(first),
                );
                if !is_completion_burst {
                    first_not_in_dict = true;
                }
            }
        }

        let missing_top_freq = {
            let guard = proxy.predictor.read().unwrap_or_else(|e| e.into_inner());
            if guard.should_rank(&[] as &[&str], midashi_str) {
                let parsed = cands_buf.get_or_insert_with(|| parse_candidates_borrowed(&response));
                guard.is_top_freq_candidate_missing(midashi_str, parsed)
            } else {
                false
            }
        };

        if first_is_unverified_fragment || missing_top_freq || first_not_in_dict {
            let remaining_fb = overall_deadline.saturating_sub(lookup_start.elapsed());
            let cap_ms = if first_is_unverified_fragment || missing_top_freq {
                Duration::from_millis(200)
            } else {
                // ローカルバイナリ辞書（<2ms）のヒット回収を目的とし、Google Suggest タイムアウト待ちを抑制
                Duration::from_millis(40)
            };
            let fb_timeout = proxy.yaskkserv2.timeout.min(cap_ms).min(remaining_fb);
            if !fb_timeout.is_zero()
                && let Ok(fallback_resp) = proxy
                    .yaskkserv2
                    .query_with_timeout(request, fb_timeout)
                    .await
                && is_found(&fallback_resp)
            {
                fallback_resp_buf = Some(fallback_resp);
            }
        }
    }

    if let Some(ref fb_buf) = fallback_resp_buf {
        let mut merged = cands_buf
            .take()
            .unwrap_or_else(|| parse_candidates_borrowed(&response));
        let fb_cands = crate::encoding::parse_candidates_unpacked_borrowed(fb_buf);
        append_unique_by_clean(&mut merged, fb_cands);
        return rank_candidates_to_response(
            proxy,
            ctx_ref,
            lookup_start,
            overall_deadline,
            midashi_str,
            &merged,
            first_cold_skipped,
        );
    }

    finalize_raw_response(
        proxy,
        ctx_ref,
        lookup_start,
        overall_deadline,
        midashi_str,
        response,
        first_cold_skipped,
    )
}

/// `dst` に既にある候補と `clean_candidate` ベースで重複しないものだけを `src` から末尾へ追加する。
fn append_unique_by_clean<'a>(
    dst: &mut Vec<&'a str>,
    src: impl IntoIterator<Item = &'a str>,
) {
    let mut seen: HashSet<&'a str> = HashSet::with_capacity(dst.len() + 8);
    for &existing in dst.iter() {
        let clean = crate::frequency::clean_candidate(existing);
        if !clean.is_empty() {
            seen.insert(clean);
        }
    }
    for cand in src {
        let clean = crate::frequency::clean_candidate(cand);
        if !clean.is_empty() && seen.insert(clean) {
            dst.push(cand);
        }
    }
}

/// どの実効バックエンドが応答したかを示す列挙型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendHit {
    Azookey,
    Yaskkserv2,
    None,
}

/// 汎用順序付き照会の結果位置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderedHit {
    First,
    Second,
    None,
}

/// 接頭語（`midashi>`）および接尾語（`>midashi`）の変換照会処理。
/// SKK 辞書を持つ `yaskkserv2` を最優先で照会し、ミス時のみ `>` を除去した内部読みで
/// `azoo-key-skkserv` を照会したうえで候補内の余分な `>` をサニタイズする。
async fn lookup_affix(
    proxy: &Proxy,
    request: &Request<'_>,
    midashi_str: &str,
    inner_reading: &str,
    start: Instant,
    overall_deadline: Duration,
) -> Vec<u8> {
    let (mut response, found) =
        lookup_single_backend(&proxy.yaskkserv2, request, start, overall_deadline).await;

    if found && is_found(&response) {
        if contains_okuri_bracket_block(&response) {
            let unpacked = crate::encoding::parse_candidates_unpacked_borrowed(&response);
            response = format_candidates_response_str(&unpacked);
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

    // yaskkserv2 に接頭語・接尾語エントリがない場合のみ、`>` を除いた読みで azoo-key-skkserv を照会
    let remaining = overall_deadline.saturating_sub(start.elapsed());
    let azookey_timeout = proxy.azookey.timeout.min(remaining);
    if !azookey_timeout.is_zero() {
        let stripped_req = Request::Lookup(inner_reading.as_bytes());
        if let Ok(primary_resp) = proxy
            .azookey
            .query_with_timeout(&stripped_req, azookey_timeout)
            .await
            && is_found(&primary_resp)
        {
            let raw_cands = parse_candidates_borrowed(&primary_resp);
            let mut sanitized: Vec<&str> = Vec::with_capacity(raw_cands.len());
            let mut seen: HashSet<&str> = HashSet::with_capacity(raw_cands.len());
            for cand in raw_cands {
                let cleaned = cand.trim_matches(['>', '?']);
                let word = crate::frequency::clean_candidate(cleaned);
                if !word.is_empty() && seen.insert(word) {
                    sanitized.push(cleaned);
                }
            }
            if !sanitized.is_empty() {
                return format_candidates_response_str(&sanitized);
            }
        }
    }

    response
}

/// 指定バックエンド 1 系統のみを照会するヘルパー（接頭語・接尾語の変換および補完用）。
async fn lookup_single_backend(
    backend: &Backend,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, bool) {
    let elapsed = start.elapsed();
    let backend_timeout = backend
        .timeout
        .min(overall_deadline.saturating_sub(elapsed));
    if backend_timeout.is_zero() {
        return (b"4\n".to_vec(), false);
    }

    match backend.query_with_timeout(request, backend_timeout).await {
        Ok(response) => {
            let found = is_found(&response);
            (response, found)
        }
        Err(err) => {
            warn!(
                backend = %backend.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "backend failed on affix query"
            );
            (b"4\n".to_vec(), false)
        }
    }
}

#[inline]
fn compute_ffi_deadline(
    elapsed: Duration,
    overall_deadline: Duration,
    reserve: Duration,
) -> std::time::Instant {
    let remaining_for_ffi = overall_deadline
        .saturating_sub(elapsed)
        .saturating_sub(reserve);
    if remaining_for_ffi < MIN_COLD_FFI_REMAINING {
        // 新たにコールド FFI を開始する余裕がない場合は即時期限切れを返し、キャッシュ参照のみ許可する
        return std::time::Instant::now();
    }
    let budget = if crate::system_dict::is_ffi_warmed() {
        remaining_for_ffi.min(MAX_COLD_FFI_BUDGET)
    } else {
        // 初回プロセス起動直後の CoreServices 辞書ロード（~600ms）が完了するまではデッドラインいっぱいまで許容する
        remaining_for_ffi
    };
    std::time::Instant::now() + budget
}

fn rank_with_budget_and_prewarm<'a, S: AsRef<str>>(
    predictor: &SharedPredictor,
    context: &[S],
    midashi: &str,
    candidates: &[&'a str],
    annotation_priority: bool,
    ffi_deadline: std::time::Instant,
) -> (Vec<&'a str>, bool) {
    let (ranked, skipped_cold) = {
        let guard = predictor.read().unwrap_or_else(|e| e.into_inner());
        guard.rank_candidates_borrowed_with_deadline(
            context,
            midashi,
            candidates,
            annotation_priority,
            Some(ffi_deadline),
        )
    };
    let had_skipped = !skipped_cold.is_empty();
    if had_skipped && annotation_priority {
        let midashi_owned = midashi.to_string();
        crate::system_dict::enqueue_prewarm(
            skipped_cold
                .into_iter()
                .map(|cand| (cand.to_string(), Some(midashi_owned.clone()))),
        );
    }
    (ranked, had_skipped)
}

fn interpret_backend_result(
    backend: &Backend,
    start: Instant,
    res: Result<Vec<u8>, crate::backend::BackendError>,
) -> Option<Vec<u8>> {
    match res {
        Ok(response) if is_found(&response) => {
            debug!(
                backend = %backend.name,
                elapsed_ms = start.elapsed().as_millis(),
                "hit"
            );
            Some(response)
        }
        Ok(_) => {
            debug!(
                backend = %backend.name,
                elapsed_ms = start.elapsed().as_millis(),
                "miss, trying fallback"
            );
            None
        }
        Err(err) => {
            warn!(
                backend = %backend.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "first backend failed, trying fallback"
            );
            None
        }
    }
}

fn finalize_second_result(
    backend: &Backend,
    start: Instant,
    result: Result<Vec<u8>, crate::backend::BackendError>,
) -> (Vec<u8>, OrderedHit) {
    match result {
        Ok(response) => {
            let hit = if is_found(&response) {
                debug!(
                    backend = %backend.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "hit"
                );
                OrderedHit::Second
            } else {
                debug!(
                    backend = %backend.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    "miss"
                );
                OrderedHit::None
            };
            (response, hit)
        }
        Err(err) => {
            warn!(
                backend = %backend.name,
                error = %err,
                elapsed_ms = start.elapsed().as_millis(),
                "fallback backend failed"
            );
            (b"4\n".to_vec(), OrderedHit::None)
        }
    }
}

/// `first` バックエンドを先に照会し、ミス時に `second` バックエンドを照会する汎用ヘルパー。
/// `first` が 200ms を超えて遅延した場合は `second` へのヘッジ照会を並列起動し、
/// 全体デッドライン（850ms）内で初回変換失敗を防止する。
async fn lookup_ordered(
    first: &Backend,
    second: &Backend,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, OrderedHit) {
    let remaining_overall = overall_deadline.saturating_sub(start.elapsed());
    if remaining_overall.is_zero() {
        debug!("overall deadline reached before lookup, returning not found immediately");
        return (b"4\n".to_vec(), OrderedHit::None);
    }

    let first_timeout = first.timeout.min(remaining_overall);

    if !first_timeout.is_zero() {
        let mut first_fut = std::pin::pin!(first.query_with_timeout(request, first_timeout));

        let hedge_fired = if remaining_overall > PRIMARY_HEDGE_DELAY
            && first_timeout > PRIMARY_HEDGE_DELAY
        {
            tokio::select! {
                res = &mut first_fut => {
                    if let Some(resp) = interpret_backend_result(first, start, res) {
                        return (resp, OrderedHit::First);
                    }
                    false
                }
                _ = tokio::time::sleep(PRIMARY_HEDGE_DELAY) => true,
            }
        } else {
            if let Some(resp) = interpret_backend_result(first, start, first_fut.as_mut().await) {
                return (resp, OrderedHit::First);
            }
            false
        };

        if hedge_fired {
            let second_timeout = second
                .timeout
                .min(overall_deadline.saturating_sub(start.elapsed()));
            if !second_timeout.is_zero() {
                debug!(
                    backend = %first.name,
                    fallback = %second.name,
                    elapsed_ms = start.elapsed().as_millis(),
                    second_timeout_ms = second_timeout.as_millis(),
                    "first backend slow, starting hedged fallback query"
                );
                let mut second_fut =
                    std::pin::pin!(second.query_with_timeout(request, second_timeout));
                let mut second_opt: Option<Result<Vec<u8>, crate::backend::BackendError>> = None;

                let first_res = loop {
                    if second_opt.is_none() {
                        tokio::select! {
                            res = &mut first_fut => break res,
                            fb_res = &mut second_fut => second_opt = Some(fb_res),
                        }
                    } else {
                        break first_fut.as_mut().await;
                    }
                };

                if let Some(resp) = interpret_backend_result(first, start, first_res) {
                    return (resp, OrderedHit::First);
                }
                let second_res = match second_opt {
                    Some(res) => res,
                    None => second_fut.await,
                };
                return finalize_second_result(second, start, second_res);
            }
            if let Some(resp) = interpret_backend_result(first, start, first_fut.as_mut().await) {
                return (resp, OrderedHit::First);
            }
        }
    }

    let second_timeout = second
        .timeout
        .min(overall_deadline.saturating_sub(start.elapsed()));
    if second_timeout.is_zero() {
        debug!("overall deadline reached before fallback, returning not found immediately");
        return (b"4\n".to_vec(), OrderedHit::None);
    }

    let second_res = second.query_with_timeout(request, second_timeout).await;
    finalize_second_result(second, start, second_res)
}

async fn lookup_azookey_first(
    proxy: &Proxy,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, BackendHit) {
    let (resp, ordered_hit) = lookup_ordered(
        &proxy.azookey,
        &proxy.yaskkserv2,
        request,
        start,
        overall_deadline,
    )
    .await;
    let hit = match ordered_hit {
        OrderedHit::First => BackendHit::Azookey,
        OrderedHit::Second => BackendHit::Yaskkserv2,
        OrderedHit::None => BackendHit::None,
    };
    (resp, hit)
}

async fn lookup_yaskkserv2_first(
    proxy: &Proxy,
    request: &Request<'_>,
    start: Instant,
    overall_deadline: Duration,
) -> (Vec<u8>, BackendHit) {
    let (resp, ordered_hit) = lookup_ordered(
        &proxy.yaskkserv2,
        &proxy.azookey,
        request,
        start,
        overall_deadline,
    )
    .await;
    let hit = match ordered_hit {
        OrderedHit::First => BackendHit::Yaskkserv2,
        OrderedHit::Second => BackendHit::Azookey,
        OrderedHit::None => BackendHit::None,
    };
    (resp, hit)
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
