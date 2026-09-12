//! SKK送りあり見出し（okuri-ari midashi）の判定・活用復元モジュール。
//! MeCab / ChaSen の活用表規則に基づき、SKKの送りキー（アルファベット）から
//! 終止形・活用形の平仮名を合成し、azooKey から返った候補から語幹（単漢字）を抽出する。

/// 送りキー（アルファベット1文字）から終止形などの送り仮名（平仮名）を返すマッピング。
/// 五段活用、一段活用、形容詞の代表的活用語尾に対応。
pub fn okuri_key_to_suffix(key: char) -> Option<&'static str> {
    okuri_key_to_suffixes(key).first().copied()
}

/// 送りキー（アルファベット1文字）から終止形・連用形などの送り仮名（平仮名）のリストを返す。
pub fn okuri_key_to_suffixes(key: char) -> &'static [&'static str] {
    match key.to_ascii_lowercase() {
        'k' => &["く", "き"], // カ行五段 (書く/書き, 咲く/咲き, 交ぜ書き)
        's' => &["す", "し"], // サ行五段 (話す/話し, 出す/出し)
        't' => &["つ", "ち"], // タ行五段 (待つ/待ち, 立つ/立ち)
        'n' => &["ぬ", "に"], // ナ行五段 (死ぬ/死に)
        'm' => &["む", "み"], // マ行五段 (読む/読み, 頼む/頼み)
        'r' => &["る", "り"], // ラ行五段/一段 (切る/切り, 着る/着)
        'g' => &["ぐ", "ぎ"], // ガ行五段 (泳ぐ/泳ぎ, 騒ぐ/騒ぎ)
        'b' => &["ぶ", "び"], // バ行五段 (遊ぶ/遊び, 結ぶ/結び)
        'u' | 'w' => &["う", "い"], // ワ行五段 (思う/思い, 買う/買い)
        'i' => &["い", "く"], // 形容詞 (美しい/美しく)
        'd' => &["だ", "で"], // 形容動詞
        _ => &[],
    }
}

/// 大文字で始まるローマ字（例: "Ga", "G", "Ka", "To", "Shi" など）を平仮名に変換する。
/// 複合語の送りあり接尾辞（例: "交ぜGak" -> "交ぜがk", "交ぜGk" -> "交ぜがk"）の復元に使用。
pub fn romaji_to_hiragana(romaji: &str) -> Option<&'static str> {
    let mut lower = [0u8; 8];
    if romaji.is_empty() || romaji.len() > lower.len() {
        return None;
    }
    for (i, b) in romaji.bytes().enumerate() {
        lower[i] = b.to_ascii_lowercase();
    }
    let s = std::str::from_utf8(&lower[..romaji.len()]).ok()?;

    match s {
        // 母音
        "a" => Some("あ"),
        "i" => Some("い"),
        "u" => Some("う"),
        "e" => Some("え"),
        "o" => Some("お"),

        // か行・が行
        "ka" => Some("か"),
        "ki" => Some("き"),
        "ku" => Some("く"),
        "ke" => Some("け"),
        "ko" => Some("こ"),
        "ga" => Some("が"),
        "gi" => Some("ぎ"),
        "gu" => Some("ぐ"),
        "ge" => Some("げ"),
        "go" => Some("ご"),

        // さ行・ざ行
        "sa" => Some("さ"),
        "si" | "shi" => Some("し"),
        "su" => Some("す"),
        "se" => Some("せ"),
        "so" => Some("そ"),
        "za" => Some("ざ"),
        "zi" | "ji" => Some("じ"),
        "zu" => Some("ず"),
        "ze" => Some("ぜ"),
        "zo" => Some("ぞ"),

        // た行・だ行
        "ta" => Some("た"),
        "ti" | "chi" => Some("ち"),
        "tu" | "tsu" => Some("つ"),
        "te" => Some("て"),
        "to" => Some("と"),
        "da" => Some("だ"),
        "di" => Some("ぢ"),
        "du" => Some("づ"),
        "de" => Some("で"),
        "do" => Some("ど"),

        // な行
        "na" => Some("な"),
        "ni" => Some("に"),
        "nu" => Some("ぬ"),
        "ne" => Some("ね"),
        "no" => Some("の"),

        // は行・ば行・ぱ行
        "ha" => Some("は"),
        "hi" => Some("ひ"),
        "hu" | "fu" => Some("ふ"),
        "he" => Some("へ"),
        "ho" => Some("ほ"),
        "ba" => Some("ば"),
        "bi" => Some("び"),
        "bu" => Some("ぶ"),
        "be" => Some("べ"),
        "bo" => Some("ぼ"),
        "pa" => Some("ぱ"),
        "pi" => Some("ぴ"),
        "pu" => Some("ぷ"),
        "pe" => Some("ぺ"),
        "po" => Some("ぽ"),

        // ま行
        "ma" => Some("ま"),
        "mi" => Some("み"),
        "mu" => Some("む"),
        "me" => Some("め"),
        "mo" => Some("も"),

        // や行
        "ya" => Some("や"),
        "yu" => Some("ゆ"),
        "yo" => Some("よ"),

        // ら行
        "ra" => Some("ら"),
        "ri" => Some("り"),
        "ru" => Some("る"),
        "re" => Some("れ"),
        "ro" => Some("ろ"),

        // わ行
        "wa" => Some("わ"),
        "wo" => Some("を"),
        "nn" => Some("ん"),

        // 代表的な拗音
        "kya" => Some("きゃ"), "kyu" => Some("きゅ"), "kyo" => Some("きょ"),
        "gya" => Some("ぎゃ"), "gyu" => Some("ぎゅ"), "gyo" => Some("ぎょ"),
        "sya" | "sha" => Some("しゃ"), "syu" | "shu" => Some("しゅ"), "syo" | "sho" => Some("しょ"),
        "zya" | "ja" | "jya" => Some("じゃ"), "zyu" | "ju" | "jyu" => Some("じゅ"), "zyo" | "jo" | "jyo" => Some("じょ"),
        "tya" | "cha" => Some("ちゃ"), "tyu" | "chu" => Some("ちゅ"), "tyo" | "cho" => Some("ちょ"),
        "nya" => Some("にゃ"), "nyu" => Some("にゅ"), "nyo" => Some("にょ"),
        "hya" => Some("ひゃ"), "hyu" => Some("ひゅ"), "hyo" => Some("ひょ"),
        "bya" => Some("びゃ"), "byu" => Some("びゅ"), "byo" => Some("びょ"),
        "pya" => Some("ぴゃ"), "pyu" => Some("ぴゅ"), "pyo" => Some("ぴょ"),
        "mya" => Some("みゃ"), "myu" => Some("みゅ"), "myo" => Some("みょ"),
        "rya" => Some("りゃ"), "ryu" => Some("りゅ"), "ryo" => Some("りょ"),

        // 連濁・複合語頻出（gaki -> がき 等）
        "gaki" => Some("がき"),
        "kaki" => Some("かき"),

        // 後方互換（大文字子音単体）: "G" -> "が", "K" -> "か" 等
        "g" => Some("が"),
        "k" => Some("か"),
        "s" => Some("さ"),
        "t" => Some("た"),
        "d" => Some("だ"),
        "n" => Some("な"),
        "h" => Some("は"),
        "b" => Some("ば"),
        "p" => Some("ぱ"),
        "m" => Some("ま"),
        "r" => Some("ら"),
        "w" => Some("わ"),
        "y" => Some("や"),
        "z" => Some("ざ"),
        "j" => Some("じゃ"),

        _ => None,
    }
}

/// 大文字子音キー（例: 'G', 'K', 'S'）から平仮名子音・濁音へのマッピング（後方互換用）。
pub fn upper_key_to_kana(upper: char) -> Option<&'static str> {
    let mut buf = [0u8; 4];
    let s = upper.encode_utf8(&mut buf);
    romaji_to_hiragana(s)
}

/// パースされた送りあり見出しの種別
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum OkuriMidashi<'a> {
    /// 通常の送りあり（例: "かk", "きr", "まぜがk"）
    Simple { stem: &'a str, key: char },
    /// 複合語・大文字接尾辞送りあり（例: "交ぜGak", "交ぜGk", "まぜGak"）
    Compound {
        prefix: &'a str,
        romaji: &'a str,
        okuri_key: char,
    },
}

/// 送りあり見出し（通常・大文字複合語）を拡張パースする。
pub fn parse_okuri_midashi_extended(midashi: &str) -> Option<OkuriMidashi<'_>> {
    let alpha_len = midashi.bytes().rev().take_while(|b| b.is_ascii_alphabetic()).count();
    if alpha_len == 0 {
        return None;
    }

    let prefix = &midashi[..midashi.len() - alpha_len];
    if prefix.is_empty() || prefix.is_ascii() {
        return None;
    }

    let alpha = &midashi[midashi.len() - alpha_len..];
    let first = alpha.chars().next()?;
    let last = alpha.chars().last()?;

    // パターン 1: 大文字から始まる複合語接尾辞（例: "交ぜGak", "交ぜGk", "交ぜGAk"）
    if first.is_ascii_uppercase() && alpha_len >= 2 && last.is_ascii_lowercase() {
        let romaji_part = &alpha[..alpha.len() - last.len_utf8()];
        if !romaji_part.is_empty() && romaji_to_hiragana(romaji_part).is_some() {
            return Some(OkuriMidashi::Compound {
                prefix,
                romaji: romaji_part,
                okuri_key: last,
            });
        }
    }

    // パターン 2: 通常の送りあり（例: "かk", "きr", "まぜがk", "おもu"）
    if alpha_len == 1 {
        return Some(OkuriMidashi::Simple {
            stem: prefix,
            key: last,
        });
    }

    None
}

/// SKKの送りあり見出し（例: "かk", "きr", "おもu", "交ぜGak", "交ぜGk"）か判定し、
/// 送りありの場合は `(語幹, 送りキー)` を返す（後方互換用）。
pub fn parse_okuri_midashi(midashi: &str) -> Option<(&str, char)> {
    match parse_okuri_midashi_extended(midashi)? {
        OkuriMidashi::Simple { stem, key } => Some((stem, key)),
        OkuriMidashi::Compound { prefix, okuri_key, .. } => Some((prefix, okuri_key)),
    }
}

/// 照会用の活用形バリエーション
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkuriVariation {
    /// アップストリーム照会用見出し（例: "交ぜがき", "交ぜがく", "かく"）
    pub query_midashi: String,
    /// 語幹抽出用の送り仮名（例: "き", "く"）
    pub okuri_suffix: &'static str,
}

/// 送りあり見出しから、azooKey / yaskkserv2 に照会すべき全平仮名・語幹バリエーションを展開する。
/// 例: "交ぜGak" -> [("交ぜがき", "き"), ("交ぜがく", "く")]
/// 例: "交ぜGk" -> [("交ぜがき", "き"), ("交ぜがく", "く")]
/// 例: "まぜがk" -> [("まぜがき", "き"), ("まぜがく", "く")]
/// 例: "かk"     -> [("かく", "く"), ("かき", "き")]
pub fn expand_okuri_variations(midashi: &str) -> Vec<OkuriVariation> {
    let parsed = match parse_okuri_midashi_extended(midashi) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut variations = Vec::new();

    match parsed {
        OkuriMidashi::Compound { prefix, romaji, okuri_key } => {
            if let Some(upper_kana) = romaji_to_hiragana(romaji) {
                let suffixes = okuri_key_to_suffixes(okuri_key);
                // 複合語・名詞化（例: 交ぜ書き）では連用形「き」を最優先で照会
                let ordered_suffixes: Vec<&'static str> = if suffixes.contains(&"き") {
                    let mut s = vec!["き"];
                    s.extend(suffixes.iter().copied().filter(|&x| x != "き"));
                    s
                } else {
                    suffixes.to_vec()
                };

                for suffix in ordered_suffixes {
                    let mut q = String::with_capacity(prefix.len() + upper_kana.len() + suffix.len());
                    q.push_str(prefix);
                    q.push_str(upper_kana);
                    q.push_str(suffix);
                    variations.push(OkuriVariation {
                        query_midashi: q,
                        okuri_suffix: suffix,
                    });
                }
            }
        }
        OkuriMidashi::Simple { stem, key } => {
            let suffixes = okuri_key_to_suffixes(key);
            // stem の末尾が助詞連濁（「が」「に」など）または2文字以上の複合語の場合、
            // 連用形（「き」〜書き）も優先順位高く生成
            let is_compound = stem.chars().count() >= 3 || stem.ends_with('が') || stem.ends_with('に');
            let ordered_suffixes: Vec<&'static str> = if is_compound && suffixes.contains(&"き") {
                let mut s = vec!["き"];
                s.extend(suffixes.iter().copied().filter(|&x| x != "き"));
                s
            } else {
                suffixes.to_vec()
            };

            for suffix in ordered_suffixes {
                let mut q = String::with_capacity(stem.len() + suffix.len());
                q.push_str(stem);
                q.push_str(suffix);
                variations.push(OkuriVariation {
                    query_midashi: q,
                    okuri_suffix: suffix,
                });
            }
        }
    }

    variations
}

/// 送りあり見出しから、azooKey 照会用の完全な平仮名活用形と送り仮名を復元する（後方互換用）。
/// 例: "かk" -> ("かく", "く")
pub fn expand_okuri_to_full_kana(midashi: &str) -> Option<(String, &'static str)> {
    let vars = expand_okuri_variations(midashi);
    vars.into_iter().next().map(|v| (v.query_midashi, v.okuri_suffix))
}

#[inline]
pub fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}')
}

/// azooKey が返した活用形候補（例: ["交ぜ書き", "書く", "描く", "交ぜ書"]）から、
/// 送り仮名（例: "き", "く"）で終わる語幹、またはすでに語幹化されている候補をゼロコピーで抽出する。
pub fn extract_stem_candidates_borrowed<'a>(
    candidates: &[&'a str],
    okuri_suffix: &str,
    query_midashi: &str,
) -> Vec<&'a str> {
    let mut stems = Vec::with_capacity(candidates.len() * 2);
    let query_char_count = query_midashi.chars().count();
    let suffix_char_count = okuri_suffix.chars().count();
    let stem_char_count = query_char_count.saturating_sub(suffix_char_count);

    for &cand in candidates {
        let clean = clean_candidate(cand);
        if clean.is_empty() {
            continue;
        }

        // 1. 送り仮名（例: "き", "く"）を剥がした語幹（例: "交ぜ書き" -> "交ぜ書", "書く" -> "書"）を抽出
        if let Some(stem) = clean.strip_suffix(okuri_suffix) {
            if !stem.is_empty() && !stems.contains(&stem) {
                stems.push(stem);
            }
            continue;
        }

        // 2. 複合語（語幹仮名長 >= 2）において、候補がすでに語幹そのもの（送り仮名なし）の場合
        // 連用形（"き", "り", "し", "み" 等）の場合のみ語幹名詞（例: "交ぜ書"）を抽出し、
        // 終止形（"く", "る" 等）に対する名詞（例: "交ぜ学"）の誤認混入を確実に防止する。
        let is_renyoukei = matches!(okuri_suffix, "き" | "り" | "し" | "み" | "い" | "ち" | "に" | "び" | "ぎ");
        if is_renyoukei
            && stem_char_count >= 2
            && clean.chars().count() >= 2
            && clean.chars().count() <= stem_char_count
            && let Some(last_char) = clean.chars().last()
            && is_kanji(last_char)
            && !stems.contains(&clean)
        {
            stems.push(clean);
        }
    }

    stems
}

/// azooKey が返した活用形候補から語幹リストを返す（String版）
pub fn extract_stem_candidates(candidates: &[String], okuri_suffix: &str, query_midashi: &str) -> Vec<String> {
    let borrowed: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
    extract_stem_candidates_borrowed(&borrowed, okuri_suffix, query_midashi)
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}

use crate::frequency::clean_candidate;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_okuri_midashi() {
        assert_eq!(parse_okuri_midashi("かk"), Some(("か", 'k')));
        assert_eq!(parse_okuri_midashi("きr"), Some(("き", 'r')));
        assert_eq!(parse_okuri_midashi("おもu"), Some(("おも", 'u')));
        assert_eq!(parse_okuri_midashi("おもU"), Some(("おも", 'U')));
        assert_eq!(parse_okuri_midashi("うつくしi"), Some(("うつくし", 'i')));
        assert_eq!(parse_okuri_midashi("まぜがk"), Some(("まぜが", 'k')));
        assert_eq!(parse_okuri_midashi("交ぜGk"), Some(("交ぜ", 'k')));

        // 拡張パース
        assert_eq!(
            parse_okuri_midashi_extended("交ぜGk"),
            Some(OkuriMidashi::Compound {
                prefix: "交ぜ",
                romaji: "G",
                okuri_key: 'k',
            })
        );
        assert_eq!(
            parse_okuri_midashi_extended("交ぜGak"),
            Some(OkuriMidashi::Compound {
                prefix: "交ぜ",
                romaji: "Ga",
                okuri_key: 'k',
            })
        );
        assert_eq!(parse_okuri_midashi("交ぜGak"), Some(("交ぜ", 'k')));

        // 送りなし見出し
        assert_eq!(parse_okuri_midashi("とうきょう"), None);
        assert_eq!(parse_okuri_midashi("へんかん"), None);
        assert_eq!(parse_okuri_midashi("SKK"), None);
        assert_eq!(parse_okuri_midashi("k"), None);
        assert_eq!(parse_okuri_midashi("Gak"), None);
    }

    #[test]
    fn test_expand_okuri_variations() {
        // 通常の動詞
        let vars_kak = expand_okuri_variations("かk");
        assert_eq!(vars_kak[0].query_midashi, "かく");
        assert_eq!(vars_kak[0].okuri_suffix, "く");

        // 複合語 "まぜがk" -> 連用形 "まぜがき" が最優先
        let vars_mazegak = expand_okuri_variations("まぜがk");
        assert_eq!(vars_mazegak[0].query_midashi, "まぜがき");
        assert_eq!(vars_mazegak[0].okuri_suffix, "き");

        // 複合語大文字 "交ぜGk" -> "交ぜがき" が最優先
        let vars_mazegk = expand_okuri_variations("交ぜGk");
        assert_eq!(vars_mazegk[0].query_midashi, "交ぜがき");
        assert_eq!(vars_mazegk[0].okuri_suffix, "き");

        // 複合語ローマ字 "交ぜGak" -> "交ぜがき" が最優先
        let vars_mazegak_romaji = expand_okuri_variations("交ぜGak");
        assert_eq!(vars_mazegak_romaji[0].query_midashi, "交ぜがき");
        assert_eq!(vars_mazegak_romaji[0].okuri_suffix, "き");
    }

    #[test]
    fn test_extract_stem_candidates() {
        // "かく" に対する azooKey 候補: "書く", "各", "描く", "辛く", "書く;注釈"
        let candidates = vec![
            "書く".to_string(),
            "各".to_string(), // 名詞 -> 除外されるべき
            "描く".to_string(),
            "辛く".to_string(),
            "書く;注釈あり".to_string(), // 重複 -> 重複排除されるべき
        ];
        let stems = extract_stem_candidates(&candidates, "く", "かく");
        assert_eq!(stems, vec!["書", "描", "辛"]);

        // "交ぜがき" に対する azooKey 候補パターン A: "交ぜ書き", "混ぜ書き", "交ぜ餓鬼"
        let comp_cands = vec![
            "交ぜ書き".to_string(),
            "混ぜ書き".to_string(),
            "交ぜ餓鬼".to_string(),
        ];
        let comp_stems = extract_stem_candidates(&comp_cands, "き", "交ぜがき");
        // 語幹 "交ぜ書", "混ぜ書" が抽出され、名詞 "交ぜ餓鬼" は除外される
        assert_eq!(comp_stems, vec!["交ぜ書", "混ぜ書"]);

        // "交ぜがき" に対する azooKey 候補パターン B: すでに語幹化された "交ぜ書", "交ぜ餓鬼", "交ゼが来"
        let pre_stemmed = vec![
            "交ぜ書".to_string(),
            "交ぜ餓鬼".to_string(),
            "交ゼが来".to_string(),
        ];
        let pre_stems = extract_stem_candidates(&pre_stemmed, "き", "交ぜがき");
        assert_eq!(pre_stems, vec!["交ぜ書"]);

        // "交ぜがく" に対する候補: 終止形動詞 "交ぜ書く" から語幹 "交ぜ書" が抽出され、名詞 "交ぜ学", "交是学" は除外される
        let shuushi_cands = vec![
            "交ぜ書く".to_string(),
            "交ぜ学".to_string(),
            "交是学".to_string(),
        ];
        let shuushi_stems = extract_stem_candidates(&shuushi_cands, "く", "交ぜがく");
        assert_eq!(shuushi_stems, vec!["交ぜ書"]);
    }
}
