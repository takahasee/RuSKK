use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ruskkserv::backend::{Backend, UpstreamEncoding};
use ruskkserv::frequency::{FrequencyPredictor, SharedPredictor};
use ruskkserv::proxy::Proxy;

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

    // seed データの単語頻度を手動設定（~/.ruskkserv-frequency.json に相当）
    let mut predictor = FrequencyPredictor::new(None);
    predictor.frequencies.insert("KiRu".to_string(), {
        let mut m = HashMap::new();
        m.insert("着る".to_string(), 10);
        m
    });
    predictor.expand_aliases();

    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        fallback: Backend::new(
            "mock-fallback",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        predictor: shared_predictor.clone(),
        okuri_expansion: false,
        context_ranking: false,
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
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        predictor: shared_predictor,
        okuri_expansion: false,
        context_ranking: false,
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

/// 補完リクエスト（4ki ）が upstream に照会され、
/// 補完結果（1/kiru/kiku/\n）がクライアントに返却されることを検証するテスト。
#[tokio::test]
async fn test_proxy_forwards_completion() {
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
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        predictor: shared_predictor,
        okuri_expansion: false,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all(b"4ki \n").await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/kiru/kiku/\n", "upstream からの補完候補が返却されること");

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
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        predictor: shared_predictor,
        okuri_expansion: false,
        context_ranking: false,
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
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true, // 送り復元有効
        context_ranking: false,
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

/// 複合語送りあり見出し（"交ぜGk", "まぜがk"）から、
/// アップストリーム照会を経て "交ぜ書", "交ぜ書き" が返ることを検証するテスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_compound_mazegaki() {
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
                        // 復元された "交ぜがき" または "まぜがき" に対するモックレスポンス
                        if req.starts_with("1交ぜがき ".as_bytes()) {
                            let _ = stream.write_all("1/交ぜ書き/交ぜ餓鬼/\n".as_bytes()).await;
                        } else if req.starts_with("1まぜがき ".as_bytes()) {
                            let _ = stream.write_all("1/混ぜ書き/交ぜ書き/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1. 大文字送りあり "1交ぜGk " -> 語幹 "交ぜ書" が返る（macSKK で送り仮名「き」が付与され「交ぜ書き」になる）
    client.write_all("1交ぜGk \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.starts_with("1/交ぜ書/"), "unexpected resp: {}", resp);

    // 1b. 大文字ローマ字送りあり "1交ぜGak " -> 語幹 "交ぜ書" が返る
    client.write_all("1交ぜGak \n".as_bytes()).await.unwrap();
    let n1b = client.read(&mut buf).await.unwrap();
    let resp1b = String::from_utf8_lossy(&buf[..n1b]);
    assert!(resp1b.starts_with("1/交ぜ書/"), "unexpected resp1b: {}", resp1b);

    // 2. 平仮名送りあり複合語 "1まぜがk " -> 語幹 "混ぜ書", "交ぜ書" が返る
    client.write_all("1まぜがk \n".as_bytes()).await.unwrap();
    let n2 = client.read(&mut buf).await.unwrap();
    let resp2 = String::from_utf8_lossy(&buf[..n2]);
    assert!(resp2.starts_with("1/混ぜ書/"), "unexpected resp2: {}", resp2);

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 複合語の送りあり見出し（例: "ToiawaS ->e" -> "といあわs"）で、
/// 下一段・名詞形 "といあわせ" が照会され、語幹 "問い合わ", "問合" が返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_compound_toiawase() {
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
                        // 複合語として優先展開された "といあわす", "といあわせ" に対する upstream レスポンス
                        if req.starts_with("1といあわす ".as_bytes()) {
                            let _ = stream.write_all("1/問い合わす/\n".as_bytes()).await;
                        } else if req.starts_with("1といあわせ ".as_bytes()) {
                            let _ = stream.write_all("1/問い合わせ/問合せ/問い合せ/問合わせ/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1といあわs " -> 終止形・下一段形からマージされ、語幹 "問い合わ", "問合" が返る
    // （macSKK で送り仮名「せ」が付加されると「問い合わせ」「問合せ」になる）
    client.write_all("1といあわs \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.contains("/問合/"), "expected '問合' in resp: {}", resp);
    assert!(resp.contains("/問い合わ/"), "expected '問い合わ' in resp: {}", resp);

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 複合語五段動詞（例: "KakiokoS ->i" -> "かきおこs"）で、
/// 終止形 "かきおこす" 由来の「書起こ」と連用形 "かきおこし" がマージされて返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_compound_kakiokosi() {
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
                        if req.starts_with("1かきおこす ".as_bytes()) {
                            let _ = stream.write_all("1/書き起こす/書き起す/書起こす/\n".as_bytes()).await;
                        } else if req.starts_with("1かきおこし ".as_bytes()) {
                            let _ = stream.write_all("1/書き起こし/掻き起し/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1かきおこs " -> 終止形由来の "書起こ" も連用形由来の "掻き起" もマージされて返る
    client.write_all("1かきおこs \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.contains("/書き起こ/"), "expected '書き起こ' in resp: {}", resp);
    assert!(resp.contains("/書起こ/"), "expected '書起こ' in resp: {}", resp);
    assert!(resp.contains("/掻き起/"), "expected '掻き起' in resp: {}", resp);

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 複合語下一段動詞（例: "NeaG ->e" -> "ねあg"）で、
/// 下一段 "ねあげ" 由来の「値上」が第1候補として返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_compound_neage() {
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
                        if req.starts_with("1ねあげ ".as_bytes()) {
                            let _ = stream.write_all("1/値上げ/値上/\n".as_bytes()).await;
                        } else if req.starts_with("1ねあぐ ".as_bytes()) {
                            let _ = stream.write_all("1/寝あぐ/寝アグ/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all("1ねあg \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(resp.starts_with("1/値上/"), "expected '値上' as top candidate, got: {}", resp);

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
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: false, // 送り復元無効（従来動作）
        context_ranking: false,
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

/// 直前単語に基づく文脈共起並び替え（肉→切る、服→着る、木→伐る）がプロキシ上で連動することを検証するテスト。
#[tokio::test]
async fn test_proxy_context_ranking_promotes_candidates() {
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
                        if req.starts_with("1にく ".as_bytes()) {
                            let _ = stream.write_all("1/肉/\n".as_bytes()).await;
                        } else if req.starts_with("1ふく ".as_bytes()) {
                            let _ = stream.write_all("1/服/\n".as_bytes()).await;
                        } else if req.starts_with("1き ".as_bytes()) {
                            let _ = stream.write_all("1/木/\n".as_bytes()).await;
                        } else if req.starts_with("1きる ".as_bytes()) {
                            // azooKey は平仮名「きる」に対して着る・切る・伐るを返却
                            let _ = stream.write_all("1/着る/切る/伐る/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let mut predictor = FrequencyPredictor::new(None);
    // 固定頻度: 着=3, 切=2, 伐=1
    predictor.frequencies.insert("きr".to_string(), {
        let mut m = HashMap::new();
        m.insert("着".to_string(), 3);
        m.insert("切".to_string(), 2);
        m.insert("伐".to_string(), 1);
        m
    });
    // 文脈共起: 肉->切る, 服->着る, 木->伐る
    predictor.context_frequencies.insert("肉".to_string(), {
        let mut m = HashMap::new();
        m.insert("切る".to_string(), 10);
        m
    });
    predictor.context_frequencies.insert("服".to_string(), {
        let mut m = HashMap::new();
        m.insert("着る".to_string(), 10);
        m
    });
    predictor.context_frequencies.insert("木".to_string(), {
        let mut m = HashMap::new();
        m.insert("伐る".to_string(), 10);
        m
    });
    predictor.expand_aliases();

    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "mock-fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: true,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // --- ケース 1: 肉 -> 切る（切） ---
    {
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let mut buf = [0u8; 512];
        client.write_all("1にく \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/肉/\n");

        tokio::time::sleep(Duration::from_millis(80)).await;

        client.write_all("1きr \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        // 文脈「肉」により「切」が第1候補！
        assert_eq!(resp, "1/切/着/伐/\n");
    }

    tokio::time::sleep(Duration::from_millis(80)).await;

    // --- ケース 2: 服 -> 着る（着） ---
    {
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let mut buf = [0u8; 512];
        client.write_all("1ふく \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/服/\n");

        tokio::time::sleep(Duration::from_millis(80)).await;

        client.write_all("1きr \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        // 文脈「服」により「着」が第1候補！
        assert_eq!(resp, "1/着/切/伐/\n");
    }

    tokio::time::sleep(Duration::from_millis(80)).await;

    // --- ケース 3: 木 -> 伐る（伐） ---
    {
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let mut buf = [0u8; 512];
        client.write_all("1き \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/木/\n");

        tokio::time::sleep(Duration::from_millis(80)).await;

        client.write_all("1きr \n".as_bytes()).await.unwrap();
        let n = client.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        // 文脈「木」により「伐」が第1候補！
        assert_eq!(resp, "1/伐/着/切/\n");
    }

    proxy_handle.abort();
    upstream_handle.abort();
}

/// context_ranking: false の場合、前回の単語にかかわらず固定頻度順が維持されることを検証するテスト。
#[tokio::test]
async fn test_proxy_context_ranking_disabled() {
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
                        if req.starts_with("1にく ".as_bytes()) {
                            let _ = stream.write_all("1/肉/\n".as_bytes()).await;
                        } else if req.starts_with("1きる ".as_bytes()) {
                            let _ = stream.write_all("1/着る/切る/伐る/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let mut predictor = FrequencyPredictor::new(None);
    // 固定頻度: 着=3, 切=2, 伐=1
    predictor.frequencies.insert("きr".to_string(), {
        let mut m = HashMap::new();
        m.insert("着".to_string(), 3);
        m.insert("切".to_string(), 2);
        m.insert("伐".to_string(), 1);
        m
    });
    predictor.context_frequencies.insert("肉".to_string(), {
        let mut m = HashMap::new();
        m.insert("切る".to_string(), 10);
        m
    });
    predictor.expand_aliases();

    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "mock-fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false, // 文脈連動無効
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all("1にく \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/肉/\n");

    client.write_all("1きr \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    // context_ranking が無効のため、「肉」の後でも「切」は昇格せず、固定頻度順の「着」が第1候補のまま！
    assert_eq!(resp, "1/着/切/伐/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// macSKK の補完バースト（ミリ秒単位の連続 Lookup）が発生しても、
/// 直前の確定単語「肉」が保護され、次の手動変換「きr」で「切」が第1候補になることを検証するテスト。
#[tokio::test]
async fn test_proxy_context_protected_against_completion_burst() {
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
                        if req.starts_with("1にく ".as_bytes()) {
                            let _ = stream.write_all("1/肉/\n".as_bytes()).await;
                        } else if req.starts_with("1にほん ".as_bytes()) {
                            let _ = stream.write_all("1/日本/\n".as_bytes()).await;
                        } else if req.starts_with("1にほんご ".as_bytes()) {
                            let _ = stream.write_all("1/日本語/\n".as_bytes()).await;
                        } else if req.starts_with("1きる ".as_bytes()) {
                            let _ = stream.write_all("1/着る/切る/伐る/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let mut predictor = FrequencyPredictor::new(None);
    predictor.frequencies.insert("きr".to_string(), {
        let mut m = HashMap::new();
        m.insert("着".to_string(), 3);
        m.insert("切".to_string(), 2);
        m.insert("伐".to_string(), 1);
        m
    });
    predictor.context_frequencies.insert("肉".to_string(), {
        let mut m = HashMap::new();
        m.insert("切る".to_string(), 10);
        m
    });
    // 日本語に対しては別の単語（仮）
    predictor.context_frequencies.insert("日本語".to_string(), {
        let mut m = HashMap::new();
        m.insert("着る".to_string(), 50);
        m
    });
    predictor.expand_aliases();

    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "mock-fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: true,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1. ユーザーが手動で「肉」を通常変換（Lookup）
    client.write_all("1にく \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/肉/\n");

    // 2. macSKK がキー入力開始により、10ms 間隔で補完スキャン Lookup を連射！
    tokio::time::sleep(Duration::from_millis(10)).await;
    client.write_all("1にほん \n".as_bytes()).await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;
    client.write_all("1にほんご \n".as_bytes()).await.unwrap();
    let _ = client.read(&mut buf).await.unwrap();

    // 3. ユーザーが手動で「きr」を通常変換（Spaceキー押下、200ms後）
    tokio::time::sleep(Duration::from_millis(200)).await;
    client.write_all("1きr \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    // 補完スキャン「日本語」によって汚染されず、直前の正当な文脈「肉」が保持されたため、
    // 「切」が第1候補として返る！
    assert_eq!(resp, "1/切/着/伐/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 促音便動詞（例: "OmoT;ta" -> "おもt"）で、
/// 促音便 "おもった" 由来の語幹「思」が第1候補として返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_sokuon_omot() {
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
                        if req.starts_with("1おもった ".as_bytes()) {
                            let _ = stream.write_all("1/思った/\n".as_bytes()).await;
                        } else if req.starts_with("1おもって ".as_bytes()) {
                            let _ = stream.write_all("1/思って/想って/\n".as_bytes()).await;
                        } else if req.starts_with("1おもつ ".as_bytes()) {
                            let _ = stream.write_all("1/重つ/\n".as_bytes()).await;
                        } else if req.starts_with("1おもち ".as_bytes()) {
                            let _ = stream.write_all("1/重血/お持ち/お餅/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1おもt " -> "おもった", "おもって" から語幹 "思", "想" が最優先で抽出される
    client.write_all("1おもt \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/思/想/重/重血/お持/お餅/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 拗音縮約動詞（例: "WaraChau" -> "わらc"）で、
/// "わらっちゃう" 由来の語幹「笑」が返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_youon_warac() {
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
                        if req.starts_with("1わらっちゃう ".as_bytes()) {
                            let _ = stream.write_all("1/笑っちゃう/嗤っちゃう/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1わらc " -> "わらっちゃう" から語幹 "笑", "嗤" が抽出される
    client.write_all("1わらc \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/笑/嗤/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 意志・推量拗音動詞（例: "TabeYou" -> "たべy"）で、
/// "たべよう" 由来の語幹「食べ」が返ることを検証する結合テスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_youon_tabey() {
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
                        if req.starts_with("1たべよう ".as_bytes()) {
                            let _ = stream.write_all("1/食べよう/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1たべy " -> "たべよう" から語幹 "食べ" が抽出される
    client.write_all("1たべy \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/食べ/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// 送りキー 'e'（例: "つかe"）から "つかえ" を復元し、語幹 "使" が返ることを検証するテスト。
#[tokio::test]
async fn test_proxy_okuri_expansion_resolves_tsukae() {
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
                        if req.starts_with("1つかえ ".as_bytes()) {
                            let _ = stream.write_all("1/使え/仕え/遣え/支え/\n".as_bytes()).await;
                        } else if req.starts_with("1つかえる ".as_bytes()) {
                            let _ = stream.write_all("1/使える/仕える/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "azookey-mock",
            upstream_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        okuri_expansion: true,
        context_ranking: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // "1つかe " -> "つかえ" から語幹 "使" が抽出される
    client.write_all("1つかe \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp, "1/使/仕/遣/支/\n");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}


