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
    // 「服」を変換すると、session_context に「服」が追跡される
    client.write_all(b"1fuku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/服/\n");

    // upstream は通常「切る」が第1候補だが、
    // seed データにコンテキスト「服」→「着る」が定義されているため、
    // 「着る」が第1候補に昇格する！
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/着る/切る/\n");

    // 次に「肉」を変換すると、session_context が「肉」に更新される
    client.write_all(b"1niku \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/肉/\n");

    // seed データにコンテキスト「肉」→「切る」が定義されているため、
    // 今度は「切る」が第1候補になる！
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/切る/着る/\n");

    // --- テスト: 1文字プレビュー等による文脈汚染防止 ---
    // 1文字の非ASCII見出し（プレビュー）が来ても、直前の「肉」文脈は上書きされない
    client.write_all(b"1ka \n").await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // 再度 KiRu を引くと、依然として「肉」文脈が維持されており「切る」が第1候補
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/切る/着る/\n");

    // --- テスト: seed データが変更されていないことを確認 ---
    // 自動学習を廃止したので、lookup しても frequencies は更新されない（読み取り専用）
    tokio::time::sleep(Duration::from_millis(100)).await;
    {
        let guard = shared_predictor.lock().await;
        assert_eq!(
            guard.frequencies.len(),
            initial_freq_count,
            "frequencies must NOT change after lookups (read-only seed file)"
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

/// macSKK の補完クエリ直後に裏で自動送信される展開 Lookup が
/// 4\n で抑制され、macSKK の勝手な確定（0.3秒誤爆確定）を防止することを検証するテスト。
#[tokio::test]
async fn test_proxy_suppresses_completion_refer_lookup() {
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
                        if req.starts_with(b"4ki") {
                            // 補完クエリに対する候補一覧
                            let _ = stream.write_all("1/kiru/kiku/\n".as_bytes()).await;
                        } else if req.starts_with(b"1kiru") {
                            let _ = stream.write_all("1/切る/着る/\n".as_bytes()).await;
                        } else if req.starts_with(b"1ki ") {
                            let _ = stream.write_all("1/木/気/\n".as_bytes()).await;
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

    // 1. まず macSKK がタイピング中に補完クエリ "4ki " を投げる
    client.write_all(b"4ki \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.starts_with("1/"));

    // 2. macSKK が裏で補完候補 "kiru" を展開しようと Lookup "1kiru " を送信する
    // RuSKK はこれを検出し、直ちに 4\n を返して裏展開を抑止する！
    client.write_all(b"1kiru \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "4\n", "裏補完展開 Lookup は 4\\n で抑制されなければならない");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

