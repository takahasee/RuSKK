//! macOS システム辞書（大辞林等）および SKK 注釈（annotation）に基づく単語判定モジュール。
//! CoreServices の DictionaryServices API (DCSCopyTextDefinition) を呼び出し、
//! 国語辞典に項目が存在する単語、および SKK 注釈付き単語を判定して高速にキャッシュする。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, RwLock, TryLockError};

use crate::frequency::clean_candidate;
use crate::okuri::{OkuriMidashi, parse_okuri_midashi_extended};

/// CoreServices DictionaryServices の初回ロード（最大 ~600ms）が完了したかを記録するフラグ。
static FFI_WARMED: AtomicBool = AtomicBool::new(false);

/// フォアグラウンド変換がシステム辞書 FFI を待機・実行中であることを示すカウンタ。
/// バックグラウンドの事前ウォームアップ・再加熱がこの値を見て一時譲歩する。
static FOREGROUND_FFI_WAITERS: AtomicUsize = AtomicUsize::new(0);

struct ForegroundWaiterGuard;

impl ForegroundWaiterGuard {
    #[inline]
    fn new() -> Self {
        FOREGROUND_FFI_WAITERS.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for ForegroundWaiterGuard {
    #[inline]
    fn drop(&mut self) {
        FOREGROUND_FFI_WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 初回のシステム辞書 FFI 初期化が完了済みか返す。
#[inline]
pub fn is_ffi_warmed() -> bool {
    FFI_WARMED.load(Ordering::Acquire)
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CFRange {
        pub location: isize,
        pub length: isize,
    }

    pub type CFStringRef = *const c_void;
    pub type CFDictionaryRef = *const c_void;

    pub const K_CF_STRING_ENCODING_UTF8: u32 = 0x08000100;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        pub fn DCSCopyTextDefinition(
            dictionary: CFDictionaryRef,
            record_string: CFStringRef,
            text_range: CFRange,
        ) -> CFStringRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        pub fn CFStringCreateWithBytes(
            alloc: *const c_void,
            bytes: *const u8,
            num_bytes: isize,
            encoding: u32,
            is_external_representation: u8,
        ) -> CFStringRef;

        pub fn CFStringGetLength(the_string: CFStringRef) -> isize;

        pub fn CFStringGetBytes(
            the_string: CFStringRef,
            range: CFRange,
            encoding: u32,
            loss_byte: u8,
            is_external_representation: u8,
            buffer: *mut u8,
            max_buf_len: isize,
            used_buf_len: *mut isize,
        ) -> isize;

        pub fn CFStringCreateMutableCopy(
            alloc: *const c_void,
            max_length: isize,
            the_string: CFStringRef,
        ) -> *mut c_void;

        pub fn CFStringTransform(
            string: *mut c_void,
            range: *mut CFRange,
            transform: CFStringRef,
            reverse: u8,
        ) -> u8;

        pub fn CFLocaleCreate(
            allocator: *const c_void,
            locale_identifier: CFStringRef,
        ) -> *const c_void;

        pub fn CFStringTokenizerCreate(
            alloc: *const c_void,
            string: CFStringRef,
            range: CFRange,
            options: usize,
            locale: *const c_void,
        ) -> *mut c_void;

        pub fn CFStringTokenizerSetString(
            tokenizer: *mut c_void,
            string: CFStringRef,
            range: CFRange,
        );

        pub fn CFStringTokenizerAdvanceToNextToken(tokenizer: *mut c_void) -> usize;

        pub fn CFStringTokenizerCopyCurrentTokenAttribute(
            tokenizer: *mut c_void,
            attribute: usize,
        ) -> CFStringRef;

        pub fn CFRelease(cf: *const c_void);
    }

    pub const K_CF_STRING_TOKENIZER_ATTRIBUTE_LATIN_TRANSCRIPTION: usize = 1 << 16;
}

/// macOS システム辞書（大辞林等）で単語の定義・解説が存在するか直接照会する。
/// DCSCopyTextDefinition は形態素解析による語幹・終止形補正を行うため（例: "貸そ" → "か・す【貸す】"、
/// "書こ" → "か・く【書く】"、"取ろ" → 空文字列）、返却された定義ヘッダの見出し語と照合し、
/// 活用断片（送り仮名漏れ）の誤ヒットを排除する。
#[cfg(target_os = "macos")]
fn try_acquire_ffi_lock(
    ffi_deadline: Option<std::time::Instant>,
) -> Option<MutexGuard<'static, ()>> {
    // DictionaryServices (DCSCopyTextDefinition) の内部インデックス参照はマルチスレッド同時初期化・照会時に
    // 競合して NULL を返すことがあるため、キャッシュミス時の FFI 呼び出しのみ Mutex で直列化する。
    static FFI_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = FFI_LOCK.get_or_init(|| Mutex::new(()));
    let Some(deadline) = ffi_deadline else {
        return Some(lock.lock().unwrap_or_else(|e| e.into_inner()));
    };
    loop {
        match lock.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// マルチスレッド Tokio ランタイム上の場合のみ `block_in_place` で囲み、
/// 非同期ワーカースレッドが同期 FFI / mutex 待機でブロックされないようにする。
#[inline]
fn run_sync_ffi<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// 予算期限内に FFI ロックを取得できた場合のみ macOS システム辞書を照会する。
/// ロック待機で期限超過した場合は `None` を返し、キャッシュ未登録のままスキップを通知する。
#[cfg(target_os = "macos")]
fn query_system_dict_definition_budgeted(
    word: &str,
    ffi_deadline: Option<std::time::Instant>,
) -> Option<bool> {
    if word.is_empty() {
        return Some(false);
    }
    run_sync_ffi(|| {
        let _guard = try_acquire_ffi_lock(ffi_deadline)?;
        Some(query_system_dict_definition_locked(word))
    })
}

#[cfg(target_os = "macos")]
fn query_system_dict_definition_locked(word: &str) -> bool {
    unsafe {
        let cf_str = ffi::CFStringCreateWithBytes(
            std::ptr::null(),
            word.as_ptr(),
            word.len() as isize,
            ffi::K_CF_STRING_ENCODING_UTF8,
            0,
        );
        if cf_str.is_null() {
            return false;
        }
        // CFRange は UTF-16 コード単位（UniChar 長）を要求するため、
        // サロゲートペア文字（異体字・絵文字等）でも欠落しないよう CFStringGetLength を使用する
        let range = ffi::CFRange {
            location: 0,
            length: ffi::CFStringGetLength(cf_str),
        };
        let def = ffi::DCSCopyTextDefinition(std::ptr::null(), cf_str, range);
        FFI_WARMED.store(true, Ordering::Release);
        ffi::CFRelease(cf_str);
        if def.is_null() {
            return false;
        }

        let def_len = ffi::CFStringGetLength(def);
        if def_len <= 0 {
            ffi::CFRelease(def);
            return false;
        }

        // 先頭最大 128 UTF-16 コード単位（1行目の見出し語ヘッダ）をスタックバッファへ UTF-8 抽出
        let mut buf = [0u8; 512];
        let mut used_len: isize = 0;
        let head_range = ffi::CFRange {
            location: 0,
            length: def_len.min(128),
        };
        let converted = ffi::CFStringGetBytes(
            def,
            head_range,
            ffi::K_CF_STRING_ENCODING_UTF8,
            b'?',
            0,
            buf.as_mut_ptr(),
            buf.len() as isize,
            &mut used_len,
        );
        ffi::CFRelease(def);

        if converted <= 0 || used_len <= 0 {
            return false;
        }

        let valid_bytes = &buf[..(used_len as usize).min(buf.len())];
        let header_str = match std::str::from_utf8(valid_bytes) {
            Ok(s) => s,
            Err(e) => {
                let valid_up_to = e.valid_up_to();
                if valid_up_to == 0 {
                    return false;
                }
                match std::str::from_utf8(&valid_bytes[..valid_up_to]) {
                    Ok(s) => s,
                    Err(_) => return false,
                }
            }
        };

        matches_dict_headword(word, header_str)
    }
}

/// 辞書定義テキストの1行目（見出し語ヘッダ）が照会単語 `word` と一致するか検証する。
/// 例:
/// - `が・そ 1【画素】` vs `"画素"` -> true
/// - `か・す 0【貸す・藉す】` vs `"貸そ"` -> false（形態素ステミングによる誤ヒットを排除）
/// - `うり・あげ 0【売（り）上げ】` vs `"売上げ"` / `"売り上げ"` -> true
/// - `コンピューター 3computer...` vs `"コンピューター"` / `"コンピュータ"` -> true
fn matches_dict_headword(word: &str, def_text: &str) -> bool {
    let first_line = def_text.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return false;
    }

    if let Some(open_idx) = first_line.find('【') {
        let after_open = &first_line[open_idx + '【'.len_utf8()..];
        if let Some(rel_close) = after_open.find('】') {
            let reading_part = &first_line[..open_idx];
            let bracket_part = &after_open[..rel_close];

            for raw_entry in bracket_part.split('・') {
                let entry = raw_entry.trim();
                // ゼロアロケーション・ファストパス（大半の標準見出し語はここで即一致）
                if entry == word {
                    return true;
                }
                if matches_bracket_entry_variants(word, entry) {
                    return true;
                }
            }

            // かな見出し照合（例: "きれい" -> "きれい 1【綺麗・奇麗】"）
            let reading_token = reading_part.split_whitespace().next().unwrap_or("");
            if !reading_token.is_empty() {
                let clean_reading: String = reading_token
                    .chars()
                    .filter(|&c| c != '・' && c != '‐' && c != '-')
                    .collect();
                if clean_reading == word {
                    return true;
                }
            }

            return false;
        }
    }

    // 【...】を持たないカタカナ語等の見出し（例: "コンピューター 3computer..."）
    let head_prefix: String = first_line
        .chars()
        .take_while(|&c| !c.is_whitespace() && !c.is_ascii_digit() && c != '【' && c != '（' && c != '(')
        .filter(|&c| c != '・' && c != '‐' && c != '-')
        .collect();

    !head_prefix.is_empty()
        && (head_prefix == word
            || head_prefix.starts_with(word)
            || word.starts_with(&head_prefix))
}

#[inline]
fn needs_bracket_normalization(entry: &str) -> bool {
    entry.chars().any(|c| {
        matches!(
            c,
            '（' | '(' | '〔' | '[' | '）' | ')' | '〕' | ']' | '〈' | '〉' | '《' | '》' | '△' | '×' | '▽' | '‐' | '-' | '・' | ' ' | '　'
        )
    })
}

/// 短い方の文字列が長い方の文字列の順序付き文字部分列であり、
/// かつ追加で出現する差分文字がすべて平仮名（中間送り仮名の省略差）であるか判定する。
fn is_okurigana_variant_subsequence(a: &str, b: &str, a_count: usize, b_count: usize) -> bool {
    let (shorter, longer) = if a_count <= b_count { (a, b) } else { (b, a) };
    let mut short_iter = shorter.chars().peekable();
    for lc in longer.chars() {
        if short_iter.peek() == Some(&lc) {
            short_iter.next();
        } else if !('\u{3041}'..='\u{3096}').contains(&lc) {
            return false;
        }
    }
    short_iter.peek().is_none()
}

/// 3文字以上の複合語における中間送り仮名省略の許容判定
#[inline]
fn matches_compound_okuri_variant(word: &str, candidate_form: &str) -> bool {
    let word_count = word.chars().count();
    let form_count = candidate_form.chars().count();
    word_count >= 3
        && form_count >= 3
        && word.chars().next() == candidate_form.chars().next()
        && word.chars().last() == candidate_form.chars().last()
        && is_okurigana_variant_subsequence(word, candidate_form, word_count, form_count)
}

/// `【...】` 内の個別見出し表記（例: `売（り）上げ`）と `word` を照合する。
/// 括弧内の送り仮名を「省いた形（売上げ）」「含めた形（売り上げ）」の双方、および3文字以上の複合語の中間送り仮名揺れを許容する。
fn matches_bracket_entry_variants(word: &str, entry: &str) -> bool {
    if !needs_bracket_normalization(entry) {
        return entry == word || matches_compound_okuri_variant(word, entry);
    }

    let mut without_parens = String::with_capacity(entry.len());
    let mut with_parens = String::with_capacity(entry.len());
    let mut depth = 0usize;

    for c in entry.chars() {
        match c {
            '（' | '(' | '〔' | '[' => {
                depth += 1;
            }
            '）' | ')' | '〕' | ']' => {
                depth = depth.saturating_sub(1);
            }
            '〈' | '〉' | '《' | '》' | '△' | '×' | '▽' | '‐' | '-' | '・' | ' ' | '　' => {}
            _ => {
                if depth == 0 {
                    without_parens.push(c);
                }
                with_parens.push(c);
            }
        }
    }

    if without_parens == word || with_parens == word {
        return true;
    }

    matches_compound_okuri_variant(word, &with_parens)
}

#[cfg(not(target_os = "macos"))]
fn query_system_dict_definition_budgeted(
    _word: &str,
    _ffi_deadline: Option<std::time::Instant>,
) -> Option<bool> {
    Some(false)
}

/// システム辞書判定のインメモリキャッシュ（最大20,000件、FIFO 退去）
const MAX_CACHE_ENTRIES: usize = 20_000;
/// バックグラウンド事前登録キューの上限件数
const MAX_PREWARM_QUEUE: usize = 256;

struct SystemDictCacheInner {
    map: HashMap<Arc<str>, bool>,
    order: VecDeque<Arc<str>>,
}

pub struct SystemDictCache {
    cache: RwLock<SystemDictCacheInner>,
}

impl Default for SystemDictCache {
    fn default() -> Self {
        Self {
            cache: RwLock::new(SystemDictCacheInner {
                map: HashMap::with_capacity(1024),
                order: VecDeque::with_capacity(1024),
            }),
        }
    }
}

impl SystemDictCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 単語がシステム辞書に存在するか、1 回の read ロック参照＋予算期限制御で判定する。
    /// キャッシュミス時、`allow_cold_ffi == false` または予算超過で FFI を開始できない場合は `None` を返す。
    pub(crate) fn lookup_with_budget(
        &self,
        word: &str,
        allow_cold_ffi: bool,
        ffi_deadline: Option<std::time::Instant>,
        track_foreground: bool,
    ) -> Option<bool> {
        if word.is_empty() {
            return Some(false);
        }

        // 読み取りロックで高速参照（ヒット時は 1 回のロック取得で完結）
        if let Ok(guard) = self.cache.read()
            && let Some(&res) = guard.map.get(word)
        {
            return Some(res);
        }

        if !allow_cold_ffi || ffi_deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return None;
        }

        let _fg = track_foreground.then(ForegroundWaiterGuard::new);
        let res = query_system_dict_definition_budgeted(word, ffi_deadline)?;

        if let Ok(mut guard) = self.cache.write()
            && !guard.map.contains_key(word)
        {
            while guard.map.len() >= MAX_CACHE_ENTRIES
                && let Some(oldest) = guard.order.pop_front()
            {
                guard.map.remove(&oldest);
            }
            let key: Arc<str> = Arc::from(word);
            guard.order.push_back(Arc::clone(&key));
            guard.map.insert(key, res);
        }

        Some(res)
    }
}

/// グローバルなシステム辞書キャッシュのシングルトン
fn global_dict_cache() -> &'static SystemDictCache {
    static CACHE: OnceLock<SystemDictCache> = OnceLock::new();
    CACHE.get_or_init(SystemDictCache::new)
}

/// 候補単語が「SKK注釈付き」または「システム辞書に項目が存在する（annotationが引ける）」か判定する。
/// 送りあり見出し（例: "かk", "すばらしi"）が渡された場合、語幹＋終止形（例: "書く", "素晴らしい"）でも判定する。
/// `allow_cold_ffi` が false または `ffi_deadline` を超過した場合はインメモリキャッシュのみを参照し、
/// 未キャッシュの重い FFI 照会をスキップする。戻り値は `(辞書掲載/注釈ありか, 予算超過/禁止により未キャッシュ FFI をスキップしたか)`。
#[cfg(test)]
pub fn is_annotated_or_in_dict_budgeted(
    candidate: &str,
    midashi: Option<&str>,
    allow_cold_ffi: bool,
    ffi_deadline: Option<std::time::Instant>,
) -> (bool, bool) {
    let parsed_okuri = midashi.and_then(parse_okuri_midashi_extended);
    is_annotated_or_in_dict_parsed_budgeted(
        candidate,
        parsed_okuri.as_ref(),
        allow_cold_ffi,
        ffi_deadline,
    )
}

/// 事前にパース済みの `OkuriMidashi` を受け取り、繰り返し見出し語パースを行わずに判定する。
pub fn is_annotated_or_in_dict_parsed_budgeted(
    candidate: &str,
    parsed_okuri: Option<&OkuriMidashi<'_>>,
    allow_cold_ffi: bool,
    ffi_deadline: Option<std::time::Instant>,
) -> (bool, bool) {
    is_annotated_or_in_dict_impl(candidate, parsed_okuri, allow_cold_ffi, ffi_deadline, true)
}

fn is_annotated_or_in_dict_impl(
    candidate: &str,
    parsed_okuri: Option<&OkuriMidashi<'_>>,
    allow_cold_ffi: bool,
    ffi_deadline: Option<std::time::Instant>,
    track_foreground: bool,
) -> (bool, bool) {
    // 1. 候補文字列自体に ';' による SKK 注釈が含まれている場合は即座に true
    if candidate.contains(';') {
        return (true, false);
    }

    let clean = clean_candidate(candidate);
    if clean.is_empty() {
        return (false, false);
    }

    let cache = global_dict_cache();
    let mut skipped_cold = false;
    let mut check_word = |w: &str| -> bool {
        match cache.lookup_with_budget(w, allow_cold_ffi, ffi_deadline, track_foreground) {
            Some(res) => res,
            None => {
                skipped_cold = true;
                false
            }
        }
    };

    // 2. 送りあり見出しの場合、語幹＋終止形・名詞形送り仮名を最優先で照会する。
    //    動詞・形容詞の語幹単体（例: 「切」）よりも終止形（例: 「切る」）の方が国語辞典の見出し語に
    //    100%確実にヒットするため、終止形を先に照会して不要なキャッシュミスと FFI 呼び出しを激減させる。
    if let Some(parsed) = parsed_okuri {
        let mut explicit_suffix_opt: Option<&str> = None;
        let key_opt = match parsed {
            OkuriMidashi::Simple { key, .. } => Some(*key),
            OkuriMidashi::Compound { okuri_key, .. } => Some(*okuri_key),
            OkuriMidashi::Explicit { suffix, .. } => {
                explicit_suffix_opt = Some(*suffix);
                suffix.chars().next().and_then(crate::okuri::kana_to_okuri_key)
            }
        };

        let mut full_word = String::with_capacity(clean.len() + 12);
        full_word.push_str(clean);
        let base_len = full_word.len();

        if let Some(explicit_suffix) = explicit_suffix_opt {
            full_word.push_str(explicit_suffix);
            if check_word(&full_word) {
                return (true, false);
            }
            full_word.truncate(base_len);
        }

        if let Some(key) = key_opt {
            let suffixes = crate::okuri::okuri_key_to_dict_suffixes(key);
            for &suffix in suffixes {
                full_word.push_str(suffix);
                if check_word(&full_word) {
                    return (true, false);
                }
                full_word.truncate(base_len);
            }
        }
    }

    // 3. 送りなし見出し、または語幹＋終止形でヒットしなかった場合、単語本体がシステム辞書に存在するか
    if check_word(clean) {
        (true, false)
    } else {
        (false, skipped_cold)
    }
}

struct PrewarmState {
    queue: Mutex<VecDeque<(String, Option<String>)>>,
    cvar: Condvar,
}

fn global_prewarm_queue() -> &'static Arc<PrewarmState> {
    static QUEUE: OnceLock<Arc<PrewarmState>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let state = Arc::new(PrewarmState {
            queue: Mutex::new(VecDeque::with_capacity(64)),
            cvar: Condvar::new(),
        });
        let worker_state = Arc::clone(&state);
        let _ = std::thread::Builder::new()
            .name("ruskkserv-prewarm".to_string())
            .spawn(move || {
                loop {
                    let item = {
                        let mut guard = worker_state.queue.lock().unwrap_or_else(|e| e.into_inner());
                        while guard.is_empty() {
                            guard = worker_state.cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
                        }
                        guard.pop_front()
                    };
                    if let Some((candidate, midashi)) = item {
                        let mut waited = 0u8;
                        while FOREGROUND_FFI_WAITERS.load(Ordering::Acquire) > 0 && waited < 50 {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            waited += 1;
                        }
                        let parsed = midashi.as_deref().and_then(parse_okuri_midashi_extended);
                        let _ = is_annotated_or_in_dict_impl(
                            &candidate,
                            parsed.as_ref(),
                            true,
                            None,
                            false,
                        );
                    }
                }
            });
        state
    })
}

/// 予算制限により本番リクエスト中にスキップされた候補群や起動時代表語を、
/// 単一の有界バックグラウンドワーカースレッド（最大 256 件）へ投入して先行登録する。
/// キュー内に既に同一エントリが存在する場合は重複投入を抑止する。
/// ワーカーはフォアグラウンド変換中の場合一時譲歩し、ユーザー入力をブロックしない。
pub fn enqueue_prewarm(entries: impl IntoIterator<Item = (String, Option<String>)>) {
    let state = global_prewarm_queue();
    let mut guard = state.queue.lock().unwrap_or_else(|e| e.into_inner());
    let mut added = false;
    for entry in entries {
        if guard.len() >= MAX_PREWARM_QUEUE {
            break;
        }
        if !guard.iter().any(|existing| existing == &entry) {
            guard.push_back(entry);
            added = true;
        }
    }
    if added {
        state.cvar.notify_one();
    }
}

/// 候補単語が「SKK注釈付き」または「システム辞書に項目が存在する」か判定する（標準判定）。
#[inline]
pub fn is_annotated_or_in_dict(candidate: &str, midashi: Option<&str>) -> bool {
    let parsed_okuri = midashi.and_then(parse_okuri_midashi_extended);
    is_annotated_or_in_dict_impl(candidate, parsed_okuri.as_ref(), true, None, true).0
}

/// 文字列がすべて平仮名・カタカナ・長音記号（`ー`）のみで構成されている場合、
/// 平仮名へ正規化した文字列を返す（長音 `ー` は維持）。漢字や ASCII 等を含む場合は `None`。
pub fn pure_kana_to_hiragana(word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        if ('\u{3041}'..='\u{3096}').contains(&c) || c == 'ー' {
            out.push(c);
        } else if ('\u{30A1}'..='\u{30F3}').contains(&c) {
            let h = char::from_u32(c as u32 - 0x60)?;
            out.push(h);
        } else if c == 'ヴ' {
            out.push('ゔ');
        } else if c == 'ヵ' {
            out.push('か');
        } else if c == 'ヶ' {
            out.push('け');
        } else {
            return None;
        }
    }
    Some(out)
}

/// 形態素解析で読み揺れしやすい慣用複合語の確定読みテーブル
fn known_compound_reading_override(word: &str) -> Option<&'static str> {
    let r = match word {
        "一軒家" => "いっけんや",
        "一文" => "いちぶん",
        "和菓子" => "わがし",
        "咽喉" => "いんこう",
        "地ビール" => "じびーる",
        "小麦粉" => "こむぎこ",
        "手下" => "てした",
        "日本刀" => "にほんとう",
        "日本酒" => "にほんしゅ",
        "柴犬" => "しばいぬ",
        "珈琲" => "こーひー",
        "コーヒー豆" => "こーひーまめ",
        "ウーロン茶" => "うーろんちゃ",
        "生ビール" => "なまびーる",
        "生肉" => "なまにく",
        "真犯人" => "しんはんにん",
        "麦酒" => "びーる",
        "〆切" => "しめきり",
        _ => return None,
    };
    Some(r)
}

#[cfg(target_os = "macos")]
struct ReusableTokenizer {
    tokenizer: *mut std::ffi::c_void,
    transform_latin_hira: ffi::CFStringRef,
}

#[cfg(target_os = "macos")]
// SAFETY: ReusableTokenizer はグローバル Mutex のロック下でシングルスレッド排他アクセスされる。
unsafe impl Send for ReusableTokenizer {}

#[cfg(target_os = "macos")]
fn tokenize_to_hiragana_macos(word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    if let Some(hira) = pure_kana_to_hiragana(word) {
        return Some(hira);
    }

    // カタカナ長音（`ー`）と漢字が混在する複合語（例: "ジェット機", "コーヒー豆"）は、
    // `Latin-Hiragana` 変換で `ー` が母音展開されるのを防ぐため、カナ連続部分と非カナ部分に分割して処理する
    if word.contains('ー') {
        let mut combined = String::with_capacity(word.len() * 2);
        let mut segment_start = 0usize;
        let mut current_is_kana: Option<bool> = None;

        for (idx, c) in word.char_indices() {
            let is_kana = ('\u{3041}'..='\u{3096}').contains(&c)
                || ('\u{30A1}'..='\u{30F6}').contains(&c)
                || c == 'ー';
            if let Some(prev_kana) = current_is_kana {
                if prev_kana != is_kana {
                    let seg = &word[segment_start..idx];
                    let part = if prev_kana {
                        pure_kana_to_hiragana(seg)?
                    } else {
                        tokenize_to_hiragana_macos_raw(seg)?
                    };
                    combined.push_str(&part);
                    segment_start = idx;
                    current_is_kana = Some(is_kana);
                }
            } else {
                current_is_kana = Some(is_kana);
            }
        }
        if let Some(prev_kana) = current_is_kana {
            let seg = &word[segment_start..];
            let part = if prev_kana {
                pure_kana_to_hiragana(seg)?
            } else {
                tokenize_to_hiragana_macos_raw(seg)?
            };
            combined.push_str(&part);
        }
        return (!combined.is_empty()).then_some(combined);
    }

    tokenize_to_hiragana_macos_raw(word)
}

#[cfg(target_os = "macos")]
struct CfRefGuard(*const std::ffi::c_void);

#[cfg(target_os = "macos")]
impl Drop for CfRefGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ffi::CFRelease(self.0) };
        }
    }
}

#[cfg(target_os = "macos")]
fn tokenize_to_hiragana_macos_raw(word: &str) -> Option<String> {
    static TOKENIZER_LOCK: OnceLock<Mutex<Option<ReusableTokenizer>>> = OnceLock::new();
    let lock = TOKENIZER_LOCK.get_or_init(|| {
        unsafe {
            let ja_bytes = b"ja_JP";
            let ja_str = CfRefGuard(ffi::CFStringCreateWithBytes(
                std::ptr::null(),
                ja_bytes.as_ptr(),
                ja_bytes.len() as isize,
                ffi::K_CF_STRING_ENCODING_UTF8,
                0,
            ));
            if ja_str.0.is_null() {
                return Mutex::new(None);
            }
            let locale = CfRefGuard(ffi::CFLocaleCreate(std::ptr::null(), ja_str.0));
            if locale.0.is_null() {
                return Mutex::new(None);
            }
            let empty_str = CfRefGuard(ffi::CFStringCreateWithBytes(
                std::ptr::null(),
                b"".as_ptr(),
                0,
                ffi::K_CF_STRING_ENCODING_UTF8,
                0,
            ));
            if empty_str.0.is_null() {
                return Mutex::new(None);
            }
            let tok = ffi::CFStringTokenizerCreate(
                std::ptr::null(),
                empty_str.0,
                ffi::CFRange {
                    location: 0,
                    length: 0,
                },
                0,
                locale.0,
            );
            if tok.is_null() {
                return Mutex::new(None);
            }

            let tx_bytes = b"Latin-Hiragana";
            let tx_str = ffi::CFStringCreateWithBytes(
                std::ptr::null(),
                tx_bytes.as_ptr(),
                tx_bytes.len() as isize,
                ffi::K_CF_STRING_ENCODING_UTF8,
                0,
            );
            if tx_str.is_null() {
                ffi::CFRelease(tok);
                return Mutex::new(None);
            }

            Mutex::new(Some(ReusableTokenizer {
                tokenizer: tok,
                transform_latin_hira: tx_str,
            }))
        }
    });

    let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let state = guard.as_ref()?;

    unsafe {
        let cf_str = CfRefGuard(ffi::CFStringCreateWithBytes(
            std::ptr::null(),
            word.as_ptr(),
            word.len() as isize,
            ffi::K_CF_STRING_ENCODING_UTF8,
            0,
        ));
        if cf_str.0.is_null() {
            return None;
        }
        let range = ffi::CFRange {
            location: 0,
            length: ffi::CFStringGetLength(cf_str.0),
        };
        ffi::CFStringTokenizerSetString(state.tokenizer, cf_str.0, range);

        let mut out = String::with_capacity(word.len() * 2);
        let mut buf = [0u8; 256];

        while ffi::CFStringTokenizerAdvanceToNextToken(state.tokenizer) != 0 {
            let latin = CfRefGuard(ffi::CFStringTokenizerCopyCurrentTokenAttribute(
                state.tokenizer,
                ffi::K_CF_STRING_TOKENIZER_ATTRIBUTE_LATIN_TRANSCRIPTION,
            ));
            if latin.0.is_null() {
                return None;
            }
            let mutable_raw = ffi::CFStringCreateMutableCopy(std::ptr::null(), 0, latin.0);
            if mutable_raw.is_null() {
                return None;
            }
            let _mutable_guard = CfRefGuard(mutable_raw);
            let ok = ffi::CFStringTransform(
                mutable_raw,
                std::ptr::null_mut(),
                state.transform_latin_hira,
                0,
            );
            if ok == 0 {
                return None;
            }
            let len = ffi::CFStringGetLength(mutable_raw);
            let mut used: isize = 0;
            let converted = ffi::CFStringGetBytes(
                mutable_raw,
                ffi::CFRange {
                    location: 0,
                    length: len,
                },
                ffi::K_CF_STRING_ENCODING_UTF8,
                b'?',
                0,
                buf.as_mut_ptr(),
                buf.len() as isize,
                &mut used,
            );
            if converted != len || used <= 0 {
                return None;
            }
            let token_str = std::str::from_utf8(&buf[..(used as usize).min(buf.len())]).ok()?;
            // 空白やアクセント記号等を除去し純平仮名として連結
            for c in token_str.chars() {
                if c.is_whitespace() || matches!(c, '・' | '\'' | '’' | '-') {
                    continue;
                }
                if ('\u{3041}'..='\u{3096}').contains(&c) || c == 'ー' {
                    out.push(c);
                } else if ('\u{30A1}'..='\u{30F3}').contains(&c)
                    && let Some(h) = char::from_u32(c as u32 - 0x60)
                {
                    out.push(h);
                } else if c == 'ヴ' {
                    out.push('ゔ');
                } else {
                    return None;
                }
            }
        }

        (!out.is_empty()).then_some(out)
    }
}

#[cfg(not(target_os = "macos"))]
fn tokenize_to_hiragana_macos(word: &str) -> Option<String> {
    pure_kana_to_hiragana(word)
}

/// 送りなし単語（漢字熟語・カタカナ語・複合語、例: `"主治医"`, `"広葉樹"`, `"銀メダル"`, `"パーカー"`）から
/// SKK の見出し語補完（`opcode 4`）に適した平仮名見出し語（`"しゅじい"`, `"こうようじゅ"`, `"ぎんめだる"`, `"ぱーかー"`）を推定する。
/// 結果はプロセス内のグローバルキャッシュに保持され、2 回目以降は $O(1)$ で即座に返る。
pub fn infer_hiragana_reading(word: &str) -> Option<String> {
    if word.chars().count() < 2 {
        return None;
    }
    if word
        .chars()
        .any(|c| c.is_ascii() || c.is_whitespace() || c.is_control())
    {
        return None;
    }
    // 送りあり用言（末尾が平仮名で非平仮名を含む語、例: "切る", "診る", "確認する"）は除外
    // （※サ変動詞 "確認する" 等は expand_aliases で語幹 "確認" が展開されるため、そちらが送りなし名詞として対象になる）
    if word.chars().last().is_some_and(|c| ('\u{3041}'..='\u{3096}').contains(&c))
        && word.chars().any(|c| !('\u{3041}'..='\u{3096}').contains(&c))
        && !word.starts_with('お')
        && !word.starts_with('ご')
    {
        return None;
    }

    if let Some(ov) = known_compound_reading_override(word) {
        return Some(ov.to_string());
    }
    if let Some(hira) = pure_kana_to_hiragana(word) {
        return Some(hira);
    }

    static READING_CACHE: OnceLock<RwLock<HashMap<String, Option<String>>>> = OnceLock::new();
    let cache = READING_CACHE.get_or_init(|| RwLock::new(HashMap::with_capacity(2048)));

    if let Ok(guard) = cache.read()
        && let Some(cached) = guard.get(word)
    {
        return cached.clone();
    }

    let resolved = tokenize_to_hiragana_macos(word).filter(|r| r.chars().count() >= 2);
    if let Ok(mut guard) = cache.write()
        && guard.len() < MAX_CACHE_ENTRIES
    {
        guard.insert(word.to_string(), resolved.clone());
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_annotation_detection() {
        assert!(is_annotated_or_in_dict("画;線を画く", None));
        assert!(is_annotated_or_in_dict("計る;時間・計画", None));
        assert!(is_annotated_or_in_dict("切;注釈", Some("きr")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_system_dict_lookup() {
        // macOS 環境の大辞林等に確実に存在する語
        assert!(is_annotated_or_in_dict("時間", None));
        assert!(is_annotated_or_in_dict("切る", None));
        assert!(is_annotated_or_in_dict("東京", None));

        // 送りあり復元判定: 語幹「書」+ 見出し「かk」-> 「書く」が大辞林にヒット
        assert!(is_annotated_or_in_dict("書", Some("かk")));
        // 語幹「素晴らし」+ 見出し「すばらしi」-> 「素晴らしい」が大辞林にヒット
        assert!(is_annotated_or_in_dict("素晴らし", Some("すばらしi")));
        // タ行連用形名詞・ナ行下一段・アスタリスク明示送り仮名も大辞林にヒット
        assert!(is_annotated_or_in_dict("手持", Some("てもt")));
        assert!(is_annotated_or_in_dict("兼", Some("かn")));
        assert!(is_annotated_or_in_dict("値上", Some("ねあ*げ")));

        // サロゲートペア文字（UTF-16 長 > Unicode スカラー値数）を含む単語でも正確な UTF-16 長で照会できること
        // "𠮟る" = '𠮟' (U+20B9F: UTF-16 2 units) + 'る' (1 unit) -> chars().count() == 2, UTF-16 length == 3
        let word_surrogate = "𠮟る";
        assert_eq!(word_surrogate.chars().count(), 2);
        assert_eq!(word_surrogate.encode_utf16().count(), 3);
        unsafe {
            let cf_str = ffi::CFStringCreateWithBytes(
                std::ptr::null(),
                word_surrogate.as_ptr(),
                word_surrogate.len() as isize,
                ffi::K_CF_STRING_ENCODING_UTF8,
                0,
            );
            assert!(!cf_str.is_null());
            assert_eq!(ffi::CFStringGetLength(cf_str), 3);
            ffi::CFRelease(cf_str);
        }
        assert!(!is_annotated_or_in_dict("😀xyz123", None));

        // 辞書に存在しない造語や記号
        assert!(!is_annotated_or_in_dict("すバラス", None));
        assert!(!is_annotated_or_in_dict("次場RAS", None));
        assert!(!is_annotated_or_in_dict("hoge12345", None));

        // 送りなし見出し「がそ」の「画素」は true、「貸そ」（「貸す」の未然・意志形断片）は false となること
        assert!(is_annotated_or_in_dict("画素", Some("がそ")));
        assert!(!is_annotated_or_in_dict("貸そ", Some("がそ")));
        assert!(!is_annotated_or_in_dict("解そ", Some("げそ")));
        assert!(!is_annotated_or_in_dict("書こ", Some("かこ")));
        assert!(!is_annotated_or_in_dict("見よ", Some("みよ")));
        assert!(!is_annotated_or_in_dict("取ろ", Some("とろ")));
        assert!(!is_annotated_or_in_dict("右や", Some("みぎや")));
        assert!(!is_annotated_or_in_dict("左や", Some("ひだりや")));
        assert!(!is_annotated_or_in_dict("したヤ", Some("したや")));
    }

    #[test]
    fn test_matches_dict_headword_unit() {
        assert!(matches_dict_headword("画素", "が・そ 1【画素】"));
        assert!(!matches_dict_headword("貸そ", "か・す 0【貸す・藉す】"));
        assert!(!matches_dict_headword("書こ", "か・く 1【書く】"));
        assert!(!matches_dict_headword("取ろ", ""));
        assert!(matches_dict_headword("売上げ", "うり・あげ 0【売（り）上げ】"));
        assert!(matches_dict_headword("売り上げ", "うり・あげ 0【売（り）上げ】"));
        assert!(matches_dict_headword("きれい", "きれい 1【綺麗・奇麗】"));
        assert!(matches_dict_headword(
            "コンピューター",
            "コンピューター 3computer"
        ));
        assert!(matches_dict_headword(
            "コンピュータ",
            "コンピューター 3computer"
        ));
        assert!(matches_dict_headword(
            "問い合せ",
            "とい・あわせ 0【問い（合）わせ】"
        ));
        assert!(matches_dict_headword(
            "問い合わせ",
            "とい・あわせ 0【問（い）合わせ】"
        ));
        // 漢字違いや異文字の 3 文字語（先頭・末尾だけ一致）が誤ヒットしないこと
        assert!(!matches_dict_headword("貸しす", "かし・だす 0【貸し出す】"));
        assert!(!matches_dict_headword("切らる", "きり・とる 0【切り取る】"));
        // 大辞林の熟字訓・特殊読み記号（〈...〉, 《...》, △, ×, ▽）を正規化して照合できること
        assert!(matches_dict_headword("親族", "うから 【〈親族〉 】"));
        assert!(matches_dict_headword("友達", "ともだち 0【《友達》 】"));
        assert!(matches_dict_headword("時計", "とけい 0【《時計》 ・土圭】"));
        assert!(matches_dict_headword("梅雨", "つゆ 0【《梅雨》 ・〈黴雨〉 】"));
        assert!(matches_dict_headword("黴雨", "つゆ 0【《梅雨》 ・〈黴雨〉 】"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_latency_benchmark() {
        use std::time::Instant;

        let words = [
            "日本", "林檎", "蜜柑", "飛行機", "電車の窓", "京都大学", "計算機",
            "開発者", "プログラミング", "変換辞書",
        ];

        // 1. 初回照会（キャッシュミス時）のレイテンシ
        let start = Instant::now();
        for &w in &words {
            let _ = is_annotated_or_in_dict(w, None);
        }
        let cold_elapsed = start.elapsed();

        // 2. 2回目照会（キャッシュヒット時）のレイテンシ
        let start = Instant::now();
        for &w in &words {
            let _ = is_annotated_or_in_dict(w, None);
        }
        let hot_elapsed = start.elapsed();

        println!(
            "\n[Benchmark] 10 words cold lookup: {:?} ({:?}/word)",
            cold_elapsed,
            cold_elapsed / (words.len() as u32)
        );
        println!(
            "[Benchmark] 10 words hot (cached) lookup: {:?} ({:?}/word)\n",
            hot_elapsed,
            hot_elapsed / (words.len() as u32)
        );
    }

    #[test]
    fn test_budgeted_dict_lookup_respects_deadline_and_uses_cache() {
        use std::time::{Duration, Instant};

        // 注釈付きはデッドライン超過でも常に即 true（FFI スキップなし）
        let past_deadline = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
        assert_eq!(
            is_annotated_or_in_dict_budgeted("画;注釈", None, true, Some(past_deadline)),
            (true, false)
        );

        // 未キャッシュ・未知語に過去デッドラインを渡すとコールド FFI をスキップし cold_skipped=true となる
        let unseen = "予算超過時未キャッシュ検証用テスト語XYZ987654321";
        assert_eq!(
            is_annotated_or_in_dict_budgeted(unseen, None, true, Some(past_deadline)),
            (false, true)
        );

        // 一度通常照会でキャッシュに載せた後は、過去デッドラインでもキャッシュヒットして同じ結果が返る
        #[cfg(target_os = "macos")]
        {
            assert!(is_annotated_or_in_dict("時間", None));
            assert_eq!(
                is_annotated_or_in_dict_budgeted("時間", None, true, Some(past_deadline)),
                (true, false)
            );
        }
    }

    #[test]
    fn test_infer_hiragana_reading() {
        // 純カタカナ・ひらがな語（全 OS 共通）
        assert_eq!(
            infer_hiragana_reading("パーカー").as_deref(),
            Some("ぱーかー")
        );
        assert_eq!(
            infer_hiragana_reading("リポート").as_deref(),
            Some("りぽーと")
        );
        assert_eq!(
            infer_hiragana_reading("ヴァイオリン").as_deref(),
            Some("ゔぁいおりん")
        );
        assert_eq!(infer_hiragana_reading("うさぎ").as_deref(), Some("うさぎ"));

        // 除外対象（1文字、ASCII含み、送りあり用言）
        assert_eq!(infer_hiragana_reading("服"), None);
        assert_eq!(infer_hiragana_reading("あ"), None);
        assert_eq!(infer_hiragana_reading("USB"), None);
        assert_eq!(infer_hiragana_reading("Tシャツ"), None);
        assert_eq!(infer_hiragana_reading("切る"), None);
        assert_eq!(infer_hiragana_reading("診る"), None);
        assert_eq!(infer_hiragana_reading("確認する"), None);

        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                infer_hiragana_reading("主治医").as_deref(),
                Some("しゅじい")
            );
            assert_eq!(
                infer_hiragana_reading("広葉樹").as_deref(),
                Some("こうようじゅ")
            );
            assert_eq!(
                infer_hiragana_reading("銀メダル").as_deref(),
                Some("ぎんめだる")
            );
            assert_eq!(
                infer_hiragana_reading("主題歌").as_deref(),
                Some("しゅだいか")
            );
            assert_eq!(infer_hiragana_reading("親戚").as_deref(), Some("しんせき"));
            assert_eq!(
                infer_hiragana_reading("一軒家").as_deref(),
                Some("いっけんや")
            );
            assert_eq!(
                infer_hiragana_reading("ウーロン茶").as_deref(),
                Some("うーろんちゃ")
            );
            assert_eq!(
                infer_hiragana_reading("コーヒー豆").as_deref(),
                Some("こーひーまめ")
            );
            assert_eq!(
                infer_hiragana_reading("お祝い").as_deref(),
                Some("おいわい")
            );
            assert_eq!(
                infer_hiragana_reading("ぬか床").as_deref(),
                Some("ぬかどこ")
            );
        }
    }
}
