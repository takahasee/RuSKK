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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
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
    assert_eq!(
        resp, "1/kiru/kiku/\n",
        "upstream からの補完候補が返却されること"
    );

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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true, // 送り復元有効
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
    assert!(
        resp1b.starts_with("1/交ぜ書/"),
        "unexpected resp1b: {}",
        resp1b
    );

    // 2. 平仮名送りあり複合語 "1まぜがk " -> 語幹 "混ぜ書", "交ぜ書" が返る
    client.write_all("1まぜがk \n".as_bytes()).await.unwrap();
    let n2 = client.read(&mut buf).await.unwrap();
    let resp2 = String::from_utf8_lossy(&buf[..n2]);
    assert!(
        resp2.starts_with("1/混ぜ書/"),
        "unexpected resp2: {}",
        resp2
    );

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
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        // 複合語として優先展開された "といあわす", "といあわせ" に対する upstream レスポンス
                        if req.starts_with("1といあわす ".as_bytes()) {
                            let _ = stream.write_all("1/問い合わす/\n".as_bytes()).await;
                        } else if req.starts_with("1といあわせ ".as_bytes()) {
                            let _ = stream
                                .write_all("1/問い合わせ/問合せ/問い合せ/問合わせ/\n".as_bytes())
                                .await;
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
    assert!(
        resp.contains("/問い合わ/"),
        "expected '問い合わ' in resp: {}",
        resp
    );

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
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1かきおこす ".as_bytes()) {
                            let _ = stream
                                .write_all("1/書き起こす/書き起す/書起こす/\n".as_bytes())
                                .await;
                        } else if req.starts_with("1かきおこし ".as_bytes()) {
                            let _ = stream
                                .write_all("1/書き起こし/掻き起し/\n".as_bytes())
                                .await;
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
    assert!(
        resp.contains("/書き起こ/"),
        "expected '書き起こ' in resp: {}",
        resp
    );
    assert!(
        resp.contains("/書起こ/"),
        "expected '書起こ' in resp: {}",
        resp
    );
    assert!(
        resp.contains("/掻き起/"),
        "expected '掻き起' in resp: {}",
        resp
    );

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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
    assert!(
        resp.starts_with("1/値上/"),
        "expected '値上' as top candidate, got: {}",
        resp
    );

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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: false, // 送り復元無効（従来動作）
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: true,
        annotation_priority: false,
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
    // macSKK は ▽ 開始時に先頭 1 文字を機械的に Lookup し、Space で再度 Lookup する。
    // 「き」(▽開始) →「き」(Space・確定) →「き」(次の ▽ 開始) →「きr」の順に届く。
    {
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let mut buf = [0u8; 512];
        for _ in 0..3 {
            client.write_all("1き \n".as_bytes()).await.unwrap();
            let n = client.read(&mut buf).await.unwrap();
            assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/木/\n");
            tokio::time::sleep(Duration::from_millis(80)).await;
        }

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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false, // 文脈連動無効
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: true,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1わらっちゃう ".as_bytes()) {
                            let _ = stream
                                .write_all("1/笑っちゃう/嗤っちゃう/\n".as_bytes())
                                .await;
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
                        if n == 0 {
                            break;
                        }
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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
                            let _ = stream
                                .write_all("1/使え/仕え/遣え/支え/\n".as_bytes())
                                .await;
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
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: false,
        annotation_priority: false,
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

/// annotation_priority が有効な場合、システム辞書掲載語や注釈付き語が優先昇格し、
/// 無効（false）にした場合は元のバックエンド返却順が維持されることを検証するテスト。
#[tokio::test]
async fn test_proxy_annotation_priority() {
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
                        if req.starts_with(b"1\xe3\x81\xa8\xe3\x81\x86\xe3\x81\x8d\xe3\x82\x87\xe3\x81\x86 ") {
                            // "1とうきょう " -> 辞書外の造語が先頭、2番目が東京、3番目が注釈付き
                            let _ = stream
                                .write_all("1/謎造語xyz/東京/時間;時刻/\n".as_bytes())
                                .await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let shared_predictor: SharedPredictor =
        Arc::new(std::sync::RwLock::new(FrequencyPredictor::new(None)));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    // 1. annotation_priority = true のプロキシ
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
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: true, // システム辞書・注釈優先有効
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // CoreServices DictionaryServices の初回フレームワーク初期化（~700-900ms）を無関係な語で済ませ、
    // 本番デーモン起動後と同じ状態で「謎造語xyz」と「東京」の未キャッシュ比較を検証する
    let _ = ruskkserv::system_dict::is_annotated_or_in_dict("日本", None);

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    client.write_all("1とうきょう \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    // 謎造語xyz は最下位（末尾）に沈み、東京および時間;時刻が上位に来ていること
    assert!(resp.starts_with("1/"), "unexpected resp: {resp:?}");
    assert!(resp.ends_with("/謎造語xyz/\n"), "unexpected resp: {resp:?}");

    drop(client);
    proxy_handle.abort();
    upstream_handle.abort();
}

/// Fallback (yaskkserv2) の個別タイムアウト設定が overall_deadline (850ms) より短い場合、
/// 850ms まで待たずに指定した fallback.timeout で速やかにタイムアウトすることを検証するテスト。
#[tokio::test]
async fn test_proxy_respects_fallback_timeout() {
    // Primary は即座に 4\n (miss) を返すモック
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();
    let primary_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = primary_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let _ = stream.write_all(b"4\n").await;
                    }
                });
            }
        }
    });

    // Fallback は接続を受け付けるが応答しない（ハング状態をシミュレート）
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();
    let fallback_handle = tokio::spawn(async move {
        let mut held_streams = Vec::new();
        while let Ok((stream, _)) = fallback_listener.accept().await {
            held_streams.push(stream);
        }
    });

    let shared_predictor: SharedPredictor =
        Arc::new(std::sync::RwLock::new(FrequencyPredictor::new(None)));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary-miss",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(200),
        ),
        fallback: Backend::new(
            "mock-fallback-hang",
            fallback_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(120), // 850ms より十分短い 120ms を指定
        ),
        predictor: shared_predictor,
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    let start = tokio::time::Instant::now();
    client.write_all("1みとうろく \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(&buf[..n], b"4\n");
    assert!(
        elapsed < Duration::from_millis(450),
        "fallback timeout (120ms) should trigger well before 850ms overall deadline, took {:?}",
        elapsed
    );

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
    fallback_handle.abort();
}

/// 送りなし見出し（「がそ」「みぎや」「げそ」等）で Primary (azooKey) が送り仮名・助詞混じり断片（貸そ/右や/解そ）を
/// 先頭に返したり、frequencies の登録候補（→）を欠いている場合、
/// Fallback (yaskkserv2) とマージ・再整列して「画素」「→」「下足」が第1候補になることを検証するテスト。
#[tokio::test]
async fn test_proxy_okuri_nashi_demotes_fragments_and_merges_fallback() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();
    let primary_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = primary_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1がそ ".as_bytes()) {
                            let _ = stream.write_all("1/貸そ/画素/\n".as_bytes()).await;
                        } else if req.starts_with("1みぎや ".as_bytes()) {
                            let _ = stream.write_all("1/右矢/右や/\n".as_bytes()).await;
                        } else if req.starts_with("1げそ ".as_bytes()) {
                            let _ = stream.write_all("1/解そ/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();
    let fallback_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = fallback_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1がそ ".as_bytes()) {
                            let _ = stream.write_all("1/画素/\n".as_bytes()).await;
                        } else if req.starts_with("1みぎや ".as_bytes()) {
                            let _ = stream.write_all("1/→/⇒/右矢/右谷/\n".as_bytes()).await;
                        } else if req.starts_with("1げそ ".as_bytes()) {
                            let _ = stream.write_all("1/下足/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let mut predictor = FrequencyPredictor::new(None);
    predictor.frequencies.insert("みぎや".to_string(), {
        let mut m = HashMap::new();
        m.insert("→".to_string(), 1);
        m
    });
    predictor.expand_aliases();
    let shared_predictor: SharedPredictor = Arc::new(std::sync::RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(200),
        ),
        fallback: Backend::new(
            "mock-fallback",
            fallback_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(200),
        ),
        predictor: shared_predictor,
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: true,
        annotation_priority: true,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // CoreServices DictionaryServices の初回フレームワーク初期化を無関係な語で済ませ、
    // 単独実行時・並列実行時のいずれでも初回変換デッドライン (850ms) 内で通常辞書判定を検証する
    let _ = ruskkserv::system_dict::is_annotated_or_in_dict("日本", None);

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1. 「がそ」: 「貸そ」ではなく「画素」が第1候補になること
    client.write_all("1がそ \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp_gaso = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp_gaso, "1/画素/貸そ/\n");

    // 2. 「みぎや」: Fallback の「→」がマージされて第1候補になり、「右や」が最下位に沈むこと
    client.write_all("1みぎや \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp_migiya = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp_migiya, "1/→/右矢/⇒/右谷/右や/\n");

    // 3. 「げそ」: Primary が単一断片「解そ」しか返さなくても、Fallback の「下足」がマージされ第1候補になること
    client.write_all("1げそ \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp_geso = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp_geso, "1/下足/解そ/\n");

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
    fallback_handle.abort();
}

/// 接頭語（`midashi>`）および接尾語（`>midashi`）が通常変換と混同されず、
/// SKK 辞書を持つ Fallback (yaskkserv2) へ優先ルーティングされること、および
/// 送りなし複合名詞（`せつびご`）で Primary が辞書外の誤結合（`設備後`）を返した際に Fallback（`接尾語`）がマージされることを検証するテスト。
#[tokio::test]
async fn test_proxy_prefix_and_suffix_routing() {
    // Primary (azooKey 模擬): `>` 付き見出しを送ると `>` が候補に混入する挙動を再現
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();
    let primary_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = primary_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1さい> ".as_bytes()) {
                            let _ = stream.write_all("1/際>/\n".as_bytes()).await;
                        } else if req.starts_with("1>せい ".as_bytes()) {
                            let _ = stream.write_all("1/>製/\n".as_bytes()).await;
                        } else if req.starts_with("1みとうろく ".as_bytes()) {
                            let _ = stream.write_all("1/未登録/\n".as_bytes()).await;
                        } else if req.starts_with("1せつびご ".as_bytes()) {
                            let _ = stream.write_all("1/設備後/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    // Fallback (yaskkserv2 模擬): SKK-JISYO の接頭語・接尾語エントリおよび複合語を持つ
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();
    let fallback_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = fallback_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        if req.starts_with("1さい> ".as_bytes()) {
                            let _ = stream.write_all("1/再/最/歳;?/\n".as_bytes()).await;
                        } else if req.starts_with("1>せい ".as_bytes()) {
                            let _ = stream.write_all("1/性/制/製/生/\n".as_bytes()).await;
                        } else if req.starts_with("1>わたs ".as_bytes()) {
                            let _ = stream.write_all("1/渡/[す/渡/]/\n".as_bytes()).await;
                        } else if req.starts_with("4>せ ".as_bytes()) {
                            let _ = stream.write_all("1/>せい/>せいけん/\n".as_bytes()).await;
                        } else if req.starts_with("1せつびご ".as_bytes()) {
                            let _ = stream.write_all("1/接尾語/\n".as_bytes()).await;
                        } else {
                            let _ = stream.write_all(b"4\n").await;
                        }
                    }
                });
            }
        }
    });

    let predictor = FrequencyPredictor::new(None);
    let shared_predictor: SharedPredictor = Arc::new(std::sync::RwLock::new(predictor));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(200),
        ),
        fallback: Backend::new(
            "mock-fallback",
            fallback_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(200),
        ),
        predictor: shared_predictor,
        lookup_cache: Default::default(),
        okuri_expansion: true,
        context_ranking: true,
        annotation_priority: true,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // CoreServices DictionaryServices の初回フレームワーク初期化を無関係な語で済ませ、
    // 単独実行時・並列実行時のいずれでも初回変換デッドライン (850ms) 内で通常辞書判定を検証する
    let _ = ruskkserv::system_dict::is_annotated_or_in_dict("日本", None);

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1. 接頭語 `さい>`: Fallback (yaskkserv2) の `再/最/歳;?` が返り、`歳;?` の注釈で `再` が追い越されないこと
    client.write_all("1さい> \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/再/最/歳;?/\n");

    // 2. 接尾語 `>せい`: Fallback (yaskkserv2) の `性/制/製/生` が返り、`>` が混入しないこと
    client.write_all("1>せい \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/性/制/製/生/\n");

    // 3. 送りあり接尾語 `>わたs`: 送りあり通常動詞と混同されず、角括弧ブロックが展開されて `1/渡/\n` になること
    client.write_all("1>わたs \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/渡/\n");

    // 4. Fallback にない接尾語 `>みとうろく`: `>` を除去した内部読み `みとうろく` で Primary にフォールバックすること
    client
        .write_all("1>みとうろく \n".as_bytes())
        .await
        .unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/未登録/\n");

    // 5. 接尾語補完 `4>せ`: Fallback (yaskkserv2) の見出し補完が返ること
    client.write_all("4>せ \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/>せい/>せいけん/\n");

    // 6. 通常複合名詞 `せつびご`: Primary が大辞林未掲載の `設備後` を返した際、Fallback の `接尾語` がマージされ第1候補になること
    tokio::time::sleep(Duration::from_millis(60)).await;
    client.write_all("1せつびご \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/接尾語/設備後/\n");

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
    fallback_handle.abort();
}

#[tokio::test]
async fn test_proxy_lookup_cache_hits_and_avoids_upstream() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let primary_req_count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&primary_req_count);

    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();

    let primary_handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = primary_listener.accept().await {
            let count = Arc::clone(&count_clone);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    let line = String::from_utf8_lossy(&buf[..n]);
                    if line.contains("にほん") {
                        let _ = stream.write_all("1/日本/二本/\n".as_bytes()).await;
                    } else if line.contains("きr") || line.contains("きる") {
                        let _ = stream.write_all("1/切/着/\n".as_bytes()).await;
                    } else {
                        let _ = stream.write_all(b"4\n").await;
                    }
                }
            });
        }
    });

    let shared_predictor: SharedPredictor = Arc::new(std::sync::RwLock::new(FrequencyPredictor::new(None)));
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let lookup_cache = Arc::new(ruskkserv::cache::LookupCache::default());

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-primary",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_secs(1),
        ),
        fallback: Backend::new(
            "mock-fallback",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        lookup_cache: Arc::clone(&lookup_cache),
        okuri_expansion: false,
        context_ranking: true,
        annotation_priority: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1回目照会: Primary に問い合わせが発生
    client.write_all("1にほん \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/日本/二本/\n");
    let count_after_first = primary_req_count.load(Ordering::SeqCst);
    assert!(count_after_first >= 1);

    // 2回目照会: キャッシュヒットにより Primary への追加照会ゼロ！
    client.write_all("1にほん \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/日本/二本/\n");
    assert_eq!(
        primary_req_count.load(Ordering::SeqCst),
        count_after_first,
        "2回目の照会はキャッシュヒットし、Primary への照会数は増加しないこと"
    );

    // キャッシュをクリア
    lookup_cache.clear();

    // 3回目照会: キャッシュクリア後のため、再び Primary へ照会が行われる
    client.write_all("1にほん \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/日本/二本/\n");
    assert_eq!(
        primary_req_count.load(Ordering::SeqCst),
        count_after_first + 1,
        "キャッシュクリア後の照会は Primary への照会数が増加すること"
    );

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
}

/// Primary が 350ms 遅延してミス (`4\n`) を返し、Fallback が 550ms かかる場合でも、
/// 200ms 経過時点での並列ヘッジ照会により overall_deadline (850ms) 内に初回変換が成功することを検証する。
/// また Primary が遅延後にヒットした場合は、従来通り Primary が優先されることも確認する。
#[tokio::test]
async fn test_proxy_hedged_fallback_resolves_slow_first_conversion_within_deadline() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();

    let primary_handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = primary_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    tokio::time::sleep(Duration::from_millis(350)).await;
                    if req.contains("おそいかち") {
                        let _ = stream.write_all("1/遅い勝ち/\n".as_bytes()).await;
                    } else {
                        let _ = stream.write_all(b"4\n").await;
                    }
                }
            });
        }
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_addr = fallback_listener.local_addr().unwrap();

    let fallback_handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = fallback_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if req.contains("おそいかち") {
                        let _ = stream.write_all("1/フォールバック側/\n".as_bytes()).await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(520)).await;
                        let _ = stream.write_all("1/バックアップ/\n".as_bytes()).await;
                    }
                }
            });
        }
    });

    let shared_predictor: SharedPredictor =
        Arc::new(std::sync::RwLock::new(FrequencyPredictor::new(None)));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-slow-primary",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(600),
        ),
        fallback: Backend::new(
            "mock-slow-fallback",
            fallback_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(850),
        ),
        predictor: shared_predictor,
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: false,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // 1. Primary が 350ms でミス + Fallback が 520ms でも、200ms 時点からのヘッジ並列化により
    //    直列合計 (870ms > 850ms) を下回る ~720ms で初回変換がヒットすること
    let start = tokio::time::Instant::now();
    client.write_all("1ばっくあっぷ \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let elapsed = start.elapsed();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/バックアップ/\n");
    assert!(
        elapsed < Duration::from_millis(840),
        "hedged fallback should finish within overall_deadline, took {:?}",
        elapsed
    );

    // 2. Fallback が高速に返せても、Primary がタイムアウト内 (350ms) でヒットすれば Primary が勝つこと
    client.write_all("1おそいかち \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    assert_eq!(String::from_utf8_lossy(&buf[..n]), "1/遅い勝ち/\n");

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
    fallback_handle.abort();
}

/// バックエンド応答が overall_deadline (850ms) 直前に到達しても、
/// 同期システム辞書 FFI が予算超過時にスキップされるため、初回変換が 1.0 秒タイムアウトを超えずに応答できることを検証する。
#[tokio::test]
async fn test_proxy_caps_cold_ffi_within_overall_deadline() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_addr = primary_listener.local_addr().unwrap();

    let primary_handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = primary_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 512];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    // FFI 安全マージン（790ms）付近の 780ms 遅延で応答を返す
                    tokio::time::sleep(Duration::from_millis(780)).await;
                    let _ = stream
                        .write_all("1/未知初回語A91/未知初回語B92/未知初回語C93/\n".as_bytes())
                        .await;
                }
            });
        }
    });

    let shared_predictor: SharedPredictor =
        Arc::new(std::sync::RwLock::new(FrequencyPredictor::new(None)));

    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = proxy_listener.local_addr().unwrap();
    drop(proxy_listener);

    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend::new(
            "mock-near-deadline-primary",
            primary_addr,
            UpstreamEncoding::Utf8,
            Duration::from_millis(850),
        ),
        fallback: Backend::new(
            "mock-fallback-down",
            "127.0.0.1:1".parse().unwrap(),
            UpstreamEncoding::Utf8,
            Duration::from_millis(50),
        ),
        predictor: shared_predictor,
        lookup_cache: Default::default(),
        okuri_expansion: false,
        context_ranking: false,
        annotation_priority: true,
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    let start = tokio::time::Instant::now();
    client.write_all("1ちえんご \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(
        String::from_utf8_lossy(&buf[..n]),
        "1/未知初回語A91/未知初回語B92/未知初回語C93/\n"
    );
    assert!(
        elapsed < Duration::from_millis(920),
        "cold FFI must stay capped inside overall_deadline, took {:?}",
        elapsed
    );

    drop(client);
    proxy_handle.abort();
    primary_handle.abort();
}
