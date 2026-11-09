//! macOS システム辞書（大辞林等）および SKK 注釈（annotation）に基づく単語判定モジュール。
//! CoreServices の DictionaryServices API (DCSCopyTextDefinition) を呼び出し、
//! 国語辞典に項目が存在する単語、および SKK 注釈付き単語を判定して高速にキャッシュする。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, RwLock};

use crate::frequency::clean_candidate;

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

        pub fn CFRelease(cf: *const c_void);
    }
}

/// macOS システム辞書（大辞林等）で単語の定義・解説が存在するか直接照会する。
/// DCSCopyTextDefinition は形態素解析による語幹・終止形補正を行うため（例: "貸そ" → "か・す【貸す】"、
/// "書こ" → "か・く【書く】"、"取ろ" → 空文字列）、返却された定義ヘッダの見出し語と照合し、
/// 活用断片（送り仮名漏れ）の誤ヒットを排除する。
#[cfg(target_os = "macos")]
fn query_system_dict_definition(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    // DictionaryServices (DCSCopyTextDefinition) の内部インデックス参照はマルチスレッド同時初期化・照会時に
    // 競合して NULL を返すことがあるため、キャッシュミス時の FFI 呼び出しのみ Mutex で直列化する。
    static FFI_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = FFI_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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

/// `【...】` 内の個別見出し表記（例: `売（り）上げ`）と `word` を照合する。
/// 括弧内の送り仮名を「省いた形（売上げ）」「含めた形（売り上げ）」の双方、および3文字以上の複合語の中間送り仮名揺れを許容する。
fn matches_bracket_entry_variants(word: &str, entry: &str) -> bool {
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
            '‐' | '-' | '・' | ' ' | '　' => {}
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

    // 3文字以上の複合語における中間送り仮名省略の許容（先頭文字と末尾文字が一致する場合のみ）
    let word_count = word.chars().count();
    let entry_count = with_parens.chars().count();
    if word_count >= 3
        && entry_count >= 3
        && word.chars().next() == with_parens.chars().next()
        && word.chars().last() == with_parens.chars().last()
    {
        return true;
    }

    false
}

#[cfg(not(target_os = "macos"))]
fn query_system_dict_definition(_word: &str) -> bool {
    false
}

/// システム辞書判定のインメモリキャッシュ（最大20,000件）
const MAX_CACHE_ENTRIES: usize = 20_000;

pub struct SystemDictCache {
    cache: RwLock<HashMap<String, bool>>,
}

impl Default for SystemDictCache {
    fn default() -> Self {
        Self {
            cache: RwLock::new(HashMap::with_capacity(1024)),
        }
    }
}

impl SystemDictCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 単語がシステム辞書に存在するか、キャッシュ付きで高速判定する。
    pub fn has_definition(&self, word: &str) -> bool {
        if word.is_empty() {
            return false;
        }

        // 読み取りロックで高速参照
        if let Ok(guard) = self.cache.read()
            && let Some(&res) = guard.get(word)
        {
            return res;
        }

        // キャッシュにない場合はシステム辞書を照会
        let res = query_system_dict_definition(word);

        if let Ok(mut guard) = self.cache.write() {
            if guard.len() >= MAX_CACHE_ENTRIES {
                guard.clear();
            }
            guard.insert(word.to_string(), res);
        }

        res
    }
}

/// グローバルなシステム辞書キャッシュのシングルトン
fn global_dict_cache() -> &'static SystemDictCache {
    static CACHE: OnceLock<SystemDictCache> = OnceLock::new();
    CACHE.get_or_init(SystemDictCache::new)
}

/// 候補単語が「SKK注釈付き」または「システム辞書に項目が存在する（annotationが引ける）」か判定する。
/// 送りあり見出し（例: "かk", "すばらしi"）が渡された場合、語幹＋終止形（例: "書く", "素晴らしい"）でも判定する。
pub fn is_annotated_or_in_dict(candidate: &str, midashi: Option<&str>) -> bool {
    // 1. 候補文字列自体に ';' による SKK 注釈が含まれている場合は即座に true
    if candidate.contains(';') {
        return true;
    }

    let clean = clean_candidate(candidate);
    if clean.is_empty() {
        return false;
    }

    let cache = global_dict_cache();

    // 2. 単語本体がシステム辞書に存在するか
    if cache.has_definition(clean) {
        return true;
    }

    // 3. 送りあり見出しの場合、語幹＋終止形・名詞形送り仮名でも判定する
    if let Some(mid) = midashi
        && let Some(parsed) = crate::okuri::parse_okuri_midashi_extended(mid)
    {
        let key_opt = match parsed {
            crate::okuri::OkuriMidashi::Simple { key, .. } => Some(key),
            crate::okuri::OkuriMidashi::Compound { okuri_key, .. } => Some(okuri_key),
            crate::okuri::OkuriMidashi::Explicit { suffix, .. } => {
                let mut full_word = String::with_capacity(clean.len() + suffix.len());
                full_word.push_str(clean);
                full_word.push_str(suffix);
                if cache.has_definition(&full_word) {
                    return true;
                }
                suffix.chars().next().and_then(crate::okuri::kana_to_okuri_key)
            }
        };

        if let Some(key) = key_opt {
            let suffixes = crate::okuri::okuri_key_to_dict_suffixes(key);
            for &suffix in suffixes {
                let mut full_word = String::with_capacity(clean.len() + suffix.len());
                full_word.push_str(clean);
                full_word.push_str(suffix);
                if cache.has_definition(&full_word) {
                    return true;
                }
            }
        }
    }

    false
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
}
