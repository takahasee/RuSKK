use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use ruskkserv::backend::{Backend, UpstreamEncoding};
use ruskkserv::protocol::{Request, is_found};

#[tokio::test]
async fn backend_utf8_passthrough() {
    let addr = spawn_mock(|req| {
        assert!(req.starts_with(b"1"));
        b"1/\xe3\x81\x82/\n".to_vec()
    })
    .await;

    let backend = Backend::new(
        "mock-utf8",
        addr,
        UpstreamEncoding::Utf8,
        Duration::from_secs(1),
    );
    let resp = backend.query(&Request::Lookup(b"a")).await.unwrap();
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

    let backend = Backend::new(
        "mock-euc",
        addr,
        UpstreamEncoding::EucJp,
        Duration::from_secs(1),
    );
    let resp = backend.query(&Request::Lookup(b"a")).await.unwrap();
    assert_eq!(resp, b"1/\xe3\x81\x82/\n");
}

#[tokio::test]
async fn backend_not_found() {
    let addr = spawn_mock(|_| b"4\n".to_vec()).await;
    let backend = Backend::new(
        "mock-miss",
        addr,
        UpstreamEncoding::Utf8,
        Duration::from_secs(1),
    );
    let resp = backend.query(&Request::Lookup(b"zzz")).await.unwrap();
    assert!(!is_found(&resp));
    assert_eq!(resp, b"4\n");
}

#[tokio::test]
async fn backend_pool_concurrent_queries() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // 複数並行接続を処理するモックサーバー
    tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1024];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let _ = stream.write_all(b"1/\xe3\x81\x82/\n").await;
                    }
                });
            }
        }
    });

    let backend = Backend::new(
        "mock-pool",
        addr,
        UpstreamEncoding::Utf8,
        Duration::from_secs(1),
    );

    // 10 個の並行クエリを同時に実行
    let mut handles = Vec::new();
    for _ in 0..10 {
        let b = backend.clone();
        handles.push(tokio::spawn(async move {
            b.query(&Request::Lookup(b"a")).await.unwrap()
        }));
    }

    for h in handles {
        let resp = h.await.unwrap();
        assert_eq!(resp, b"1/\xe3\x81\x82/\n");
    }
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

    let primary_utf8 = ruskkserv::encoding::response_euc_to_utf8(primary_resp);
    let fallback_utf8 = ruskkserv::encoding::response_euc_to_utf8(&fallback_resp);

    let primary_cands = ruskkserv::encoding::parse_candidates_borrowed(&primary_utf8);
    let fallback_cands = ruskkserv::encoding::parse_candidates_borrowed(&fallback_utf8);

    let mut merged = primary_cands;
    for cand in fallback_cands {
        if !merged.contains(&cand) {
            merged.push(cand);
        }
    }
    assert_eq!(merged, vec!["あ", "い", "う"]);

    let formatted = ruskkserv::encoding::format_candidates_response_str(&merged);
    assert_eq!(formatted, "1/あ/い/う/\n".as_bytes());
}

#[tokio::test]
async fn backend_recovers_when_multiple_pooled_connections_are_stale() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let close_idle_conns = Arc::new(AtomicBool::new(false));
    let close_notify = Arc::new(Notify::new());
    let close_flag_srv = Arc::clone(&close_idle_conns);
    let close_notify_srv = Arc::clone(&close_notify);

    let srv_handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let close_flag = Arc::clone(&close_flag_srv);
            let notify = Arc::clone(&close_notify_srv);
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                loop {
                    tokio::select! {
                        n_res = stream.read(&mut buf) => {
                            match n_res {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {
                                    if close_flag.load(Ordering::SeqCst) {
                                        // 失効した接続として即座に閉じることはせず、2巡目の通常応答
                                        let _ = stream.write_all("1/新接続成功/\n".as_bytes()).await;
                                    } else {
                                        let _ = stream.write_all("1/初回プール構築/\n".as_bytes()).await;
                                    }
                                }
                            }
                        }
                        _ = notify.notified(), if !close_flag.load(Ordering::SeqCst) => {
                            // 通知を受けたら既存のアイドルソケットをサーバー側から切断する
                            break;
                        }
                    }
                }
            });
        }
    });

    let backend = Backend::new(
        "mock-stale-pool",
        addr,
        UpstreamEncoding::Utf8,
        Duration::from_secs(1),
    );

    // 1. 同時に 5 本の接続を開き、プールに 5 本のアイドル接続を溜める
    let mut handles = Vec::new();
    for _ in 0..5 {
        let b = backend.clone();
        handles.push(tokio::spawn(async move {
            b.query(&Request::Lookup(b"a")).await.unwrap()
        }));
    }
    for h in handles {
        let resp = h.await.unwrap();
        assert_eq!(resp, "1/初回プール構築/\n".as_bytes());
    }

    // 2. サーバー側でプール内の全 5 本を切断し、以後は新規接続に「新接続成功」を返す
    close_idle_conns.store(true, Ordering::SeqCst);
    close_notify.notify_waiters();
    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. プールに 2 本以上（5本）の死んだ接続があっても、初回クエリでプールを全破棄して新規接続し成功すること
    let resp = backend.query(&Request::Lookup(b"a")).await.unwrap();
    assert_eq!(resp, "1/新接続成功/\n".as_bytes());

    srv_handle.abort();
}
