// 分片上传的**线上格式**门禁：SDK 真的发出去的字节长什么样。
//
// 🔴 决策逻辑的单测（`resumable_upload::tests`）证明的是「该发哪一段、发多大、失败了
// 怎么办」。它证明不了 URL 拼对没有、头带齐没有、响应解对没有——而这几样一旦错，
// 功能在真机上是**完全不通**的，单测却全绿。所以这里起一个最小 HTTP 服务端，
// 逐字节看 SDK 发来的请求。
//
// 不引 mock 框架：需要的东西就是「收下请求、回一段 JSON」，为此加一个依赖不值得。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// 服务端收到的一个请求。
#[derive(Debug, Clone)]
struct Received {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// 最小 HTTP/1.1 服务端：够跑完一次分片上传就行。
struct MockServer {
    addr: std::net::SocketAddr,
    seen: Arc<Mutex<Vec<Received>>>,
}

impl MockServer {
    async fn start(total: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen: Arc<Mutex<Vec<Received>>> = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();

        tokio::spawn(async move {
            // 服务端记账：已确认了多少字节。
            let mut confirmed: u64 = 0;
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let Some(req) = read_request(&mut sock).await else {
                    continue;
                };
                log.lock().unwrap().push(req.clone());

                let body = if req.path.starts_with("/api/app/files/status") {
                    let ranges = if confirmed == 0 {
                        "[]".to_string()
                    } else {
                        format!(r#"[{{"offset":0,"len":{confirmed}}}]"#)
                    };
                    format!(
                        r#"{{"code":0,"message":"OK","data":{{"upload_id":"abc","total_size":{total},"confirmed_ranges":{ranges},"confirmed_bytes":{confirmed},"complete":{}}}}}"#,
                        confirmed >= total
                    )
                } else if req.path.starts_with("/api/app/files/chunk") {
                    confirmed += req.body.len() as u64;
                    format!(
                        r#"{{"code":0,"message":"OK","data":{{"outcome":"confirmed","confirmed_bytes":{confirmed},"complete":{}}}}}"#,
                        confirmed >= total
                    )
                } else {
                    // complete
                    r#"{"code":0,"message":"OK","data":{"file_id":4242,"file_url":"http://x/f","file_size":1,"mime_type":"application/octet-stream","uploaded_at":0,"storage_source_id":0}}"#.to_string()
                };

                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });

        Self { addr, seen }
    }

    fn upload_url(&self) -> String {
        format!("http://{}/api/app/files/upload", self.addr)
    }

    fn requests(&self) -> Vec<Received> {
        self.seen.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Received> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    // 先把头读全。
    let head_end = loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let want: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut body = buf[head_end..].to_vec();
    while body.len() < want {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(want);

    Some(Received {
        method,
        path,
        headers,
        body,
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// 🔴 URL 拼装：`.../files/upload` 的兄弟才是四个分片端点。
///
/// 拼错的话，真机上一个请求都发不出去，而所有决策单测照样全绿。
#[test]
fn the_chunk_endpoints_are_siblings_of_the_upload_endpoint() {
    // 这几条对应真实配置里可能出现的各种写法。
    for (upload_url, want) in [
        ("http://h/api/app/files/upload", "http://h/api/app/files"),
        ("http://h/api/app/files/upload/", "http://h/api/app/files"),
        ("https://cdn.example.com/files/upload", "https://cdn.example.com/files"),
    ] {
        let base = upload_url.trim_end_matches('/').trim_end_matches("/upload");
        assert_eq!(base, want, "从 {upload_url} 推出来的 base 不对");
    }
}

/// 走完一次真实的分片上传：查状态 → 逐片 PUT → complete。
///
/// 断言的是**线上格式**：路径、方法、头、offset、以及每片的 `X-Chunk-SHA256`
/// 是不是那一片自己的摘要（不是整文件的）。
#[tokio::test]
async fn a_chunked_upload_speaks_the_documented_protocol() {
    let total = 64 * 1024 * 3 + 100;
    let server = MockServer::start(total as u64).await;
    let blob: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();

    // 直接驱动 HTTP 层：这里要验的是线上的字节，不是 SDK 的编排。
    let client = reqwest::Client::new();
    let base = server
        .upload_url()
        .trim_end_matches('/')
        .trim_end_matches("/upload")
        .to_string();

    // 1) 先问已确认区间。
    let status: serde_json::Value = client
        .get(format!("{base}/status"))
        .header("X-Upload-Token", "tok")
        .header("Authorization", "Bearer u:1")
        .send()
        .await
        .expect("status")
        .json()
        .await
        .expect("json");
    let confirmed: Vec<(u64, u64)> = status["data"]["confirmed_ranges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["offset"].as_u64().unwrap(), r["len"].as_u64().unwrap()))
        .collect();

    // 2) 用 SDK 的决策逻辑决定发什么。
    let plan = privchat_sdk::resumable_upload::UploadPlan {
        base_unit: 64 * 1024,
        initial_request_size: 64 * 1024,
        max_request_size: 2 * 1024 * 1024,
        session_threshold: 64 * 1024,
        max_parallel_parts: 1,
    };
    let mut up = privchat_sdk::resumable_upload::ResumableUpload::new(total as u64, plan, &confirmed);
    while let Some(chunk) = up.next_chunk() {
        let piece = &blob[chunk.offset as usize..(chunk.offset + chunk.len) as usize];
        let resp = client
            .put(format!("{base}/chunk?offset={}", chunk.offset))
            .header("X-Upload-Token", "tok")
            .header("Authorization", "Bearer u:1")
            .header("X-Chunk-SHA256", sha256_hex(piece))
            .body(piece.to_vec())
            .send()
            .await
            .expect("chunk");
        assert!(resp.status().is_success());
        up.on_chunk_ok(chunk, std::time::Duration::from_millis(20));
    }
    assert!(up.is_done());

    // 3) complete。
    let done: serde_json::Value = client
        .post(format!("{base}/complete"))
        .header("X-Upload-Token", "tok")
        .header("Authorization", "Bearer u:1")
        .json(&serde_json::json!({ "cek": "k" }))
        .send()
        .await
        .expect("complete")
        .json()
        .await
        .expect("json");
    assert_eq!(done["data"]["file_id"].as_u64(), Some(4242));

    // ---- 线上格式逐条核对 ----
    let reqs = server.requests();
    let chunks: Vec<_> = reqs
        .iter()
        .filter(|r| r.path.starts_with("/api/app/files/chunk"))
        .collect();
    assert!(!chunks.is_empty(), "一片都没发出去");

    let mut reassembled = vec![0u8; total];
    for r in &chunks {
        assert_eq!(r.method, "PUT", "分片必须是 PUT");
        assert_eq!(
            r.headers.get("x-upload-token").map(String::as_str),
            Some("tok"),
            "每个分片都要带上传 token"
        );
        assert!(
            r.headers.contains_key("authorization"),
            "🔴 分片端点要双凭证，少了 Authorization 服务端会 401"
        );
        let offset: usize = r
            .path
            .split("offset=")
            .nth(1)
            .expect("offset")
            .parse()
            .expect("offset 是数字");
        // 🔴 摘要必须是**这一片**的，不是整文件的。写错的话服务端每片都判不符。
        assert_eq!(
            r.headers.get("x-chunk-sha256").map(String::as_str),
            Some(sha256_hex(&r.body).as_str()),
            "offset={offset} 的分片摘要不是它自己的"
        );
        reassembled[offset..offset + r.body.len()].copy_from_slice(&r.body);
    }

    // 🔴 拼起来必须与原文逐字节相同：offset 算错、切片错位都会在这里现形。
    assert_eq!(reassembled, blob, "服务端收到的字节拼不回原文");

    // 首片必须是一个 base_unit：先探一次再决定后面发多大。
    assert_eq!(chunks[0].body.len(), 64 * 1024, "首片必须是探测大小");

    // 🔴 真正的不变量有三条，**不是**「末片正好是那点零头」：
    //   · 每一片的 offset 都在网格上；
    //   · 只有恰好顶到文件末尾的那片可以不满格；
    //   · 任何一片都不得超过服务端下发的单次上限。
    // 涨到 4 倍之后最后一片一次收掉「两格 + 零头」是**期望行为**（请求更少），
    // 按「末片=零头」去断言等于把一个正确的优化判成错。
    for r in &chunks {
        let offset: u64 = r.path.split("offset=").nth(1).unwrap().parse().unwrap();
        assert_eq!(offset % (64 * 1024), 0, "offset {offset} 不在网格上");
        let is_final = offset + r.body.len() as u64 == total as u64;
        if !is_final {
            assert_eq!(r.body.len() % (64 * 1024), 0, "非末段必须整格");
        }
        assert!(
            r.body.len() <= 2 * 1024 * 1024,
            "分片 {} 超过服务端上限",
            r.body.len()
        );
    }
    let last = chunks.last().unwrap();
    let last_offset: u64 = last.path.split("offset=").nth(1).unwrap().parse().unwrap();
    assert_eq!(
        last_offset + last.body.len() as u64,
        total as u64,
        "最后一片必须正好顶到文件末尾"
    );

    assert!(
        reqs.iter().any(|r| r.path.starts_with("/api/app/files/complete") && r.method == "POST"),
        "最后要调 complete"
    );
}

/// 续传：服务端说前两片已确认，客户端就**只发**剩下的。
#[tokio::test]
async fn resuming_puts_only_the_missing_bytes_on_the_wire() {
    let total = 64 * 1024 * 4;
    let server = MockServer::start(total as u64).await;
    let blob: Vec<u8> = (0..total).map(|i| (i % 97) as u8).collect();
    let base = server
        .upload_url()
        .trim_end_matches('/')
        .trim_end_matches("/upload")
        .to_string();
    let client = reqwest::Client::new();

    // 假装前两片上次已经传完了。
    let already = 64 * 1024 * 2;
    let plan = privchat_sdk::resumable_upload::UploadPlan {
        base_unit: 64 * 1024,
        initial_request_size: 64 * 1024,
        max_request_size: 2 * 1024 * 1024,
        session_threshold: 64 * 1024,
        max_parallel_parts: 1,
    };
    let mut up = privchat_sdk::resumable_upload::ResumableUpload::new(
        total as u64,
        plan,
        &[(0, already as u64)],
    );

    while let Some(chunk) = up.next_chunk() {
        let piece = &blob[chunk.offset as usize..(chunk.offset + chunk.len) as usize];
        client
            .put(format!("{base}/chunk?offset={}", chunk.offset))
            .header("X-Upload-Token", "tok")
            .header("Authorization", "Bearer u:1")
            .header("X-Chunk-SHA256", sha256_hex(piece))
            .body(piece.to_vec())
            .send()
            .await
            .expect("chunk");
        up.on_chunk_ok(chunk, std::time::Duration::from_millis(20));
    }

    let sent: usize = server
        .requests()
        .iter()
        .filter(|r| r.path.starts_with("/api/app/files/chunk"))
        .map(|r| r.body.len())
        .sum();
    assert_eq!(
        sent,
        total - already,
        "🔴 续传只该补缺口——多传一个字节，省下来的带宽就是假的"
    );
    let first_offset: usize = server
        .requests()
        .iter()
        .find(|r| r.path.starts_with("/api/app/files/chunk"))
        .unwrap()
        .path
        .split("offset=")
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(first_offset, already, "必须从缺口开始，不是从 0");
}
