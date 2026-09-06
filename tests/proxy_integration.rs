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
async fn test_primary_azookey_ranks_by_context_and_observes() {
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
    // Pre-observe context "服" -> "着る"
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

    // rank_candidates IS applied to Primary (azooKey) results.
    // The predictor has "服" -> "着る" observed twice, so "着る" should rank first
    // even though azooKey returns "切る/着る" by default.
    client.write_all(b"1kiru \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/着る/切る/\n", "rank_candidates must reorder Primary results by context!");

    drop(client);
    // Give a short moment for EOF handling and pending observation flush
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Both "服" and "着る" should be observed in the predictor now
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.get("fuku").and_then(|m| m.get("服")).copied().unwrap_or(0),
            1,
            "Primary hit '服' should be observed on subsequent lookup"
        );
        assert_eq!(
            guard.frequencies.get("kiru").and_then(|m| m.get("着る")).copied().unwrap_or(0),
            3, // Initial 2 + 1 newly observed
            "Primary hit '着る' should be observed on client disconnect flush"
        );
    }

    proxy_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
async fn test_completion_burst_does_not_pollute_and_timeout_flushes() {
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
                        if req.starts_with(b"4ho") {
                            // Completion returns multiple completion keys
                            let resp = "1/hozon/hozonundou/ほぞん/ほぞんうんどう/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1hozonundou") {
                            let resp = "1/保存運動/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1hozon") {
                            let resp = "1/保存/\n".as_bytes();
                            let _ = stream.write_all(resp).await;
                        } else if req.starts_with(b"1hontou") {
                            // Local dict completion (not in upstream 4ho, but starts with prefix "ho")
                            let resp = "1/本当/\n".as_bytes();
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

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(tokio::sync::Mutex::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend {
            name: "primary".into(),
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

    // 1. macSKK completion flow: opcode 4 followed by lookups (with delay between them)
    client.write_all(b"4ho \n").await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // Lookups for completion preview:
    // First lookup: in recent_completions
    client.write_all(b"1hozon \n").await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // Introduce 300ms delay (>150ms burst threshold) between completion lookups,
    // simulating network/Google Suggest latency from yaskkserv2.
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_all(b"1hozonundou \n").await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // Local dictionary completion candidate (starts with prefix "ho", delayed by 300ms)
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.write_all(b"1hontou \n").await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // Single-char preview lookup (like typing 1st char "れ" or "か" where macSKK sends 1<char> without opcode 4)
    client.write_all("1れ \n".as_bytes()).await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // Idle for 3.2s (> 3.0s debounce timeout)
    tokio::time::sleep(Duration::from_millis(3200)).await;

    // Verify completion lookups and single-char preview are NEVER observed or confirmed!
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.get("hozon").and_then(|m| m.get("保存")).copied().unwrap_or(0),
            0,
            "Completion lookup 'hozon' must NOT be auto-confirmed"
        );
        assert_eq!(
            guard.frequencies.get("hozonundou").and_then(|m| m.get("保存運動")).copied().unwrap_or(0),
            0,
            "Completion lookup 'hozonundou' must NOT be auto-confirmed"
        );
        assert_eq!(
            guard.frequencies.get("hontou").and_then(|m| m.get("本当")).copied().unwrap_or(0),
            0,
            "Local dict completion lookup 'hontou' must NOT be auto-confirmed"
        );
        assert_eq!(
            guard.frequencies.get("れ").map(|m| m.values().sum::<u64>()).unwrap_or(0),
            0,
            "Single-char preview lookup 'れ' must NOT be auto-confirmed"
        );
    }

    // 2. Now send a REGULAR user conversion lookup "1fuku \n"
    client.write_all(b"1fuku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/服/\n");

    // Client stays connected (like macSKK connection pool).
    // Wait for the 3.0s timeout flush to trigger!
    tokio::time::sleep(Duration::from_millis(3200)).await;

    // Verify "fuku" -> "服" was automatically flushed to predictor on idle timeout!
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.get("fuku").and_then(|m| m.get("服")).copied().unwrap_or(0),
            1,
            "Regular conversion 'fuku' -> '服' must be flushed on idle timeout without closing TCP connection"
        );
    }

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}
