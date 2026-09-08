use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use ruskk::backend::{Backend, UpstreamEncoding};
use ruskk::encoding::encode_euc_jp;
use ruskk::frequency::{FrequencyPredictor, SharedPredictor};
use ruskk::proxy::Proxy;

#[tokio::test]
async fn test_yaskkserv2_standalone_seed_ranking() {
    // 1. Setup yaskkserv2 mock server (communicating strictly in EUC-JP)
    // Upstream defaults to returning EUC-JP encoded candidates with "切る" first: 1/切る/着る/\n
    let yaskkserv2_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let yaskkserv2_addr = yaskkserv2_listener.local_addr().unwrap();

    let euc_kiru = encode_euc_jp("きr"); // SKK okuri-ari key for "きr"
    let euc_kiru_full = encode_euc_jp("きる");
    let euc_fuku = encode_euc_jp("ふく");
    let euc_niku = encode_euc_jp("にく");

    // EUC-JP encoded responses dynamically generated via encode_euc_jp
    let euc_resp_kiru = encode_euc_jp("1/切る/着る/\n");
    let euc_resp_fuku = encode_euc_jp("1/服/\n");
    let euc_resp_niku = encode_euc_jp("1/肉/\n");

    let yaskkserv2_handle = tokio::spawn(async move {
        loop {
            if let Ok((mut stream, _)) = yaskkserv2_listener.accept().await {
                let euc_kiru = euc_kiru.clone();
                let euc_kiru_full = euc_kiru_full.clone();
                let euc_fuku = euc_fuku.clone();
                let euc_niku = euc_niku.clone();
                let euc_resp_kiru = euc_resp_kiru.clone();
                let euc_resp_fuku = euc_resp_fuku.clone();
                let euc_resp_niku = euc_resp_niku.clone();

                tokio::spawn(async move {
                    let mut buf = [0u8; 512];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        let req = &buf[..n];
                        // Verify that request midashi is encoded in EUC-JP
                        if req.starts_with(b"1") {
                            let operand = &req[1..req.len().saturating_sub(1)]; // Strip 1 and trailing LF/space
                            let operand_trimmed = operand.iter().as_slice();
                            
                            if operand_trimmed.windows(euc_kiru.len()).any(|w| w == euc_kiru.as_slice())
                                || operand_trimmed.windows(euc_kiru_full.len()).any(|w| w == euc_kiru_full.as_slice())
                                || req.starts_with(b"1kiru") || req.starts_with(b"1KiRu")
                            {
                                // yaskkserv2 ALWAYS returns "切る" first!
                                let _ = stream.write_all(&euc_resp_kiru).await;
                            } else if operand_trimmed.windows(euc_fuku.len()).any(|w| w == euc_fuku.as_slice())
                                || req.starts_with(b"1fuku")
                            {
                                let _ = stream.write_all(&euc_resp_fuku).await;
                            } else if operand_trimmed.windows(euc_niku.len()).any(|w| w == euc_niku.as_slice())
                                || req.starts_with(b"1niku")
                            {
                                let _ = stream.write_all(&euc_resp_niku).await;
                            } else {
                                let _ = stream.write_all(b"4\n").await;
                            }
                        }
                    }
                });
            }
        }
    });

    // 2. Setup FrequencyPredictor seed data (mocking ~/.ruskk-frequency.json)
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

    // Primary is down/unreachable (simulating azooKey not running or miss)
    // Fallback is yaskkserv2 with UpstreamEncoding::EucJp
    let proxy = Proxy {
        listen: proxy_addr.to_string(),
        primary: Backend {
            name: "azookey-primary-down".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            encoding: UpstreamEncoding::Utf8,
            timeout: Duration::from_millis(50),
        },
        fallback: Backend {
            name: "yaskkserv2-fallback".into(),
            addr: yaskkserv2_addr,
            encoding: UpstreamEncoding::EucJp,
            timeout: Duration::from_secs(1),
        },
        predictor: shared_predictor.clone(),
    };

    let proxy_handle = tokio::spawn(async move {
        let _ = proxy.run().await;
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // 3. Client connection with context "服" -> lookup "きr"
    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    let mut buf = [0u8; 512];

    // Client requests "ふく" (UTF-8) -> yaskkserv2 returns "服"
    client.write_all("1ふく \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp1 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp1, "1/服/\n");

    // seed データに「服」→「着る」が定義されているため、
    // yaskkserv2 の返却順（切る/着る）に関わらず、「着る」が第1候補に並び替えられる！
    client.write_all("1きr \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp2 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp2, "1/着る/切る/\n");

    // 続いて "にく" を変換
    client.write_all("1にく \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp3 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp3, "1/肉/\n");

    // seed データに「肉」→「切る」が定義されているため、今度は「切る」が第1候補になる！
    client.write_all("1きr \n".as_bytes()).await.unwrap();
    let n = client.read(&mut buf).await.unwrap();
    let resp4 = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(resp4, "1/切る/着る/\n");

    drop(client);
    proxy_handle.abort();
    yaskkserv2_handle.abort();
}
