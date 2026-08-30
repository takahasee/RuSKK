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
