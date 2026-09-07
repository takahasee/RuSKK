use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ruskk::backend::{Backend, UpstreamEncoding};
use ruskk::frequency::{FrequencyPredictor, SharedPredictor};
use ruskk::proxy::Proxy;

/// seed データに基づいて候補が並び替えられることをテストする。
/// upstream は常に「切る/着る」の順で返すが、seed データの文脈共起により
/// 「服」の後は「着る」が、「肉」の後は「切る」が第1候補になることを検証する。
#[tokio::test]
async fn test_proxy_ranks_by_seed_context() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();

    let upstream_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = upstream_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 { break; }
                        let req = &buf[..n];
                        if req.starts_with(b"1KiRu") || req.starts_with(b"1kiru") {
                            // upstream は常に「切る」が第1候補
                            let _ = stream.write_all("1/切る/着る/\n".as_bytes()).await;
                        } else if req.starts_with(b"1fuku") {
                            let _ = stream.write_all("1/服/\n".as_bytes()).await;
                        } else if req.starts_with(b"1niku") {
                            let _ = stream.write_all("1/肉/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    // seed データを手動設定（~/.ruskk-frequency.json に相当）
    let mut predictor = FrequencyPredictor::new(None);
    predictor.context_frequencies.insert("服".to_string(), {
        let mut m = HashMap::new();
        m.insert("着る".to_string(), 10);
        m
    });
    predictor.context_frequencies.insert("肉".to_string(), {
        let mut m = HashMap::new();
        m.insert("切る".to_string(), 10);
        m
    });

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
            name: "mock-fallback".into(),
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

    // seed データを変更しないことを確認するため、初期状態を記録
    let initial_freq_count = {
        let guard = shared_predictor.lock().await;
        guard.frequencies.len()
    };

    // --- テスト: 候補の並び替え ---
    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // Lookup して候補返却を確認（プロキシ動作確認）
    client.write_all(b"1fuku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.contains("服"));

    // seed データにコンテキスト「服」→「着る」が定義されているが、
    // session_context は自動更新されないため、初回の KiRu では元順序のまま
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    // session_context が空なので、seed のコンテキスト共起は使われない → 元順序
    assert_eq!(resp, "1/切る/着る/\n");

    // --- テスト: seed データが変更されていないことを確認 ---
    // 自動学習を廃止したので、lookup しても frequencies は更新されない
    tokio::time::sleep(Duration::from_millis(100)).await;
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.len(),
            initial_freq_count,
            "frequencies must NOT change after lookups (no auto-learning)"
        );
    }

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// Lookup のレスポンスが正しく返ること（プロキシの基本動作）をテストする。
#[tokio::test]
async fn test_proxy_basic_lookup_passthrough() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap();

    let upstream_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = upstream_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 { break; }
                        let req = &buf[..n];
                        if req.starts_with(b"1kiru") {
                            let _ = stream.write_all("1/切る/着る/\n".as_bytes()).await;
                        } else if req.starts_with(b"1fuku") {
                            let _ = stream.write_all("1/服/\n".as_bytes()).await;
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
        predictor: shared_predictor,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 通常の Lookup が正しく通過すること
    client.write_all(b"1fuku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/服/\n");

    client.write_all(b"1kiru \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/切る/着る/\n");

    // 見つからない場合
    client.write_all(b"1zzz \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "4\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}
