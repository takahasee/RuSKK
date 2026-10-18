//! macOS システム辞書（大辞林等）および SKK 注釈（annotation）に基づく単語判定モジュール。
//! CoreServices の DictionaryServices API (DCSCopyTextDefinition) を呼び出し、
//! 国語辞典に項目が存在する単語、および SKK 注釈付き単語を判定して高速にキャッシュする。

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

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

        pub fn CFRelease(cf: *const c_void);
    }
}

/// macOS システム辞書（大辞林等）で単語の定義・解説が存在するか直接照会する。
#[cfg(target_os = "macos")]
fn query_system_dict_definition(word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
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
        let range = ffi::CFRange {
            location: 0,
            length: word.chars().count() as isize,
        };
        let def = ffi::DCSCopyTextDefinition(std::ptr::null(), cf_str, range);
        ffi::CFRelease(cf_str);
        if !def.is_null() {
            ffi::CFRelease(def);
            true
        } else {
            false
        }
    }
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

    // 3. 送りあり見出しの場合、語幹＋終止形送り仮名でも判定する
    if let Some(mid) = midashi
        && let Some((_stem, key)) = crate::okuri::parse_okuri_midashi(mid)
    {
        let suffixes = crate::okuri::okuri_key_to_suffixes(key);
        // 代表的な終止形・名詞形サフィックスを試行
        for &suffix in suffixes.iter().take(4) {
            let mut full_word = String::with_capacity(clean.len() + suffix.len());
            full_word.push_str(clean);
            full_word.push_str(suffix);
            if cache.has_definition(&full_word) {
                return true;
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

        // 辞書に存在しない造語や記号
        assert!(!is_annotated_or_in_dict("すバラス", None));
        assert!(!is_annotated_or_in_dict("次場RAS", None));
        assert!(!is_annotated_or_in_dict("hoge12345", None));
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
