use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use skk_proxy::backend::{Backend, UpstreamEncoding};
use skk_proxy::protocol::{is_found, Request};

#[tokio::test]
async fn backend_utf8_passthrough() {
    let addr = spawn_mock(|req| {
        assert!(req.starts_with(b"1"));
        b"1/\xe3\x81\x82/\n".to_vec()
    })
    .await;

    let backend = Backend {
        name: "mock-utf8".into(),
        addr,
        encoding: UpstreamEncoding::Utf8,
        timeout: Duration::from_secs(1),
    };
    let resp = backend
        .query(&Request::Lookup(b"a".to_vec()))
        .await
        .unwrap();
    assert!(is_found(&resp));
    assert_eq!(resp, b"1/\xe3\x81\x82/\n");
}

#[tokio::test]
async fn backend_euc_converts_to_utf8() {
    let addr = spawn_mock(|req| {
        assert!(req.starts_with(b"1"));
        vec![b'1', b'/', 0xA4, 0xA2, b'/', b'\n']
    })
    .await;

    let backend = Backend {
        name: "mock-euc".into(),
        addr,
        encoding: UpstreamEncoding::EucJp,
        timeout: Duration::from_secs(1),
    };
    let resp = backend
        .query(&Request::Lookup(b"a".to_vec()))
        .await
        .unwrap();
    assert_eq!(resp, b"1/\xe3\x81\x82/\n");
}

#[tokio::test]
async fn backend_not_found() {
    let addr = spawn_mock(|_| b"4\n".to_vec()).await;
    let backend = Backend {
        name: "mock-miss".into(),
        addr,
        encoding: UpstreamEncoding::Utf8,
        timeout: Duration::from_secs(1),
    };
    let resp = backend
        .query(&Request::Lookup(b"zzz".to_vec()))
        .await
        .unwrap();
    assert!(!is_found(&resp));
    assert_eq!(resp, b"4\n");
}

async fn spawn_mock(handler: fn(&[u8]) -> Vec<u8>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let response = handler(&buf[..n]);
        let _ = stream.write_all(&response).await;
    });
    addr
}

#[test]
fn test_completion_merge_logic() {
    let primary_resp = b"1/\xe3\x81\x82/\xe3\x81\x84/\n"; // 1/あ/い/
    let fallback_resp = vec![b'1', b'/', 0xA4, 0xA4, b'/', 0xA4, 0xA6, b'/', b'\n']; // 1/い/う/ in EUC-JP

    let primary_utf8 = skk_proxy::encoding::response_euc_to_utf8(primary_resp);
    let fallback_utf8 = skk_proxy::encoding::response_euc_to_utf8(&fallback_resp);

    let primary_cands = skk_proxy::encoding::parse_candidates(&primary_utf8);
    let fallback_cands = skk_proxy::encoding::parse_candidates(&fallback_utf8);

    let merged = skk_proxy::encoding::merge_candidates(&primary_cands, &fallback_cands);
    assert_eq!(merged, vec!["あ", "い", "う"]);

    let formatted = skk_proxy::encoding::format_candidates_response(&merged);
    assert_eq!(formatted, "1/あ/い/う/\n".as_bytes());
}

