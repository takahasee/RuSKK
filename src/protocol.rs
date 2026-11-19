//! skkserv protocol parsing.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request<'a> {
    /// opcode `0` — disconnect
    End,
    /// opcode `1` — dictionary lookup
    Lookup(&'a [u8]),
    /// opcode `2` — server version
    Version,
    /// opcode `3` — host info
    Host,
    /// opcode `4` — server completion
    Completion(&'a [u8]),
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("empty request")]
    Empty,
    #[error("unknown opcode: {0}")]
    UnknownOpcode(u8),
    #[error("invalid midashi")]
    InvalidMidashi,
}

#[inline]
fn has_disallowed_midashi_byte(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| b <= 0x20 || b == 0x7F)
}

/// Parse one skkserv request from a line (without trailing LF).
///
/// Accepted forms:
/// - `0` / `0 ` — end
/// - `1<midashi> ` — lookup (trailing space optional)
/// - `2` / `2 ` — version
/// - `3` / `3 ` — host
/// - `4<midashi> ` — completion (trailing space optional)
pub fn parse_request(line: &[u8]) -> Result<Request<'_>, ProtocolError> {
    let line = trim_ascii_whitespace_end(line);
    if line.is_empty() {
        return Err(ProtocolError::Empty);
    }

    let opcode = line[0];
    let operand = &line[1..];

    match opcode {
        b'0' => Ok(Request::End),
        b'1' => {
            // リクエストスマグリング防止: 見出し語内部の空白・制御文字（\r, \n, \0 等）を拒否する
            if has_disallowed_midashi_byte(operand) {
                return Err(ProtocolError::InvalidMidashi);
            }
            Ok(Request::Lookup(operand))
        }
        b'2' => Ok(Request::Version),
        b'3' => Ok(Request::Host),
        b'4' => {
            if has_disallowed_midashi_byte(operand) {
                return Err(ProtocolError::InvalidMidashi);
            }
            Ok(Request::Completion(operand))
        }
        other => Err(ProtocolError::UnknownOpcode(other)),
    }
}

/// Rebuild a wire request for forwarding to an upstream skkserv.
fn encode_request_inner(request: &Request<'_>, is_euc: bool) -> Vec<u8> {
    use std::borrow::Cow;

    use crate::encoding::{decode_euc_or_utf8, encode_euc_jp_cow};

    let (opcode, midashi) = match request {
        Request::End => return b"0 \n".to_vec(),
        Request::Version => return b"2 \n".to_vec(),
        Request::Host => return b"3 \n".to_vec(),
        Request::Lookup(m) => (b'1', m),
        Request::Completion(m) => (b'4', m),
    };

    // defense-in-depth: 空白・制御文字を含むバイトが渡された場合も必ず除外してフレーム境界を保護する
    let sanitized_midashi: Cow<'_, [u8]> = if has_disallowed_midashi_byte(midashi) {
        Cow::Owned(
            midashi
                .iter()
                .copied()
                .filter(|&b| b > 0x20 && b != 0x7F)
                .collect(),
        )
    } else {
        Cow::Borrowed(*midashi)
    };

    let midashi_str = decode_euc_or_utf8(&sanitized_midashi);
    let payload: Cow<'_, [u8]> = if is_euc {
        encode_euc_jp_cow(&midashi_str)
    } else {
        Cow::Borrowed(midashi_str.as_bytes())
    };

    let mut buf = Vec::with_capacity(payload.len() + 3);
    buf.push(opcode);
    buf.extend_from_slice(&payload);
    buf.extend_from_slice(b" \n");
    buf
}

/// Rebuild a wire request for forwarding to an upstream skkserv (UTF-8).
pub fn encode_request(request: &Request<'_>) -> Vec<u8> {
    encode_request_inner(request, false)
}

/// EUC-JP バックエンド向けにリクエストを構築する。
/// 見出し語を正しくデコードした上で EUC-JP にエンコードして送信する。
pub fn encode_request_for_encoding(request: &Request<'_>) -> Vec<u8> {
    encode_request_inner(request, true)
}

#[inline]
pub fn is_found(response: &[u8]) -> bool {
    response.first() == Some(&b'1')
}

#[inline]
fn trim_ascii_whitespace_end(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    &bytes[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lookup() {
        assert_eq!(parse_request(b"1ai ").unwrap(), Request::Lookup(b"ai"));
        assert_eq!(parse_request(b"1ai").unwrap(), Request::Lookup(b"ai"));
    }

    #[test]
    fn parse_simple_opcodes() {
        assert_eq!(parse_request(b"0").unwrap(), Request::End);
        assert_eq!(parse_request(b"2 ").unwrap(), Request::Version);
        assert_eq!(parse_request(b"3").unwrap(), Request::Host);
    }

    #[test]
    fn parse_completion() {
        assert_eq!(
            parse_request(b"4kan ").unwrap(),
            Request::Completion(b"kan")
        );
    }

    #[test]
    fn encode_lookup_roundtrip() {
        let req = Request::Lookup(b"test");
        let wire = encode_request(&req);
        assert_eq!(wire, b"1test \n");
    }

    #[test]
    fn reject_smuggling_midashi_and_sanitize_encode() {
        assert!(matches!(
            parse_request(b"1foo 1bar "),
            Err(ProtocolError::InvalidMidashi)
        ));
        assert!(matches!(
            parse_request(b"1foo\rbar"),
            Err(ProtocolError::InvalidMidashi)
        ));
        assert!(matches!(
            parse_request(b"1foo\n1bar"),
            Err(ProtocolError::InvalidMidashi)
        ));
        assert!(matches!(
            parse_request(b"1foo\0bar"),
            Err(ProtocolError::InvalidMidashi)
        ));
        assert!(matches!(
            parse_request(b"4kan 1bar"),
            Err(ProtocolError::InvalidMidashi)
        ));

        // 空 operand はプロキシ側で接続維持のまま 4\n を返せるようパースは許容する
        assert_eq!(parse_request(b"1 ").unwrap(), Request::Lookup(b""));
        assert_eq!(parse_request(b"4 ").unwrap(), Request::Completion(b""));

        // defense-in-depth: 制御文字や空白が encode_request に渡されても除去されること
        let sanitized = encode_request(&Request::Lookup(b"foo 1bar\r\n\0"));
        assert_eq!(sanitized, b"1foo1bar \n");
    }
}
