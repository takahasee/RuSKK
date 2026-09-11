//! SKK送りあり見出し（okuri-ari midashi）の判定・活用復元モジュール。
//! MeCab / ChaSen の活用表規則に基づき、SKKの送りキー（アルファベット）から
//! 終止形・活用形の平仮名を合成し、azooKey から返った候補から語幹（単漢字）を抽出する。

/// 送りキー（アルファベット1文字）から終止形などの送り仮名（平仮名）を返すマッピング。
/// 五段活用、一段活用、形容詞の代表的活用語尾に対応。
pub fn okuri_key_to_suffix(key: char) -> Option<&'static str> {
    match key.to_ascii_lowercase() {
        'k' => Some("く"), // カ行五段 (書く, 咲く, 動く, 描く, 働く)
        's' => Some("す"), // サ行五段 (話す, 差す, 押す, 出す, 直す)
        't' => Some("つ"), // タ行五段 (待つ, 立つ, 勝つ, 保つ, 持つ)
        'n' => Some("ぬ"), // ナ行五段 (死ぬ)
        'm' => Some("む"), // マ行五段 (読む, 飲む, 休む, 頼む, 噛む)
        'r' => Some("る"), // ラ行五段 / 一段 (切る, 着る, 走る, 食べる, 見る)
        'g' => Some("ぐ"), // ガ行五段 (泳ぐ, 脱ぐ, 防ぐ, 騒ぐ)
        'b' => Some("ぶ"), // バ行五段 (遊ぶ, 飛ぶ, 呼ぶ, 結ぶ, 喜ぶ)
        'u' | 'w' => Some("う"), // ワ行五段 (思う, 買う, 言う, 会う, 使う)
        'i' => Some("い"), // 形容詞 (美しい, 高い, 広い, 赤い)
        'd' => Some("だ"), // 形容動詞
        _ => None,
    }
}

/// SKKの送りあり見出し（例: "かk", "きr", "おもu"）か判定し、
/// 送りありの場合は `(語幹, 送りキー)` を返す。
pub fn parse_okuri_midashi(midashi: &str) -> Option<(&str, char)> {
    let mut chars = midashi.chars().rev();
    let last = chars.next()?;
    let prev = chars.next()?;

    // 末尾が ASCII アルファベットで、その直前が非ASCII文字（ひらがな等）の場合に送りありと判定
    if last.is_ascii_alphabetic() && !prev.is_ascii() {
        let stem_len = midashi.len() - last.len_utf8();
        Some((&midashi[..stem_len], last))
    } else {
        None
    }
}

/// 送りあり見出しから、azooKey 照会用の完全な平仮名活用形と送り仮名を復元する。
/// 例: "かk" -> ("かく", "く")
/// 例: "きr" -> ("きる", "る")
/// 例: "おもu" -> ("おもう", "う")
pub fn expand_okuri_to_full_kana(midashi: &str) -> Option<(String, &'static str)> {
    let (stem, key) = parse_okuri_midashi(midashi)?;
    let suffix = okuri_key_to_suffix(key)?;
    let mut full = String::with_capacity(stem.len() + suffix.len());
    full.push_str(stem);
    full.push_str(suffix);
    Some((full, suffix))
}

/// azooKey が返した活用形候補（例: ["書く", "各", "描く", "辛く"]）から、
/// 送り仮名（例: "く"）で終わる候補のみを抽出し、送り仮名を剥がして語幹（単漢字）リストを返す。
/// 送り仮名を持たない候補（例: 名詞 "各"）は自動的に除外される。
pub fn extract_stem_candidates(candidates: &[String], okuri_suffix: &str) -> Vec<String> {
    let mut stems = Vec::with_capacity(candidates.len());
    let mut seen = std::collections::HashSet::new();

    for cand in candidates {
        let clean = clean_candidate(cand);
        if let Some(stem) = clean.strip_suffix(okuri_suffix)
            && !stem.is_empty()
            && seen.insert(stem.to_string())
        {
            stems.push(stem.to_string());
        }
    }

    stems
}

fn clean_candidate(cand: &str) -> &str {
    cand.split(';').next().unwrap_or(cand).trim()
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

        // 送りなし見出し
        assert_eq!(parse_okuri_midashi("とうきょう"), None);
        assert_eq!(parse_okuri_midashi("へんかん"), None);
        assert_eq!(parse_okuri_midashi("SKK"), None);
        assert_eq!(parse_okuri_midashi("k"), None);
    }

    #[test]
    fn test_expand_okuri_to_full_kana() {
        assert_eq!(expand_okuri_to_full_kana("かk"), Some(("かく".to_string(), "く")));
        assert_eq!(expand_okuri_to_full_kana("よm"), Some(("よむ".to_string(), "む")));
        assert_eq!(expand_okuri_to_full_kana("きr"), Some(("きる".to_string(), "る")));
        assert_eq!(expand_okuri_to_full_kana("はなs"), Some(("はなす".to_string(), "す")));
        assert_eq!(expand_okuri_to_full_kana("おもu"), Some(("おもう".to_string(), "う")));
        assert_eq!(expand_okuri_to_full_kana("おもU"), Some(("おもう".to_string(), "う")));
        assert_eq!(expand_okuri_to_full_kana("うつくしi"), Some(("うつくしい".to_string(), "い")));

        assert_eq!(expand_okuri_to_full_kana("とうきょう"), None);
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
        let stems = extract_stem_candidates(&candidates, "く");
        assert_eq!(stems, vec!["書", "描", "辛"]);
    }
}
