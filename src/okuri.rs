//! SKK送りあり見出し（okuri-ari midashi）の判定・活用復元モジュール。
//! MeCab / ChaSen の活用表規則に基づき、SKKの送りキー（アルファベット）から
//! 終止形・活用形の平仮名を合成し、azooKey から返った候補から語幹（単漢字）を抽出する。

use crate::frequency::clean_candidate;

/// 送りキー（アルファベット1文字）から終止形・連用形・下一段（名詞形）などの送り仮名（平仮名）のリストを返す。
pub fn okuri_key_to_suffixes(key: char) -> &'static [&'static str] {
    match key.to_ascii_lowercase() {
        'k' => &["く", "き", "け", "かった", "くて", "きゃ"], // カ行五段/下一段/形容詞過去 (書く/書き, 受付け, 高かった/高くて, 行かなきゃ)
        's' => &["す", "し", "せ", "した", "して", "しゃ"], // サ行五段/下一段 (話す/話し/話した/話して, 問い合わせ/問合せ, 話しゃ)
        't' => &["った", "って", "て", "つ", "ち"], // タ行五段促音便/下一段 (思った/思って, 待つ/待ち, 割当て/言立て)
        'n' => &["ない", "なきゃ", "なくちゃ", "ぬ", "ね", "に", "にゃ", "んだ", "んで"], // ナ行五段/下一段/否定 (少ない/行かない/行かなきゃ, 死ぬ/死に/死んだ, 兼ね)
        'm' => &["む", "み", "め", "んだ", "んで", "みゃ", "もう"], // マ行五段/下一段/撥音便 (読む/読み/読んだ/読んで, 読もう, 読みゃ)
        'r' => &["る", "り", "れ", "りゃ", "ろう"], // ラ行五段/一段 (切る/切り/切ろう, 入れ/垂れ, 切りゃ)
        'g' => &["ぐ", "ぎ", "げ", "いだ", "いで", "ぎゃ", "ごう"], // ガ行五段/下一段/イ音便 (泳ぐ/泳ぎ/泳いだ/泳いで, 売上げ, 泳ごう, 泳ぎゃ)
        'b' => &["ぶ", "び", "べ", "んだ", "んで", "びゃ", "ぼう"], // バ行五段/下一段/撥音便 (遊ぶ/遊び/遊んだ/遊んで, 調べ, 遊ぼう, 遊びゃ)
        'u' | 'w' => &["う", "い", "え", "った", "って", "おう"], // ワ行五段/促音便 (思う/思い/思った/思って, 買い/会え, 思おう)
        'e' => &["え", "える", "う", "えた", "えて", "えば", "えれば"], // 下一段/仮定・命令形/ワ行五段 (使え/教え/考え/答え, 使える/教える, 使う, 使えた, 使えて, 使えば, 考えれば)
        'o' => &["おう", "お", "う", "よう"], // 意志形/オ段 (使おう/思おう/買おう, 使う, 見よう)
        'a' => &["わない", "わ", "う", "ない", "わせる", "われる", "わず"], // 未然形/使役/受身 (使わない/思わない, 使う, 教えない, 使わせる, 使われる, 使わず)
        'i' => &["い", "う", "く", "け", "かった", "くて", "ければ", "った", "って"], // 形容詞/連用形/促音便 (美しい/美しく/美しかった/美しくて/美しければ, 使い/使う/使った/使って)
        'd' => &["だ", "で", "んだ", "んで"], // 形容動詞/撥音便 (読んだ/遊んだ)
        'c' => &["っちゃう", "っちゃった", "ちゃう", "ちゃった", "ちまう", "ちまった", "ち"], // 拗音・促音縮約 (笑っちゃう/思っちゃう/書いちゃう/ちまう)
        'y' => &["よう", "や", "ゆ"], // 拗音・意志・推量 (食べよう/見よう/始めよう, 言や)
        'j' => &["じる", "じ", "じゃう", "じゃった", "じゃ"], // ザ行・拗音 (論じる/信じる/読んじゃう)
        'p' => &["っぽい", "ぽい"], // 半濁音・促音接尾辞 (安っぽい/無理っぽい/理屈っぽい)
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
    /// アスタリスク明示送り仮名（例: "ねあ*げ", "まぜ*がき", "ねあ*g"）
    Explicit {
        prefix: &'a str,
        suffix: &'a str,
    },
}

/// 送りあり見出し（通常・大文字複合語・アスタリスク明示）を拡張パースする。
pub fn parse_okuri_midashi_extended(midashi: &str) -> Option<OkuriMidashi<'_>> {
    // パターン 0: アスタリスク明示区切り（例: "ねあ*げ", "まぜ*がき", "ねあ*g"）
    if let Some(star_idx) = midashi.find('*') {
        let prefix = &midashi[..star_idx];
        let suffix = &midashi[star_idx + 1..];
        if !prefix.is_empty() && !suffix.is_empty() {
            // 末尾が1文字のアルファベットなら Simple として扱う
            if suffix.len() == 1 && suffix.chars().next().unwrap().is_ascii_alphabetic() {
                return Some(OkuriMidashi::Simple {
                    stem: prefix,
                    key: suffix.chars().next().unwrap().to_ascii_lowercase(),
                });
            }
            return Some(OkuriMidashi::Explicit { prefix, suffix });
        }
    }

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
    if first.is_ascii_uppercase() && alpha_len >= 2 {
        let romaji_part = &alpha[..alpha.len() - last.len_utf8()];
        if !romaji_part.is_empty() && romaji_to_hiragana(romaji_part).is_some() {
            return Some(OkuriMidashi::Compound {
                prefix,
                romaji: romaji_part,
                okuri_key: last.to_ascii_lowercase(),
            });
        }
    }

    // パターン 2: 通常の送りあり（例: "かk", "きr", "まぜがk", "おもu", "ねあg", "ねあG"）
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
        OkuriMidashi::Explicit { prefix, suffix } => {
            let key = suffix.chars().next()?.to_ascii_lowercase();
            Some((prefix, key))
        }
    }
}

use std::borrow::Cow;

/// 見出し語（例: "てだR", "てだ*れ", "かk"）を正規化された小文字の送りあり見出し（例: "てだr", "かk"）に変換する。
/// 送りありでない場合は元の文字列のスライス借用またはクローンを返す。
pub fn normalize_okuri_midashi_key(midashi: &str) -> Cow<'_, str> {
    if let Some((stem, key)) = parse_okuri_midashi(midashi) {
        let mut s = String::with_capacity(stem.len() + 1);
        s.push_str(stem);
        s.push(key.to_ascii_lowercase());
        Cow::Owned(s)
    } else {
        Cow::Borrowed(midashi)
    }
}

/// 照会用の活用形バリエーション
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OkuriVariation {
    /// アップストリーム照会用見出し（例: "交ぜがき", "交ぜがく", "かく"）
    pub query_midashi: String,
    /// 語幹抽出用の送り仮名（例: "き", "く"）
    pub okuri_suffix: String,
}

/// 語幹（stem）の文字数に応じて、活用サフィックスの照会優先順位を決定する。
/// - 短語（語幹 1文字、例: "かk", "きr", "よm", "まt", "およg"）: 五段終止形・連用形を最優先し、余計な名詞の混入を防止。
/// - 複合語・多音節語（語幹 2文字以上、例: "といあわs", "わりあt", "うけつk", "まぜがk", "ねあg", "みおk", "もうしこm", "しらb", "うりきr", "とりあつかw"）:
///   全子音（k, s, t, n, m, r, g, b, u/w, d, c, y, j, p）において、下一段・連用形名詞形（"せ", "て", "け", "げ", "れ", "み", "り", "ぎ", "び", "い", "ね", "べ" 等）と
///   終止形・促音便・音便をバランスよく均質に照会し、一発で「問合せ」「割当て」「値上げ」「見送り」「受入れ」「申込み」「取扱い」「調べ」等を抽出する。
fn order_suffixes_for_stem(stem: &str, key: char, default_suffixes: &'static [&'static str]) -> &'static [&'static str] {
    let char_count = stem.chars().count();
    let is_compound = char_count >= 2;
    if !is_compound {
        return default_suffixes;
    }

    match key.to_ascii_lowercase() {
        'k' => &["き", "く", "け", "かった", "くて"], // 交ぜ書き/見送り (連用形), 書く/届く (終止形), 受付け/助け (下一段), 高かった (形容詞過去)
        's' => &["す", "し", "せ", "した", "して"], // 書き起こす/言い出す (終止形), 書き起こし/言い出し (連用形), 問い合わせ/合わせ (下一段)
        't' => {
            if char_count >= 3 {
                &["て", "つ", "ち", "った", "って"] // 割り当て/引き当て (下一段最優先)
            } else {
                &["った", "って", "て", "つ", "ち"] // 思った/待った (促音便最優先)
            }
        }
        'n' => &["ね", "に", "ぬ", "ない", "んだ", "んで"], // 束ね/兼ね (下一段), 死に (連用形), 死ぬ (終止形), 読まない (否定), 死んだ (撥音便)
        'm' => &["み", "む", "め", "んだ", "んで"], // 申込み/頼み (連用形), 読む/含む (終止形), 早め/詰め (下一段), 読んだ (撥音便)
        'r' => &["り", "る", "れ", "った", "って"], // 見送り/乗り/売り (連用形), 切る/乗る/売る (終止形), 受け入れ/乗り換え (下一段), 切った (促音便)
        'g' => &["げ", "ぎ", "ぐ", "いだ", "いで"], // 値上げ/売上げ/引き上げ (下一段), 泳ぎ/騒ぎ (連用形), 泳ぐ/騒ぐ (終止形), 泳いだ (イ音便)
        'b' => &["び", "ぶ", "べ", "んだ", "んで"], // 遊び/飛び (連用形), 遊ぶ/飛ぶ (終止形), 調べ/並べ (下一段), 遊んだ (撥音便)
        'u' | 'w' => &["い", "う", "え", "った", "って"], // 取扱い/思い (連用形), 扱う/思う (終止形), 迎え/訴え (下一段), 思った (促音便)
        'e' => &["え", "える", "う", "えた", "えて", "えれば"], // 使え/教え/考え (下一段名詞/命令), 使える/教える (下一段終止), 使う (五段終止), 考えれば
        'o' => &["おう", "お", "う", "よう"], // 使おう/思おう
        'a' => &["わない", "わ", "う", "ない", "わせる", "われる", "わず"], // 使わない/思わない/使わせる
        'i' => &["い", "う", "く", "け", "かった", "くて", "ければ"], // 美しい/美しく/使い/使う
        'd' => &["で", "だ", "んだ", "んで"], // 読んだ/遊んだ
        'c' => &["っちゃう", "ちゃう", "ちまう", "ち"], // 笑っちゃう/追っ払っちゃう
        'y' => &["よう", "や", "ゆ"], // 繰り広げよう/食べよう
        'j' => &["じ", "じる", "じゃう"], // やり損じ/信じる
        'p' => &["っぽい", "ぽい"], // 安っぽい/無理っぽい
        _ => default_suffixes,
    }
}

/// 送りあり見出しから、azooKey に照会すべき平仮名・語幹バリエーションを展開する。
/// 例: "といあわs" -> [("といあわす", "す"), ("といあわし", "し"), ("といあわせ", "せ")]
/// 例: "交ぜGak"   -> [("交ぜがき", "き"), ("交ぜがく", "く"), ("交ぜがけ", "け")]
/// 例: "交ぜGk"    -> [("交ぜがき", "き"), ("交ぜがく", "く"), ("交ぜがけ", "け")]
/// 例: "まぜがk"   -> [("まぜがき", "き"), ("まぜがく", "く"), ("まぜがけ", "け")]
/// 例: "かk"       -> [("かく", "く"), ("かき", "き"), ("かけ", "け")]
/// 例: "ねあ*げ"   -> [("ねあげ", "げ")]
pub fn expand_okuri_variations(midashi: &str) -> Vec<OkuriVariation> {
    let parsed = match parse_okuri_midashi_extended(midashi) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let mut variations = Vec::new();

    match parsed {
        OkuriMidashi::Explicit { prefix, suffix } => {
            let mut q = String::with_capacity(prefix.len() + suffix.len());
            q.push_str(prefix);
            q.push_str(suffix);
            variations.push(OkuriVariation {
                query_midashi: q,
                okuri_suffix: suffix.to_string(),
            });
        }
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
                        okuri_suffix: suffix.to_string(),
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
                    okuri_suffix: suffix.to_string(),
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

/// 抽出された語幹候補が正当か（部分変換の残骸や無意味な平仮名末尾でないか）を判定する。
/// - 短語幹（かな長 1文字、例: "かk" -> "書", "きr" -> "切"）: 漢字を1文字以上含むこと。
/// - 複合語幹（かな長 2文字以上、例: "てだr", "みおk", "うけいr"）:
///   1) 末尾が漢字であること（例: "手練", "見送", "受け入"）。
///   2) 漢字が2文字以上含まれること（例: "問い合わ", "言い合わ"）。
///   3) 漢字が1文字以上で、かつ末尾が一段動詞・形容詞の語幹語尾（い段・え段、例: "食べ", "教え", "調べ", "美し"）であること。
///
/// ※現代日本語の活用規則上、あ段・う段・お段・んで終わる用言語幹は存在しないため、
/// 「てだり」->「手だ」（末尾: あ段「だ」）のような未知語の部分変換残骸を確実に排除する。
#[inline]
pub fn is_valid_stem(stem: &str, stem_char_count: usize) -> bool {
    if stem.is_empty() {
        return false;
    }
    if stem_char_count < 2 {
        return stem.chars().any(is_kanji);
    }
    if let Some(last) = stem.chars().last() {
        if is_kanji(last) {
            return true;
        }
        let kanji_count = stem.chars().filter(|&c| is_kanji(c)).count();
        if kanji_count >= 2 {
            return true;
        }
        if kanji_count >= 1 {
            // い段・え段のみ用言語幹（上一段・下一段・形容詞）として許容
            return matches!(
                last,
                'い' | 'き' | 'し' | 'ち' | 'に' | 'ひ' | 'み' | 'り' | 'ぎ' | 'じ' | 'び' | 'ぴ'
                | 'え' | 'け' | 'せ' | 'て' | 'ね' | 'へ' | 'め' | 'れ' | 'げ' | 'ぜ' | 'で' | 'べ' | 'ぺ'
            );
        }
    }
    false
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
            if is_valid_stem(stem, stem_char_count) && !stems.contains(&stem) {
                stems.push(stem);
            }
            continue;
        }

        // 2. 複合語（語幹仮名長 >= 2）において、候補がすでに語幹そのもの（送り仮名なし）の場合
        // 連用形名詞（い段）および下一段名詞（え段）の場合のみ語幹名詞（例: "交ぜ書", "問合", "受付", "値上", "受入", "見送"）を抽出し、
        // 終止形（"く", "る" 等）に対する名詞（例: "交ぜ学"）の誤認混入を確実に防止する。
        let is_noun_stem = matches!(
            okuri_suffix,
            // い段（連用形名詞形）
            "き" | "し" | "ち" | "に" | "ひ" | "み" | "り" | "ぎ" | "じ" | "び" | "い"
                // え段（下一段名詞形）
                | "け" | "せ" | "て" | "ね" | "へ" | "め" | "れ" | "げ" | "ぜ" | "で" | "べ" | "え"
        );
        let clean_char_count = clean.chars().count();
        if is_noun_stem
            && stem_char_count >= 2
            && clean_char_count >= 2
            && clean_char_count <= stem_char_count
            && is_valid_stem(clean, stem_char_count)
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

        // アスタリスク明示送り仮名（画面表示マーカー対応）
        assert_eq!(
            parse_okuri_midashi_extended("ねあ*げ"),
            Some(OkuriMidashi::Explicit {
                prefix: "ねあ",
                suffix: "げ",
            })
        );
        assert_eq!(
            parse_okuri_midashi_extended("ねあ*g"),
            Some(OkuriMidashi::Simple {
                stem: "ねあ",
                key: 'g',
            })
        );
        assert_eq!(parse_okuri_midashi("ねあ*げ"), Some(("ねあ", 'げ')));
        assert_eq!(parse_okuri_midashi("ねあG"), Some(("ねあ", 'G')));

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

        // 短語五段動詞促音便 "おもt" (OmoT ->ta/te) -> 促音便 "おもった", "おもって" が最優先
        let vars_omot = expand_okuri_variations("おもt");
        assert_eq!(vars_omot[0].query_midashi, "おもった");
        assert_eq!(vars_omot[0].okuri_suffix, "った");
        assert_eq!(vars_omot[1].query_midashi, "おもって");
        assert_eq!(vars_omot[1].okuri_suffix, "って");

        // 拗音・縮約 "わらc" (WaraChau) -> "わらっちゃう", "わらっちゃった" が最優先
        let vars_warac = expand_okuri_variations("わらc");
        assert_eq!(vars_warac[0].query_midashi, "わらっちゃう");
        assert_eq!(vars_warac[0].okuri_suffix, "っちゃう");

        // 拗音・意志 "たべy" (TabeYou) -> "たべよう" が最優先
        let vars_tabey = expand_okuri_variations("たべy");
        assert_eq!(vars_tabey[0].query_midashi, "たべよう");
        assert_eq!(vars_tabey[0].okuri_suffix, "よう");

        // 形容詞過去 "たかk" (TakaK ->atta) -> 終止形 "たかく" に加え "たかかった" も展開
        let vars_takak = expand_okuri_variations("たかk");
        assert!(vars_takak.iter().any(|v| v.query_midashi == "たかく"));
        assert!(vars_takak.iter().any(|v| v.query_midashi == "たかかった" && v.okuri_suffix == "かった"));

        // 複合語 "ねあg" (NeaG ->e) -> 下一段 "ねあげ" ("げ") が最優先
        let vars_neag = expand_okuri_variations("ねあg");
        assert_eq!(vars_neag[0].query_midashi, "ねあげ");
        assert_eq!(vars_neag[0].okuri_suffix, "げ");

        // 全子音の複合語テスト（k, s, t, n, m, r, g, b, w/u）
        // k: "みおk" (見送り/見送る)
        let vars_miok = expand_okuri_variations("みおk");
        assert!(vars_miok.iter().any(|v| v.query_midashi == "みおき"));
        assert!(vars_miok.iter().any(|v| v.query_midashi == "みおく"));

        // s: "いいだs" (言い出し/言い出す)
        let vars_iidas = expand_okuri_variations("いいだs");
        assert!(vars_iidas.iter().any(|v| v.query_midashi == "いいだし"));
        assert!(vars_iidas.iter().any(|v| v.query_midashi == "いいだす"));

        // t: "ひきあt" (引き当て/引き当てる)
        let vars_hikiat = expand_okuri_variations("ひきあt");
        assert!(vars_hikiat.iter().any(|v| v.query_midashi == "ひきあて"));

        // r: "うけいr" (受け入れ/受け入れる)
        let vars_ukeir = expand_okuri_variations("うけいr");
        assert!(vars_ukeir.iter().any(|v| v.query_midashi == "うけいれ"));
        assert!(vars_ukeir.iter().any(|v| v.query_midashi == "うけいる"));

        // m: "もうしこm" (申込み/申し込む)
        let vars_moushikom = expand_okuri_variations("もうしこm");
        assert!(vars_moushikom.iter().any(|v| v.query_midashi == "もうしこみ"));
        assert!(vars_moushikom.iter().any(|v| v.query_midashi == "もうしこむ"));

        // b: "しらb" (調べ/調べる)
        let vars_shirab = expand_okuri_variations("しらb");
        assert!(vars_shirab.iter().any(|v| v.query_midashi == "しらべ"));

        // w: "とりあつかw" (取扱い/取り扱う)
        let vars_toriatsukaw = expand_okuri_variations("とりあつかw");
        assert!(vars_toriatsukaw.iter().any(|v| v.query_midashi == "とりあつかい"));
        assert!(vars_toriatsukaw.iter().any(|v| v.query_midashi == "とりあつかう"));

        // e: "つかe" (使え/使える/使う)
        let vars_tsukae = expand_okuri_variations("つかe");
        assert_eq!(vars_tsukae[0].query_midashi, "つかえ");
        assert_eq!(vars_tsukae[0].okuri_suffix, "え");
        assert!(vars_tsukae.iter().any(|v| v.query_midashi == "つかえる"));
        assert!(vars_tsukae.iter().any(|v| v.query_midashi == "つかう"));
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

        // "うけいれ" に対する候補: "受け入れ", "受入れ", "受入"
        let ukeire_cands = ["受け入れ", "受入れ", "受入"];
        let ukeire_stems = extract_stem_candidates_borrowed(&ukeire_cands, "れ", "うけいれ");
        assert_eq!(ukeire_stems, vec!["受け入", "受入"]);

        // "もうしこみ" に対する候補: "申込み", "申込", "申し込み"
        let moushikomi_cands = ["申込み", "申込", "申し込み"];
        let moushikomi_stems = extract_stem_candidates_borrowed(&moushikomi_cands, "み", "もうしこみ");
        assert_eq!(moushikomi_stems, vec!["申込", "申し込"]);

        // "とりあつかい" に対する候補: "取扱い", "取扱", "取り扱い"
        let toriatsukai_cands = ["取扱い", "取扱", "取り扱い"];
        let toriatsukai_stems = extract_stem_candidates_borrowed(&toriatsukai_cands, "い", "とりあつかい");
        assert_eq!(toriatsukai_stems, vec!["取扱", "取り扱"]);

        // 促音便動詞 "おもった" / "おもって" からの語幹抽出
        let omotta_cands = ["思った", "重った"];
        let omotta_stems = extract_stem_candidates_borrowed(&omotta_cands, "った", "おもった");
        assert_eq!(omotta_stems, vec!["思", "重"]);

        let omotte_cands = ["思って", "想って"];
        let omotte_stems = extract_stem_candidates_borrowed(&omotte_cands, "って", "おもって");
        assert_eq!(omotte_stems, vec!["思", "想"]);

        // "きった" からサフィックス "った" で語幹 "切" が抽出され、同音名詞 "橘田" は自然に除外される
        let kitta_cands = ["切った", "橘田"];
        let kitta_stems = extract_stem_candidates_borrowed(&kitta_cands, "った", "きった");
        assert_eq!(kitta_stems, vec!["切"]);

        // 拗音動詞 "わらっちゃう" からの語幹抽出
        let warac_cands = ["笑っちゃう", "嗤っちゃう"];
        let warac_stems = extract_stem_candidates_borrowed(&warac_cands, "っちゃう", "わらっちゃう");
        assert_eq!(warac_stems, vec!["笑", "嗤"]);

        // 意志形 "たべよう" からの語幹抽出
        let tabey_cands = ["食べよう"];
        let tabey_stems = extract_stem_candidates_borrowed(&tabey_cands, "よう", "たべよう");
        assert_eq!(tabey_stems, vec!["食べ"]);

        // 形容詞過去 "たかかった" からの語幹抽出
        let takak_cands = ["高かった"];
        let takak_stems = extract_stem_candidates_borrowed(&takak_cands, "かった", "たかかった");
        assert_eq!(takak_stems, vec!["高"]);

        // "つかえ" に対する azooKey 候補 "使え", "仕え", "遣え", "支え" から語幹抽出
        let tsukae_cands = ["使え", "仕え", "遣え", "支え", "事え", "痞", "痞え"];
        let tsukae_stems = extract_stem_candidates_borrowed(&tsukae_cands, "え", "つかえ");
        assert_eq!(tsukae_stems, vec!["使", "仕", "遣", "支", "事", "痞"]);
    }
}
