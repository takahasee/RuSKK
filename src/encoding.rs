use std::borrow::Cow;
use encoding_rs::EUC_JP;

/// Decode bytes that may be EUC-JP or already UTF-8 into a UTF-8 String or slice.
/// Returns Cow::Borrowed for UTF-8 bytes to avoid unnecessary heap allocation.
pub fn decode_euc_or_utf8(bytes: &[u8]) -> Cow<'_, str> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Cow::Borrowed(s);
    }
    let (cow, _, _) = EUC_JP.decode(bytes);
    cow
}

/// Encode a UTF-8 string to EUC-JP bytes. Unmappable chars are replaced.
pub fn encode_euc_jp(text: &str) -> Vec<u8> {
    let (cow, _, _) = EUC_JP.encode(text);
    cow.into_owned()
}

/// Convert an skkserv response body from EUC-JP to UTF-8 bytes.
/// Protocol framing (`1`, `4`, `/`, spaces, newlines) is ASCII and preserved.
pub fn response_euc_to_utf8(response: &[u8]) -> Vec<u8> {
    if response.is_empty() {
        return Vec::new();
    }
    // すでに UTF-8（ASCII のみを含む場合など）であれば、そのまま返す
    if std::str::from_utf8(response).is_ok() {
        return response.to_vec();
    }
    // EUC-JP のマルチバイトコード (0xA1..=0xFE) は ASCII 記号 (0x00..=0x7F: '/', '1', '4', '\n') と
    // バイト値が衝突しないため、一括デコードするだけで区切り文字を完全に維持した UTF-8 バイト列が得られる。
    let (cow, _, _) = EUC_JP.decode(response);
    cow.as_bytes().to_vec()
}

/// レスポンスバイト列から第1候補（最初の / と / の間）をゼロアロケーションで抽出
pub fn extract_first_candidate(response: &[u8]) -> Option<&str> {
    if !response.starts_with(b"1/") {
        return None;
    }
    let rest = &response[2..];
    let end = rest.iter().position(|&b| b == b'/' || b == b'\n')?;
    let first = &rest[..end];
    if first.is_empty() {
        return None;
    }
    std::str::from_utf8(first).ok()
}

/// レスポンスバイト列から借用スライスの候補リスト（ゼロコピー）を抽出
pub fn parse_candidates_borrowed(response: &[u8]) -> Vec<&str> {
    if !response.starts_with(b"1") {
        return Vec::new();
    }
    let body = response[1..].strip_suffix(b"\n").unwrap_or(&response[1..]);
    body.split(|&b| b == b'/')
        .skip(1)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok())
        .collect()
}

/// レスポンスバイト列から候補リストをパースする。
/// SKKの送りあり辞書エントリ（例: `1/[っ/思/]/重/御持/想;注釈/`）に含まれる
/// 角括弧ブロック `[送り仮名/候補...]` も適切に展開し、語幹候補を抽出する。
pub fn parse_candidates_unpacked(response: &[u8]) -> Vec<String> {
    if !response.starts_with(b"1") {
        return Vec::new();
    }
    let body = response[1..].strip_suffix(b"\n").unwrap_or(&response[1..]);
    let Ok(body_str) = std::str::from_utf8(body) else {
        let (cow, _, _) = encoding_rs::EUC_JP.decode(body);
        return parse_candidates_from_str(&cow);
    };
    parse_candidates_from_str(body_str)
}

fn parse_candidates_from_str(body_str: &str) -> Vec<String> {
    let mut cands = Vec::new();
    let mut in_block = false;

    for part in body_str.split('/') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if part.starts_with('[') {
            in_block = true;
            // `[okuri` または `[[okuri` などのヘッダをスキップ
            continue;
        }

        if in_block {
            if let Some(cand) = part.strip_suffix(']') {
                let cand = cand.trim();
                if !cand.is_empty() && !cands.contains(&cand.to_string()) {
                    cands.push(cand.to_string());
                }
                in_block = false;
            } else if !part.is_empty() && !cands.contains(&part.to_string()) {
                cands.push(part.to_string());
            }
        } else if !cands.contains(&part.to_string()) {
            cands.push(part.to_string());
        }
    }
    cands
}

/// Format a list of borrowed candidate strings into an skkserv response.
pub fn format_candidates_response_str(candidates: &[&str]) -> Vec<u8> {
    if candidates.is_empty() {
        return b"4\n".to_vec();
    }
    let total_len: usize = candidates.iter().map(|c| c.len() + 1).sum();
    let mut out = Vec::with_capacity(total_len + 3);
    out.push(b'1');
    for cand in candidates {
        out.push(b'/');
        out.extend_from_slice(cand.as_bytes());
    }
    out.push(b'/');
    out.push(b'\n');
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn euc_roundtrip_ascii() {
        let s = "hello";
        assert_eq!(decode_euc_or_utf8(&encode_euc_jp(s)), s);
    }

    #[test]
    fn response_candidates_euc_to_utf8() {
        // "あ" in EUC-JP is A4 A2
        let mut resp = vec![b'1', b'/'];
        resp.extend_from_slice(&[0xA4, 0xA2]);
        resp.extend_from_slice(b"/\n");
        let utf8 = response_euc_to_utf8(&resp);
        assert_eq!(utf8, b"1/\xe3\x81\x82/\n");
    }

    #[test]
    fn response_not_found_plain() {
        assert_eq!(response_euc_to_utf8(b"4\n"), b"4\n");
    }

    #[test]
    fn decode_midashi_euc_jp() {
        // "あい" in EUC-JP
        let midashi = [0xA4, 0xA2, 0xA4, 0xA4];
        assert_eq!(decode_euc_or_utf8(&midashi), "あい");
    }

    #[test]
    fn test_merge_candidates_borrowed() {
        let primary = vec!["あい", "愛"];
        let fallback = vec!["愛", "相", "藍"];
        let mut merged = primary;
        for cand in fallback {
            if !merged.contains(&cand) {
                merged.push(cand);
            }
        }
        assert_eq!(merged, vec!["あい", "愛", "相", "藍"]);

        let formatted = format_candidates_response_str(&merged);
        assert_eq!(formatted, "1/あい/愛/相/藍/\n".as_bytes());

        let parsed = parse_candidates_borrowed(&formatted);
        assert_eq!(parsed, merged);
    }

    #[test]
    fn test_extract_first_candidate() {
        assert_eq!(
            extract_first_candidate("1/東京/Tokyo/tokyo/\n".as_bytes()),
            Some("東京")
        );
        assert_eq!(
            extract_first_candidate("1/服/\n".as_bytes()),
            Some("服")
        );
        assert_eq!(extract_first_candidate(b"4\n"), None);
        assert_eq!(extract_first_candidate(b"1/\n"), None);
    }

    #[test]
    fn test_parse_candidates_borrowed() {
        let resp = "1/着/切/伐/\n".as_bytes();
        let borrowed = parse_candidates_borrowed(resp);
        assert_eq!(borrowed, vec!["着", "切", "伐"]);
        let formatted = format_candidates_response_str(&borrowed);
        assert_eq!(formatted, resp);
    }

    #[test]
    fn test_parse_candidates_unpacked() {
        let resp = "1/[っ/思/]/重/御持/お持/想;注釈/\n".as_bytes();
        let unpacked = parse_candidates_unpacked(resp);
        assert_eq!(unpacked, vec!["思", "重", "御持", "お持", "想;注釈"]);

        let resp2 = "1/[ち/待/]/[っ/[つ/[て/舞/俟/\n".as_bytes();
        let unpacked2 = parse_candidates_unpacked(resp2);
        assert_eq!(unpacked2, vec!["待", "舞", "俟"]);

        let resp3 = "1/来/切/着/\n".as_bytes();
        let unpacked3 = parse_candidates_unpacked(resp3);
        assert_eq!(unpacked3, vec!["来", "切", "着"]);
    }
}

