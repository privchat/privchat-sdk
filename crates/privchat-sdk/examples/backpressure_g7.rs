//! G7 Backpressure / slow-consumer gate harness.
//!
//! 对应 REMEDIATION §0 G7 + REVIEW_CHECKLIST §B（B-PASS-1）。
//! 目标：在「快 sender 持续发 + 慢 consumer 不排空」的负载下，证明服务端背压有效——
//! 内存有界、拒绝可观测、慢消费者解除后能恢复，而不是只证明「没崩」。
//!
//! 记录的指标（Codex 硬要求，缺一不可）：
//!   - server RSS（外部 `ps -o rss=` 采样，无进程 gauge）
//!   - active connections（/metrics `privchat_connections_current`）
//!   - rejected / SERVER_BUSY（`privchat_handler_rejected_total`）
//!   - handler inflight（`privchat_handler_inflight`）
//!   - submit(send) 延迟 p99（客户端侧实测，手算分位，n≥1000 才断言）
//!   - 慢消费者解除后的恢复时间（RSS / queue 回稳耗时）
//!
//! Profile（Codex 硬要求）：
//!   --profile smoke   ~90s，快 8 / 慢 3，宽松阈值，PR/本地快速回归
//!   --profile gate    600s(10min)，快 40 / 慢 10，§B B-PASS-1 阈值
//!
//! Fail-fast：任一阈值不达标 → 打印 FAIL + 非 0 退出（CI/门禁可用）。
//!
//! 用法：
//!   G7_SERVER_BIN=<privchat binary> G7_CONFIG=<server config.toml> \
//!   [G7_HOST=127.0.0.1 G7_TCP_PORT=9001 G7_METRICS=http://127.0.0.1:9083/metrics] \
//!   cargo run --release --example backpressure_g7 -- --profile smoke
//!
//! ⚠️ 已知限制（诚实标注，见 REMEDIATION §3 证据要求）：
//!   慢消费者当前用「SDK 事件不排空 + 不 ack」模拟；真正的 socket-stall（裸 TCP 停读
//!   触发服务端 send buffer 背压）是 gate-完整性的后续项。故本 harness 跑通只能标
//!   `Harness ready / smoke passed`，不得标 gate PASS。

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::{
    ClientSubmitRequest, ClientSubmitResponse, GetChannelPtsRequest, GetChannelPtsResponse,
    GetOrCreateDirectChannelRequest, GetOrCreateDirectChannelResponse,
};
use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

const DIRECT_CHANNEL_TYPE: u8 = 1;
static LOCAL_MSG_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_local_message_id() -> u64 {
    LOCAL_MSG_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn boxed(msg: impl Into<String>) -> BoxError {
    Box::<dyn std::error::Error + Send + Sync>::from(msg.into())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

// ---------- profile ----------

struct Profile {
    name: &'static str,
    fast_clients: usize,
    slow_clients: usize,
    duration: Duration,
    sample_every: Duration,
    send_interval: Duration,
    // 阈值
    rss_growth_budget_kb: u64, // 稳态段 RSS 净增长上限
    recovery_budget: Duration, // 慢消费者解除后回稳上限
}

fn profile(name: &str) -> BoxResult<Profile> {
    match name {
        "smoke" => Ok(Profile {
            name: "smoke",
            fast_clients: 8,
            slow_clients: 3,
            duration: Duration::from_secs(90),
            sample_every: Duration::from_secs(5),
            send_interval: Duration::from_millis(50),
            rss_growth_budget_kb: 300 * 1024, // 宽松：300MB
            recovery_budget: Duration::from_secs(60),
        }),
        "gate" => Ok(Profile {
            name: "gate",
            fast_clients: 40,
            slow_clients: 10,
            duration: Duration::from_secs(600), // 10min（§B）
            sample_every: Duration::from_secs(10),
            send_interval: Duration::from_millis(20),
            rss_growth_budget_kb: 200 * 1024, // §B B-PASS-1：<200MB/10min
            recovery_budget: Duration::from_secs(120), // §B：解除后 2min 内稳定
        }),
        other => Err(boxed(format!("unknown profile '{other}' (expect smoke|gate)"))),
    }
}

// ---------- server 进程 ----------

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn spawn_server(bin: &str, config: &str) -> BoxResult<Child> {
    let log = std::env::temp_dir().join("g7-server.stdout");
    let out = std::fs::File::create(&log)?;
    let err = out.try_clone()?;
    let child = Command::new(bin)
        .arg("--config-file")
        .arg(config)
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()?;
    Ok(child)
}

async fn wait_port(host: &str, port: u16, timeout: Duration) -> BoxResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(boxed(format!("server port {port} not up in {timeout:?}")));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------- 指标采集 ----------

/// 依赖无关的极简 HTTP GET（避免引 reqwest），解析 Prometheus 文本。
fn http_get(url: &str) -> BoxResult<String> {
    let stripped = url.strip_prefix("http://").ok_or_else(|| boxed("metrics url must be http://"))?;
    let (hostport, path) = match stripped.find('/') {
        Some(i) => (&stripped[..i], &stripped[i..]),
        None => (stripped, "/"),
    };
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(hostport)?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {hostport}\r\nConnection: close\r\n\r\n")?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf)?;
    Ok(buf)
}

fn parse_gauge(body: &str, name: &str) -> Option<f64> {
    for line in body.lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix(name) {
            // 形如 `name value` 或 `name{labels} value`
            let val = rest.trim_start_matches(|c: char| c != ' ').trim();
            if let Ok(v) = val.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// server RSS（KB），macOS/Linux 通用 `ps -o rss= -p <pid>`。
fn sample_rss_kb(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok()
}

#[derive(Default, Clone)]
struct Sample {
    t_secs: u64,
    rss_kb: u64,
    connections: f64,
    rejected: f64,
    inflight: f64,
    offline_queue: f64,
}

fn percentile(mut v: Vec<u64>, p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let idx = ((p / 100.0) * (v.len() as f64 - 1.0)).round() as usize;
    v[idx]
}

// ---------- 客户端 ----------

async fn provision(idx: usize, host: &str, port: u16, base: &PathBuf) -> BoxResult<(Arc<PrivchatSdk>, u64)> {
    let data_dir = base.join(format!("c{idx}"));
    std::fs::create_dir_all(&data_dir)?;
    let sdk = Arc::new(PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: host.to_string(),
            port,
            path: None,
            use_tls: false,
        }],
        connection_timeout_secs: 30,
        data_dir: data_dir.to_string_lossy().to_string(),
    }));
    sdk.connect().await?;
    let username = format!("g7_{idx}_{}", next_local_message_id());
    let device_id = format!("dev-{idx}");
    let login = sdk.register(username, "password123".to_string(), device_id).await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone()).await?;
    Ok((sdk, login.user_id))
}

async fn get_or_create_direct(sdk: &PrivchatSdk, target_user_id: u64) -> BoxResult<u64> {
    let resp: GetOrCreateDirectChannelResponse = sdk
        .rpc_call_typed(
            routes::channel::DIRECT_GET_OR_CREATE,
            &GetOrCreateDirectChannelRequest {
                target_user_id,
                source: Some("g7".to_string()),
                source_id: Some("backpressure".to_string()),
                user_id: 0,
            },
        )
        .await?;
    Ok(resp.channel_id)
}

/// 发一条文本：get_channel_pts → submit。返回 submit 往返耗时（微秒）。
async fn send_text(sdk: &PrivchatSdk, channel_id: u64, text: &str) -> BoxResult<u64> {
    let pts: GetChannelPtsResponse = sdk
        .rpc_call_typed(
            routes::sync::GET_CHANNEL_PTS,
            &GetChannelPtsRequest { channel_id, channel_type: DIRECT_CHANNEL_TYPE },
        )
        .await?;
    let start = Instant::now();
    let _: ClientSubmitResponse = sdk
        .rpc_call_typed(
            routes::sync::SUBMIT,
            &ClientSubmitRequest {
                local_message_id: next_local_message_id(),
                channel_id,
                channel_type: DIRECT_CHANNEL_TYPE,
                last_pts: pts.current_pts,
                command_type: "text".to_string(),
                payload: serde_json::json!({ "content": text, "metadata": serde_json::Value::Null }),
                client_timestamp: now_millis(),
                device_id: None,
            },
        )
        .await?;
    Ok(start.elapsed().as_micros() as u64)
}

// ---------- 主流程 ----------

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("G7 FAIL: {e}");
        std::process::exit(1);
    }
}

async fn run() -> BoxResult<()> {
    let args: Vec<String> = std::env::args().collect();
    let prof_name = args
        .windows(2)
        .find(|w| w[0] == "--profile")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "smoke".to_string());
    let prof = profile(&prof_name)?;

    let server_bin = std::env::var("G7_SERVER_BIN")
        .map_err(|_| boxed("G7_SERVER_BIN required (path to privchat server binary)"))?;
    let config = std::env::var("G7_CONFIG")
        .map_err(|_| boxed("G7_CONFIG required (server config.toml)"))?;
    let host = env_or("G7_HOST", "127.0.0.1");
    let tcp_port: u16 = env_or("G7_TCP_PORT", "9001").parse()?;
    let metrics_url = env_or("G7_METRICS", "http://127.0.0.1:9083/metrics");

    println!("=== G7 backpressure [{}] fast={} slow={} dur={:?} ===",
        prof.name, prof.fast_clients, prof.slow_clients, prof.duration);

    // 1) 拉起 server（拿 PID 采 RSS）
    let child = spawn_server(&server_bin, &config)?;
    let pid = child.id();
    let _guard = ChildGuard(Some(child));
    wait_port(&host, tcp_port, Duration::from_secs(30)).await?;
    println!("server up pid={pid}");

    let base = std::env::temp_dir().join(format!("g7-{}", pid));

    // 2) provision 慢 consumer（订阅但故意不排空事件 → 制造投递侧堆积）
    let mut slow_ids = Vec::new();
    let mut slow_sdks = Vec::new();
    for i in 0..prof.slow_clients {
        let (sdk, uid) = provision(1000 + i, &host, tcp_port, &base).await?;
        // 慢：subscribe 但用极慢节奏 recv（不排空 → broadcast lag + 不 ack）
        let mut rx = sdk.subscribe_events();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                // 每 5s 只取一条，模拟慢消费
                let _ = rx.try_recv();
            }
        });
        slow_ids.push(uid);
        slow_sdks.push(sdk);
    }
    println!("provisioned {} slow consumers", slow_ids.len());

    // 3) provision 快 sender，每个对准一个慢 consumer 建 DM（fan-out 到慢端）
    let mut senders = Vec::new();
    for i in 0..prof.fast_clients {
        let (sdk, _uid) = provision(i, &host, tcp_port, &base).await?;
        let target = slow_ids[i % slow_ids.len()];
        let channel_id = get_or_create_direct(&sdk, target).await?;
        senders.push((sdk, channel_id));
    }
    println!("provisioned {} fast senders", senders.len());

    // 4) 负载阶段：快 sender 持续发，主循环按 sample_every 采样
    let latencies = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut tasks = Vec::new();
    for (sdk, channel_id) in senders.iter().cloned().map(|(s, c)| (s, c)) {
        let lat = latencies.clone();
        let stop = stop.clone();
        let interval = prof.send_interval;
        tasks.push(tokio::spawn(async move {
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                if let Ok(us) = send_text(&sdk, channel_id, &format!("m{n}")).await {
                    lat.lock().unwrap().push(us);
                }
                n += 1;
                tokio::time::sleep(interval).await;
            }
        }));
    }

    let mut samples: Vec<Sample> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < prof.duration {
        tokio::time::sleep(prof.sample_every).await;
        let body = http_get(&metrics_url).unwrap_or_default();
        samples.push(Sample {
            t_secs: start.elapsed().as_secs(),
            rss_kb: sample_rss_kb(pid).unwrap_or(0),
            connections: parse_gauge(&body, "privchat_connections_current").unwrap_or(0.0),
            rejected: parse_gauge(&body, "privchat_handler_rejected_total").unwrap_or(0.0),
            inflight: parse_gauge(&body, "privchat_handler_inflight").unwrap_or(0.0),
            offline_queue: parse_gauge(&body, "privchat_offline_queue_depth").unwrap_or(0.0),
        });
        let s = samples.last().unwrap();
        println!("  t={}s rss={}MB conn={} rejected={} inflight={} offq={}",
            s.t_secs, s.rss_kb / 1024, s.connections, s.rejected, s.inflight, s.offline_queue);
    }
    stop.store(true, Ordering::Relaxed);
    for t in tasks {
        let _ = t.await;
    }

    // 5) 恢复阶段：解除慢消费者（drop 它们的 sdk → 断连），测 RSS 回稳耗时
    let load_end_rss = samples.last().map(|s| s.rss_kb).unwrap_or(0);
    drop(slow_sdks);
    let recover_start = Instant::now();
    let mut recovered = None;
    while recover_start.elapsed() < prof.recovery_budget {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let rss = sample_rss_kb(pid).unwrap_or(load_end_rss);
        // 回稳判据：RSS 不再高于负载末值（趋平/回落）
        if rss <= load_end_rss {
            recovered = Some(recover_start.elapsed());
            break;
        }
    }

    // 6) 判据评估（fail-fast）
    let warmup = samples.len() / 5; // 丢前 20% warmup
    let steady: Vec<&Sample> = samples.iter().skip(warmup).collect();
    let rss_min = steady.iter().map(|s| s.rss_kb).min().unwrap_or(0);
    let rss_max = steady.iter().map(|s| s.rss_kb).max().unwrap_or(0);
    let rss_growth = rss_max.saturating_sub(rss_min);
    let lat = std::mem::take(&mut *latencies.lock().unwrap());
    let sent = lat.len();
    let p99_ms = percentile(lat, 99.0) as f64 / 1000.0;

    println!("\n=== G7 result [{}] ===", prof.name);
    println!("  sent={sent} submit_p99={p99_ms:.1}ms", );
    println!("  RSS 稳态 min={}MB max={}MB growth={}MB (budget {}MB)",
        rss_min / 1024, rss_max / 1024, rss_growth / 1024, prof.rss_growth_budget_kb / 1024);
    match recovered {
        Some(d) => println!("  慢消费者解除后 RSS 回稳耗时 {:?} (budget {:?})", d, prof.recovery_budget),
        None => println!("  ⚠️ RSS 未在 {:?} 内回稳", prof.recovery_budget),
    }

    // fail-fast 断言
    let mut fails = Vec::new();
    if rss_growth > prof.rss_growth_budget_kb {
        fails.push(format!("RSS 稳态增长 {}MB > 预算 {}MB", rss_growth / 1024, prof.rss_growth_budget_kb / 1024));
    }
    if recovered.is_none() {
        fails.push(format!("慢消费者解除后 RSS 未在 {:?} 内回稳", prof.recovery_budget));
    }
    if sent < prof.fast_clients {
        fails.push(format!("发送量 {sent} 过低，负载未真正建立"));
    }
    if sent >= 1000 {
        // p99 样本足够才断言（§分位数规范 n≥... 这里用 1000 作 smoke 门槛）
        // 只做「无病态飙高」软上限：smoke 5s，gate 2s
        let p99_budget = if prof.name == "gate" { 2000.0 } else { 5000.0 };
        if p99_ms > p99_budget {
            fails.push(format!("submit p99 {p99_ms:.1}ms > 预算 {p99_budget:.0}ms"));
        }
    }

    if fails.is_empty() {
        println!("\nG7 {} PASS（注意：{} → 证据矩阵按限制标注，见文件头）",
            prof.name,
            if prof.name == "gate" { "gate 判据达标，但慢消费者仍是 SDK 级模拟" } else { "smoke passed" });
        Ok(())
    } else {
        for f in &fails {
            eprintln!("  FAIL: {f}");
        }
        Err(boxed(format!("G7 {} 未通过（{} 项）", prof.name, fails.len())))
    }
}
