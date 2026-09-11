use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ruskk::backend::{Backend, UpstreamEncoding};
use ruskk::frequency::{FrequencyPredictor, SharedPredictor};
use ruskk::proxy::Proxy;

/// seed データの単語頻度（frequencies）に基づいて候補が並び替えられることをテストする。
/// upstream は常に「切る/着る」の順で返すが、seed データの単語頻度により
/// 「着る」が第1候補に昇格することを検証する。
#[tokio::test]
async fn test_proxy_ranks_by_seed_frequency() {
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
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    // seed データの単語頻度を手動設定（~/.ruskk-frequency.json に相当）
    let mut predictor = FrequencyPredictor::new(None);
    predictor.frequencies.insert("KiRu".to_string(), {
        let mut m = HashMap::new();
        m.insert("着る".to_string(), 10);
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
        okuri_expansion: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // upstream は通常「切る」が第1候補だが、
    // seed データに単語頻度「着る」= 10 が定義されているため、
    // 「着る」が第1候補に昇格する！
    client.write_all(b"1KiRu \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/着る/切る/\n");

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
        okuri_expansion: false,
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

/// macSKK の補完クエリ（Request::Completion）に対して常に 4\n を返し、
/// macSKK の勝手な確定（タイピング停止後の誤爆確定）を物理的に防止することを検証するテスト。
#[tokio::test]
async fn test_proxy_returns_not_found_on_completion() {
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
                            // upstream が補完候補を持っていたとしても
                            let _ = stream.write_all("1/kiru/kiku/\n".as_bytes()).await;
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
        okuri_expansion: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // macSKK が補完クエリ "4ki " を投げても、プロキシは直ちに 4\n を返す
    client.write_all(b"4ki \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "4\n", "補完クエリには常に 4\\n を返して macSKK の自動確定を防ぐ");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// ユーザーが Space を押した時、1文字見出し（「き」等）でも
/// 初回から素直に変換候補が返ることを検証するテスト。
#[tokio::test]
async fn test_proxy_single_char_lookup_works_immediately() {
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
                        if req.starts_with("1き ".as_bytes()) {
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
        okuri_expansion: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // ユーザーが Space キーを押して「き」を漢字に変換しようとした場合
    // 初回から通常変換として正規の候補が返る！
    client.write_all("1き \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/木/気/\n", "1文字見出しでも初回から候補が返る");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 送りあり見出し（例: "かk"）を平仮名活用形（"かく"）に復元し、
/// azooKey から返った動詞候補から語幹（単漢字）を抽出して返すことを検証するテスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_verb() {
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
                        // 復元された平仮名活用形 "かく" を受け取った場合
                        if req.starts_with("1かく ".as_bytes()) {
                            let _ = stream.write_all("1/書く/各/描く/辛く/\n".as_bytes()).await;
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
            name: "azookey-mock".into(),
            addr: upstream_addr,
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_secs(1),
        },
        fallback: Backend {
            name: "fallback-down".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_millis(50),
        },
        predictor: shared_predictor,
        okuri_expansion: true, // 送り復元有効
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // クライアントが SKK送りあり "1かk " を送信
    client.write_all("1かk \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    // 名詞 "各" は除外され、動詞 "書", "描", "辛" の語幹（単漢字）が返る！
    assert_eq!(resp, "1/書/描/辛/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 送り復元が無効（okuri_expansion: false）のときは、
/// 従来通り元の見出し語（"かk"）がそのまま照会されることを検証するテスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_disabled_fallback() {
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
                        // 復元されず元の "かk" のまま照会される
                        if req.starts_with("1かk ".as_bytes()) {
                            let _ = stream.write_all("1/化/家/書/\n".as_bytes()).await;
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
            name: "azookey-mock".into(),
            addr: upstream_addr,
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_secs(1),
        },
        fallback: Backend {
            name: "fallback-down".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_millis(50),
        },
        predictor: shared_predictor,
        okuri_expansion: false, // 送り復元無効（従来動作）
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all("1かk \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    // 従来の単漢字リストがそのまま返る
    assert_eq!(resp, "1/化/家/書/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

