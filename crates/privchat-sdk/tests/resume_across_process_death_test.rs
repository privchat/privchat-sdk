//! 断点续传的**唯一**真门禁：传到一半把进程杀掉，换一个新进程接着传。
//!
//! 🔴 之前那条 `resuming_puts_only_the_missing_bytes_on_the_wire` 证明不了这件事：
//! 它把「已确认 128KiB」当参数喂给 `ResumableUpload`，然后确认它从 128KiB 开始发。
//! 那测的是一个纯函数的算术。整个功能的赌注是**会话记录跨进程还在**——token 落没落
//! 盘、落盘的时机对不对、新进程读不读得回来——这些全在被喂参数的那一步之前，
//! 一条也没被覆盖。上一轮 claim-miss 那条路整整不落盘，284 个测试全绿。
//!
//! 所以这里必须有真的进程死亡：父进程跑 mock server，子进程真的发 HTTP，传到 60%
//! 用 SIGKILL 打掉（不给任何清理机会，模拟 App 被系统杀），再起一个**全新进程**，
//! 只给它同一个缓存目录，看它自己找回去。
//!
//! 判据是「两条命加起来发上线的字节数正好等于文件大小」。少一个字节文件是坏的，
//! 多一个字节就说明省下来的带宽是假的。

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

const TOTAL: usize = 64 * 1024 * 20;
const UNIT: u32 = 64 * 1024;
/// 子进程每片之间歇一下，父进程才有机会在中途开枪。
const CHUNK_PACING: std::time::Duration = std::time::Duration::from_millis(40);

fn blob() -> Vec<u8> {
    (0..TOTAL).map(|i| (i % 251) as u8).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

// ---------------------------------------------------------------- mock server

#[derive(Clone)]
struct ChunkHit {
    offset: u64,
    len: u64,
    /// 第几条命发来的——用来断言「新进程从缺口开始」。
    life: u32,
}

#[derive(Default)]
struct ServerState {
    /// 连续确认到哪里。分片是顺序发的，所以取 max(offset+len) 即可；
    /// 重发一片不会让它前进，于是重复字节会以「线上字节数超标」的形式暴露。
    confirmed: u64,
    chunks: Vec<ChunkHit>,
    completed: bool,
    life: u32,
}

struct MockServer {
    addr: std::net::SocketAddr,
    state: Arc<Mutex<ServerState>>,
}

impl MockServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = Arc::new(Mutex::new(ServerState::default()));
        let st = state.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    let Some((path, body)) = read_request(&mut sock).await else {
                        return;
                    };
                    let payload = {
                        let mut s = st.lock().unwrap();
                        if path.starts_with("/api/app/files/status") {
                            let ranges = if s.confirmed == 0 {
                                "[]".to_string()
                            } else {
                                format!(r#"[{{"offset":0,"len":{}}}]"#, s.confirmed)
                            };
                            format!(
                                r#"{{"code":0,"message":"OK","data":{{"upload_id":"abc","total_size":{TOTAL},"confirmed_ranges":{ranges},"confirmed_bytes":{},"complete":{}}}}}"#,
                                s.confirmed,
                                s.confirmed >= TOTAL as u64
                            )
                        } else if path.starts_with("/api/app/files/chunk") {
                            let offset: u64 = path
                                .split("offset=")
                                .nth(1)
                                .and_then(|v| v.split('&').next())
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0);
                            let life = s.life;
                            s.chunks.push(ChunkHit {
                                offset,
                                len: body.len() as u64,
                                life,
                            });
                            s.confirmed = s.confirmed.max(offset + body.len() as u64);
                            format!(
                                r#"{{"code":0,"message":"OK","data":{{"outcome":"confirmed","confirmed_bytes":{},"complete":{}}}}}"#,
                                s.confirmed,
                                s.confirmed >= TOTAL as u64
                            )
                        } else {
                            s.completed = true;
                            r#"{"code":0,"message":"OK","data":{"file_id":4242,"file_url":"http://x/f","file_size":1,"mime_type":"application/octet-stream","uploaded_at":0,"storage_source_id":0}}"#.to_string()
                        }
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        Self { addr, state }
    }

    fn base(&self) -> String {
        format!("http://{}/api/app/files", self.addr)
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, Vec<u8>)> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let Some(head_end) = find(&buf, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let first = lines.next()?.to_string();
        let path = first.split_whitespace().nth(1)?.to_string();
        let mut headers = HashMap::new();
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let want: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let have = buf.len() - head_end - 4;
        if have >= want {
            return Some((path, buf[head_end + 4..head_end + 4 + want].to_vec()));
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------- child

/// 子进程入口。没有那个环境变量时立刻返回，于是它在正常 `cargo test` 里是个空壳。
///
/// 用「测试二进制重新 exec 自己」而不是单独的 example，是为了让子进程跑的**就是**
/// 这次编译出来的 SDK 代码——example 会引入一条独立的构建路径，改了 SDK 却测到旧
/// 产物这种事，查起来能耗掉一整天。
#[test]
fn child_uploads_until_it_is_killed() {
    let Ok(base) = std::env::var("PCX_RESUME_CHILD_BASE") else {
        return;
    };
    let cache = std::env::var("PCX_RESUME_CHILD_CACHE").expect("cache dir");
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(child_main(base, std::path::PathBuf::from(cache)));
}

async fn child_main(base: String, cache: std::path::PathBuf) {
    use privchat_sdk::resumable_upload::{
        ResumableUpload, UploadPlan, UploadPlanRecord, UploadSessionRecord,
    };

    let data = blob();
    let digest = sha256_hex(&data);
    let sealed = cache.join("body.sealed");
    if !sealed.exists() {
        std::fs::write(&sealed, &data).expect("write sealed");
    }

    let plan = UploadPlan {
        base_unit: UNIT,
        initial_request_size: UNIT,
        // 🔴 这里把请求尺寸钉死在一个网格单元，不让它自适应扩张。
        //
        // 放开的话 20 片会被合并成 5 片大请求，整个上传 0.2 秒就结束，父进程根本
        // 来不及在中途开枪——测试会变成「一条命传完」，而它自称测的是两条命。
        // 分片尺寸的自适应有它自己的测试，这条门禁只关心「杀掉之后能不能接上」。
        max_request_size: UNIT,
        session_threshold: UNIT as u64,
        max_parallel_parts: 1,
    };

    // 这一步就是整条门禁的被测对象：新进程能不能把上次的会话捡回来。
    let resumed = UploadSessionRecord::load(&sealed, "m1").is_some();
    if !resumed {
        let rec = UploadSessionRecord {
            token: "tok".into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            upload_url: format!("{base}/upload"),
            plan: UploadPlanRecord {
                base_unit: plan.base_unit,
                initial_request_size: plan.initial_request_size,
                max_request_size: plan.max_request_size,
                session_threshold: plan.session_threshold,
                max_parallel_parts: plan.max_parallel_parts,
            },
            sealed_sha256: digest.clone(),
            sealed_size: data.len() as u64,
            user_id: 7,
            local_message_id: "m1".into(),
            server_identity: "mock".into(),
        };
        rec.store(&sealed).expect("store session");
    }
    eprintln!("[child] resumed={resumed}");

    let client = reqwest::Client::new();

    // 捡回会话的那条命必须先问服务端「你收到哪儿了」——本地记不得，也不该记：
    // 客户端以为写成功、服务端没落盘的那一片，只有服务端说了算。
    let confirmed: Vec<(u64, u64)> = if resumed {
        let body: serde_json::Value = client
            .get(format!("{base}/status"))
            .header("X-Upload-Token", "tok")
            .header("Authorization", "Bearer u:7")
            .send()
            .await
            .expect("status")
            .json()
            .await
            .expect("status json");
        body["data"]["confirmed_ranges"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| (r["offset"].as_u64().unwrap(), r["len"].as_u64().unwrap()))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    eprintln!("[child] confirmed={confirmed:?}");

    let mut up = ResumableUpload::new(data.len() as u64, plan, &confirmed);
    while let Some(chunk) = up.next_chunk() {
        let piece = &data[chunk.offset as usize..(chunk.offset + chunk.len) as usize];
        let started = std::time::Instant::now();
        client
            .put(format!("{base}/chunk?offset={}", chunk.offset))
            .header("X-Upload-Token", "tok")
            .header("Authorization", "Bearer u:7")
            .header("X-Chunk-SHA256", sha256_hex(piece))
            .body(piece.to_vec())
            .send()
            .await
            .expect("chunk");
        up.on_chunk_ok(chunk, started.elapsed());
        std::io::stderr().flush().ok();
        tokio::time::sleep(CHUNK_PACING).await;
    }

    client
        .post(format!("{base}/complete"))
        .header("X-Upload-Token", "tok")
        .header("Authorization", "Bearer u:7")
        .send()
        .await
        .expect("complete");

    // 传完了会话就该消失。
    UploadSessionRecord::discard(&sealed, "m1");
    eprintln!("[child] done");
}

// --------------------------------------------------------------------- parent

/// 子进程被 SIGKILL 之后，进程表里可能还留着；测试失败时如果不收尸，它会一直
/// 攥着 cargo 的 stdout 管道，整个 `cargo test` 就挂在那里不退出。
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_child(base: &str, cache: &std::path::Path) -> ChildGuard {
    let exe = std::env::current_exe().expect("test binary");
    let child = std::process::Command::new(exe)
        .args(["child_uploads_until_it_is_killed", "--exact", "--nocapture"])
        .env("PCX_RESUME_CHILD_BASE", base)
        .env("PCX_RESUME_CHILD_CACHE", cache)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .expect("spawn child");
    ChildGuard(child)
}

async fn wait_until<F: Fn() -> bool>(what: &str, cond: F) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("等 {what} 超时");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_upload_resumes_in_a_new_process_without_resending_bytes() {
    let server = MockServer::start().await;
    let base = server.base();
    let cache = tempfile::tempdir().expect("cache");

    // ---- 第一条命：传到过半，然后被打掉 ----
    {
        let _child = spawn_child(&base, cache.path());
        let st = server.state.clone();
        let target = (TOTAL as u64) / 2;
        wait_until("上传过半", || st.lock().unwrap().confirmed >= target).await;
        // _child 在这里 drop → SIGKILL。没有析构、没有 flush、没有告别，
        // 跟 App 被系统回收时一模一样。
    }

    let after_first = {
        let mut s = server.state.lock().unwrap();
        let c = s.confirmed;
        s.life = 1;
        c
    };
    assert!(
        after_first > 0 && after_first < TOTAL as u64,
        "第一条命该传了一部分而不是全部，实际 {after_first}/{TOTAL}"
    );

    // 会话记录必须还在盘上——它是第二条命唯一的线索。
    let sealed = cache.path().join("body.sealed");
    assert!(
        privchat_sdk::resumable_upload::UploadSessionRecord::load(&sealed, "m1").is_some(),
        "🔴 进程被杀之后会话记录没了：新进程只能从 0 重传，续传就是假的"
    );

    // ---- 第二条命：全新进程，只给同一个缓存目录 ----
    let _child2 = spawn_child(&base, cache.path());
    wait_until("上传完成", || server.state.lock().unwrap().completed).await;

    let s = server.state.lock().unwrap();
    let on_the_wire: u64 = s.chunks.iter().map(|c| c.len).sum();
    assert_eq!(
        on_the_wire, TOTAL as u64,
        "🔴 两条命加起来发上线 {on_the_wire} 字节，文件只有 {TOTAL} 字节——\
         多出来的部分是被重传的，省下的带宽是假的"
    );

    let second_life_first = s
        .chunks
        .iter()
        .find(|c| c.life == 1)
        .expect("第二条命必须真的发了东西");
    assert_eq!(
        second_life_first.offset, after_first,
        "新进程该从缺口开始，不是从 0"
    );

    // 覆盖必须完整无洞：字节数对了但排布错了，文件同样是坏的。
    let mut covered: Vec<(u64, u64)> = s.chunks.iter().map(|c| (c.offset, c.len)).collect();
    covered.sort();
    let mut cursor = 0u64;
    for (off, len) in covered {
        assert_eq!(off, cursor, "分片之间有洞或重叠");
        cursor += len;
    }
    assert_eq!(cursor, TOTAL as u64, "覆盖没顶到文件末尾");

    // 传完之后会话记录该被清掉：里面那张 token 还能用 24 小时。
    assert!(
        privchat_sdk::resumable_upload::UploadSessionRecord::load(&sealed, "m1").is_none(),
        "上传完成后会话记录仍在盘上"
    );
}
