use encoding_rs::EUC_JP;

/// Decode bytes that may be EUC-JP or already UTF-8 into a UTF-8 String.
pub fn decode_euc_or_utf8(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    let (cow, _, _) = EUC_JP.decode(bytes);
    cow.into_owned()
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

    match response[0] {
        b'1' => convert_candidates_response(response),
        b'4' => convert_not_found_response(response),
        _ => decode_euc_or_utf8(response).into_bytes(),
    }
}

fn convert_candidates_response(response: &[u8]) -> Vec<u8> {
    // Format: 1/<cand>/<cand>/...\n
    let mut out = Vec::with_capacity(response.len() * 2);
    out.push(b'1');

    let rest = &response[1..];
    let body = trim_trailing_newline(rest);

    // split("/あ/い/") => ["", "あ", "い", ""] — skip the leading empty segment
    for part in body.split(|&b| b == b'/').skip(1) {
        out.push(b'/');
        if !part.is_empty() {
            out.extend_from_slice(decode_euc_or_utf8(part).as_bytes());
        }
    }

    if rest.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

fn convert_not_found_response(response: &[u8]) -> Vec<u8> {
    // Format: 4\n  or  4<midashi> \n
    if response == b"4\n" || response == b"4" {
        return response.to_vec();
    }

    let mut out = Vec::with_capacity(response.len() * 2);
    out.push(b'4');
    let rest = &response[1..];
    let body = trim_trailing_newline(rest);
    out.extend_from_slice(decode_euc_or_utf8(body).as_bytes());
    if rest.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

fn trim_trailing_newline(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
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
}
