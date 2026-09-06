//! skkserv protocol parsing.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// opcode `0` — disconnect
    End,
    /// opcode `1` — dictionary lookup
    Lookup(Vec<u8>),
    /// opcode `2` — server version
    Version,
    /// opcode `3` — host info
    Host,
    /// opcode `4` — server completion
    Completion(Vec<u8>),
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("empty request")]
    Empty,
    #[error("unknown opcode: {0}")]
    UnknownOpcode(u8),
}

/// Parse one skkserv request from a line (without trailing LF).
///
/// Accepted forms:
/// - `0` / `0 ` — end
/// - `1<midashi> ` — lookup (trailing space optional)
/// - `2` / `2 ` — version
/// - `3` / `3 ` — host
/// - `4<midashi> ` — completion (trailing space optional)
pub fn parse_request(line: &[u8]) -> Result<Request, ProtocolError> {
    let line = trim_ascii_whitespace_end(line);
    if line.is_empty() {
        return Err(ProtocolError::Empty);
    }

    let opcode = line[0];
    let operand = &line[1..];

    match opcode {
        b'0' => Ok(Request::End),
        b'1' => Ok(Request::Lookup(normalize_midashi(operand))),
        b'2' => Ok(Request::Version),
        b'3' => Ok(Request::Host),
        b'4' => Ok(Request::Completion(normalize_midashi(operand))),
        other => Err(ProtocolError::UnknownOpcode(other)),
    }
}

/// Rebuild a wire request for forwarding to an upstream skkserv (UTF-8).
pub fn encode_request(request: &Request) -> Vec<u8> {
    use crate::encoding::decode_euc_or_utf8;

    match request {
        Request::End => b"0 \n".to_vec(),
        Request::Lookup(midashi) => {
            let midashi_str = decode_euc_or_utf8(midashi);
            let mut buf = Vec::with_capacity(midashi_str.len() + 3);
            buf.push(b'1');
            buf.extend_from_slice(midashi_str.as_bytes());
            buf.extend_from_slice(b" \n");
            buf
        }
        Request::Version => b"2 \n".to_vec(),
        Request::Host => b"3 \n".to_vec(),
        Request::Completion(midashi) => {
            let midashi_str = decode_euc_or_utf8(midashi);
            let mut buf = Vec::with_capacity(midashi_str.len() + 3);
            buf.push(b'4');
            buf.extend_from_slice(midashi_str.as_bytes());
            buf.extend_from_slice(b" \n");
            buf
        }
    }
}

/// EUC-JP バックエンド向けにリクエストを構築する。
/// 見出し語を正しくデコードした上で EUC-JP にエンコードして送信する。
pub fn encode_request_for_encoding(request: &Request) -> Vec<u8> {
    use crate::encoding::{decode_euc_or_utf8, encode_euc_jp};

    match request {
        Request::End => b"0 \n".to_vec(),
        Request::Lookup(midashi) => {
            let midashi_str = decode_euc_or_utf8(midashi);
            let euc_midashi = encode_euc_jp(&midashi_str);
            let mut buf = Vec::with_capacity(euc_midashi.len() + 3);
            buf.push(b'1');
            buf.extend_from_slice(&euc_midashi);
            buf.extend_from_slice(b" \n");
            buf
        }
        Request::Version => b"2 \n".to_vec(),
        Request::Host => b"3 \n".to_vec(),
        Request::Completion(midashi) => {
            let midashi_str = decode_euc_or_utf8(midashi);
            let euc_midashi = encode_euc_jp(&midashi_str);
            let mut buf = Vec::with_capacity(euc_midashi.len() + 3);
            buf.push(b'4');
            buf.extend_from_slice(&euc_midashi);
            buf.extend_from_slice(b" \n");
            buf
        }
    }
}

pub fn is_found(response: &[u8]) -> bool {
    response.first() == Some(&b'1')
}

fn normalize_midashi(operand: &[u8]) -> Vec<u8> {
    trim_ascii_whitespace_end(operand).to_vec()
}

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
        assert_eq!(
            parse_request(b"1ai ").unwrap(),
            Request::Lookup(b"ai".to_vec())
        );
        assert_eq!(
            parse_request(b"1ai").unwrap(),
            Request::Lookup(b"ai".to_vec())
        );
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
            Request::Completion(b"kan".to_vec())
        );
    }

    #[test]
    fn encode_lookup_roundtrip() {
        let req = Request::Lookup(b"test".to_vec());
        let wire = encode_request(&req);
        assert_eq!(wire, b"1test \n");
    }
}
