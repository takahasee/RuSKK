//! SKK送りあり見出し（okuri-ari midashi）の判定・活用復元モジュール。
//! MeCab / ChaSen の活用表規則に基づき、SKKの送りキー（アルファベット）から
//! 終止形・活用形の平仮名を合成し、azooKey から返った候補から語幹（単漢字）を抽出する。

use crate::frequency::clean_candidate;

/// 送りキー（アルファベット1文字）から終止形・連用形・下一段（名詞形）などの送り仮名（平仮名）のリストを返す。
pub fn okuri_key_to_suffixes(key: char) -> &'static [&'static str] {
    match key.to_ascii_lowercase() {
        'k' => &["く", "き", "け"], // カ行五段/下一段 (書く/書き, 受付け/仕分け)
        's' => &["す", "し", "せ"], // サ行五段/下一段 (話す/話し, 問い合わせ/問合せ)
        't' => &["つ", "ち", "て"], // タ行五段/下一段 (待つ/待ち, 割当て/言立て)
        'n' => &["ぬ", "に", "ね"], // ナ行五段/下一段 (死ぬ/死に, 兼ね)
        'm' => &["む", "み", "め"], // マ行五段/下一段 (読む/読み, 早め/詰め)
        'r' => &["る", "り", "れ"], // ラ行五段/一段 (切る/切り, 入れ/垂れ)
        'g' => &["ぐ", "ぎ", "げ"], // ガ行五段/下一段 (泳ぐ/泳ぎ, 売上げ/引き上げ)
        'b' => &["ぶ", "び", "べ"], // バ行五段/下一段 (遊ぶ/遊び, 調べ/並べ)
        'u' | 'w' => &["う", "い", "え"], // ワ行五段 (思う/思い, 買い/会え)
        'i' => &["い", "く", "け"], // 形容詞 (美しい/美しく)
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

/// 語幹（stem）の文字数やパターンに応じて、活用サフィックスの照会優先順位を決定する。
/// - 短語（語幹 1〜2文字、例: "かk", "きr", "よm"）: 五段終止形・連用形を最優先し、既存動作を完全保持。
/// - 複合語（語幹 3文字以上、または助詞連濁、例: "といあわs", "わりあt", "うけつk", "まぜがk"）:
///   下一段・名詞形（"せ", "て", "け", "げ", "れ", "み" 等）を最優先で照会し、一発で「問合せ」「割当て」等を抽出する。
fn order_suffixes_for_stem(stem: &str, key: char, default_suffixes: &'static [&'static str]) -> &'static [&'static str] {
    let is_compound = stem.chars().count() >= 3 || stem.ends_with('が') || stem.ends_with('に');
    if !is_compound {
        return default_suffixes;
    }

    match key.to_ascii_lowercase() {
        's' => &["す", "し", "せ"], // 書き起こす/書起こし (五段), 問い合わせ/問合せ (下一段)
        't' => &["て", "つ", "ち"], // 割り当て/割当て (下一段), 待つ/立ち (五段)
        'k' => &["き", "く", "け"], // 交ぜ書き (連用形), 書く (五段), 受付け (下一段)
        'g' => &["げ", "ぐ", "ぎ"], // 売上げ/引き上げ (下一段), 泳ぐ/騒ぎ (五段)
        'r' => &["る", "り", "れ"], // 切る (五段), 乗り換え (連用形), 引き入れ (下一段)
        'm' => &["み", "む", "め"], // 申込み/申込 (連用形), 読む (五段), 早め/詰め (下一段)
        'b' => &["ぶ", "び", "べ"], // 結ぶ/遊び (五段), 調べ/並べ (下一段)
        'u' | 'w' => &["う", "い", "え"], // 思う (五段), 取扱い/立ち会い (連用形)
        _ => default_suffixes,
    }
}

/// 送りあり見出しから、azooKey / yaskkserv2 に照会すべき全平仮名・語幹バリエーションを展開する。
/// 例: "といあわs" -> [("といあわせ", "せ"), ("といあわし", "し"), ("といあわす", "す")]
/// 例: "交ぜGak"   -> [("交ぜがき", "き"), ("交ぜがけ", "け"), ("交ぜがく", "く")]
/// 例: "交ぜGk"    -> [("交ぜがき", "き"), ("交ぜがけ", "け"), ("交ぜがく", "く")]
/// 例: "まぜがk"   -> [("まぜがき", "き"), ("まぜがけ", "け"), ("まぜがく", "く")]
/// 例: "かk"       -> [("かく", "く"), ("かき", "き"), ("かけ", "け")]
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
                let mut full_stem = String::with_capacity(prefix.len() + upper_kana.len());
                full_stem.push_str(prefix);
                full_stem.push_str(upper_kana);
                let ordered_suffixes = order_suffixes_for_stem(&full_stem, okuri_key, suffixes);

                for &suffix in ordered_suffixes {
                    let mut q = String::with_capacity(full_stem.len() + suffix.len());
                    q.push_str(&full_stem);
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
            let ordered_suffixes = order_suffixes_for_stem(stem, key, suffixes);

            for &suffix in ordered_suffixes {
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
        // 連用形・下一段名詞形（"き", "り", "し", "み", "せ", "て", "け" 等）の場合のみ語幹名詞（例: "交ぜ書", "問合", "受付"）を抽出し、
        // 終止形（"く", "る" 等）に対する名詞（例: "交ぜ学"）の誤認混入を確実に防止する。
        let is_noun_stem = matches!(
            okuri_suffix,
            "き" | "り" | "し" | "み" | "い" | "ち" | "に" | "び" | "ぎ"
                | "せ" | "て" | "け" | "げ" | "れ" | "め" | "べ"
        );
        let clean_char_count = clean.chars().count();
        if is_noun_stem
            && stem_char_count >= 2
            && clean_char_count >= 2
            && clean_char_count <= stem_char_count
            && let Some(last_char) = clean.chars().last()
            && is_kanji(last_char)
            && !stems.contains(&clean)
        {
            stems.push(clean);
        }
    }

    stems
}

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

        // 複合語 "かきおこs" (KakiokoS ->i/u) -> 終止形・連用形・下一段を全網羅
        let vars_kakiokos = expand_okuri_variations("かきおこs");
        assert_eq!(vars_kakiokos[0].query_midashi, "かきおこす");
        assert_eq!(vars_kakiokos[0].okuri_suffix, "す");
        assert_eq!(vars_kakiokos[1].query_midashi, "かきおこし");
        assert_eq!(vars_kakiokos[1].okuri_suffix, "し");
        assert_eq!(vars_kakiokos[2].query_midashi, "かきおこせ");
        assert_eq!(vars_kakiokos[2].okuri_suffix, "せ");

        // 複合語 "といあわs" (ToiawaS ->e) -> 終止形・連用形・下一段を全網羅
        let vars_toiawas = expand_okuri_variations("といあわs");
        assert_eq!(vars_toiawas[0].query_midashi, "といあわす");
        assert_eq!(vars_toiawas[0].okuri_suffix, "す");
        assert_eq!(vars_toiawas[1].query_midashi, "といあわし");
        assert_eq!(vars_toiawas[1].okuri_suffix, "し");
        assert_eq!(vars_toiawas[2].query_midashi, "といあわせ");
        assert_eq!(vars_toiawas[2].okuri_suffix, "せ");

        // 複合語 "わりあt" (WariaT ->e) -> 下一段・名詞形 "わりあて" が最優先
        let vars_wariat = expand_okuri_variations("わりあt");
        assert_eq!(vars_wariat[0].query_midashi, "わりあて");
        assert_eq!(vars_wariat[0].okuri_suffix, "て");
    }

    #[test]
    fn test_extract_stem_candidates_borrowed() {
        // "かく" に対する azooKey 候補: "書く", "各", "描く", "辛く", "書く;注釈"
        let candidates = ["書く", "各", "描く", "辛く", "書く;注釈あり"];
        let stems = extract_stem_candidates_borrowed(&candidates, "く", "かく");
        assert_eq!(stems, vec!["書", "描", "辛"]);

        // "交ぜがき" に対する azooKey 候補パターン A: "交ぜ書き", "混ぜ書き", "交ぜ餓鬼"
        let comp_cands = ["交ぜ書き", "混ぜ書き", "交ぜ餓鬼"];
        let comp_stems = extract_stem_candidates_borrowed(&comp_cands, "き", "交ぜがき");
        assert_eq!(comp_stems, vec!["交ぜ書", "混ぜ書"]);

        // "交ぜがき" に対する azooKey 候補パターン B: すでに語幹化された "交ぜ書", "交ぜ餓鬼", "交ゼが来"
        let pre_stemmed = ["交ぜ書", "交ぜ餓鬼", "交ゼが来"];
        let pre_stems = extract_stem_candidates_borrowed(&pre_stemmed, "き", "交ぜがき");
        assert_eq!(pre_stems, vec!["交ぜ書"]);

        // "交ぜがく" に対する候補: 終止形動詞 "交ぜ書く" から語幹 "交ぜ書" が抽出され、名詞 "交ぜ学", "交是学" は除外される
        let shuushi_cands = ["交ぜ書く", "交ぜ学", "交是学"];
        let shuushi_stems = extract_stem_candidates_borrowed(&shuushi_cands, "く", "交ぜがく");
        assert_eq!(shuushi_stems, vec!["交ぜ書"]);

        // "といあわせ" に対する候補: "問い合わせ", "問合せ", "問い合せ", "問合わせ", "問合"
        // サフィックス "せ" で送り仮名を剥がし、語幹 "問い合わ", "問合", "問い合", "問合わ" が抽出される
        let toiawase_cands = ["問い合わせ", "問合せ", "問い合せ", "問合わせ", "問合"];
        let toiawase_stems = extract_stem_candidates_borrowed(&toiawase_cands, "せ", "といあわせ");
        assert_eq!(toiawase_stems, vec!["問い合わ", "問合", "問い合", "問合わ"]);

        // "わりあて" に対する候補: "割り当て", "割当", "割当て"
        // サフィックス "て" で語幹 "割り当", "割当" が抽出される
        let wariate_cands = ["割り当て", "割当", "割当て"];
        let wariate_stems = extract_stem_candidates_borrowed(&wariate_cands, "て", "わりあて");
        assert_eq!(wariate_stems, vec!["割り当", "割当"]);
    }
}
