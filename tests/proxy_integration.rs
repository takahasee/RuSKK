use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ruskk::backend::{Backend, UpstreamEncoding};
use ruskk::frequency::{FrequencyPredictor, SharedPredictor};
use ruskk::proxy::Proxy;

#[tokio::test]
async fn test_proxy_lookup_bayesian_ranking_skk_uppercase() {
    // 1. Upstream mock server ALWAYS returns "切る" first: 1/切る/着る/\n
    // This proves that skk-proxy's Bayesian logic is actually re-ranking the candidates
    // instead of relying on any upstream language model ordering.
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();

    let upstream_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = upstream_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        // Match "KiRu" or "kiru" or "きる"
                        if req.starts_with(b"1KiRu") || req.starts_with(b"1kiru") || req.starts_with(b"1\xe3\x81\x8d\xe3\x82\x8b") {
                            // Upstream ALWAYS defaults to "切る" first!
                            let resp = "1/切る/着る/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1fuku") || req.starts_with(b"1\xe3\x81\xb5\xe3\x81\x8f") {
                            let resp = "1/服/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1niku") || req.starts_with(b"1\xe3\x81\xab\xe3\x81\x8f") {
                            let resp = "1/肉/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    // 2. Setup Proxy with pre-trained frequency predictor using SKK keys
    let mut predictor = FrequencyPredictor::new(None);
    // Pre-observe context co-occurrences with uppercase SKK keys like "KiRu"
    predictor.observe(&["服".to_string()], "KiRu", "着る");
    predictor.observe(&["服".to_string()], "KiRu", "着る");
    predictor.observe(&["肉".to_string()], "KiRu", "切る");
    predictor.observe(&["肉".to_string()], "KiRu", "切る");

    let shared_predictor: SharedPredictor = Arc::new(tokio::sync::Mutex::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend {
            name: "mock-primary-down".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_millis(50),
        },
        fallback: Backend {
            name: "mock-fallback-yaskkserv2".into(),
            addr: upstream_addr,
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_secs(1),
        },
        predictor: shared_predictor,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Connect client 1 with context "服" -> lookup SKK style "1KiRu "
    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // Lookup "ふく" -> returns "服", sets session context = ["服"]
    client.write_all(b"1fuku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp1 = String::from_utf8_lossy(&buf[..n]);
    assert!(resp1.contains("服"));

    // Lookup SKK uppercase "1KiRu " with context ["服"]
    // Upstream sends "1/切る/着る/\n", BUT skk-proxy Bayesian model MUST re-rank "着る" to 1st!
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp2 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp2, "1/着る/切る/\n");

    // 4. Connect client 2 with context "肉" -> lookup SKK style "1KiRu "
    let mut client2 = TcpStream::connect(proxy_addr).await.unwrap();

    // Lookup "にく" -> returns "肉", sets session context = ["肉"]
    client2.write_all(b"1niku \n").await.unwrap();
    let n = client2.read(&mut buf).await.unwrap();
    let resp3 = String::from_utf8_lossy(&buf[..n]);
    assert!(resp3.contains("肉"));

    // Lookup SKK uppercase "1KiRu " with context ["肉"] -> top candidate must be "切る"
    client2.write_all(b"1KiRu \n").await.unwrap();
    let n = client2.read(&mut buf).await.unwrap();
    let resp4 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp4, "1/切る/着る/\n");

    drop(client);
    drop(client2);

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_primary_azookey_preserves_order() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();

    let upstream_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = upstream_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with(b"1kiru") {
                            // Primary azooKey returns "切る" first intentionally
                            let resp = "1/切る/着る/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1fuku") {
                            let resp = "1/服/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let mut predictor = FrequencyPredictor::new(None);
    // Pre-observe context "服" -> "着る" (which would normally flip order if Bayesian re-ranking was applied)
    predictor.observe(&["服".to_string()], "kiru", "着る");
    predictor.observe(&["服".to_string()], "kiru", "着る");

    let shared_predictor: SharedPredictor = Arc::new(tokio::sync::Mutex::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend {
            name: "azookey-primary".into(),
            addr: upstream_addr,
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_secs(1),
        },
        fallback: Backend {
            name: "fallback".into(),
            addr: upstream_addr,
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_secs(1),
        },
        predictor: shared_predictor.clone(),
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all(b"1fuku \n").await.unwrap();
    let _n = client.read(&mut buf).await.unwrap();

    // Primary hits MUST preserve azooKey's original candidate order ("1/切る/着る/\n")
    client.write_all(b"1kiru \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/切る/着る/\n", "Primary azooKey order must be preserved!");

    drop(client);

    // Verify Primary hits did NOT write to predictor observation counts
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.get("fuku").and_then(|m| m.get("服")).copied().unwrap_or(0),
            0,
            "Primary hit '服' should not be observed"
        );
    }

    proxy_handle.abort();
    upstream_handle.abort();
}
