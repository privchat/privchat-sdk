// TCP vs WebSocket 重连耗时对比。
//
// 回答的问题：App 从后台回到前台时那一轮「建连 + 认证」，换传输协议会不会更快。
// 每轮都是完整的冷重连（新建 SDK 实例 + 空 data_dir），量三段：
//   connect   —— 传输层建连（TCP 三次握手；WebSocket 还要多一次 HTTP upgrade）
//   auth      —— AuthorizationRequest 往返
//   total     —— 两者之和，即用户盯着状态条的那段时间
//
// 跑法（在服务器本机跑可排除网络，只比协议栈；从客户端网络跑才是真实体感）：
//   BENCH_HOST=127.0.0.1 BENCH_ROUNDS=10 \
//     cargo run --release --example transport_reconnect_bench
//
// env（默认值）：BENCH_HOST=127.0.0.1、BENCH_TCP_PORT=9001、
// BENCH_WS_PORT=9080、BENCH_WS_PATH=/gate、BENCH_ROUNDS=10。

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

struct Sample {
    connect: Duration,
    auth: Duration,
}

impl Sample {
    fn total(&self) -> Duration {
        self.connect + self.auth
    }
}

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_num<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn endpoint(protocol: TransportProtocol, host: &str) -> ServerEndpoint {
    match protocol {
        TransportProtocol::WebSocket => ServerEndpoint {
            protocol,
            host: host.to_string(),
            port: env_num("BENCH_WS_PORT", 9080u16),
            path: Some(env_str("BENCH_WS_PATH", "/gate")),
            use_tls: false,
        },
        _ => ServerEndpoint {
            protocol,
            host: host.to_string(),
            port: env_num("BENCH_TCP_PORT", 9001u16),
            path: None,
            use_tls: false,
        },
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// 拿一组可复用的 (uid, token, device_id)，后续每轮都用它做 authenticate，
/// 避免把注册开销算进重连耗时。
///
/// 生产是 PLATFORM 模式、内置注册已禁用，所以优先读 env 里注入的既有凭据
/// （BENCH_UID / BENCH_TOKEN / BENCH_DEVICE）；只有本地 BUILTIN server 才走注册。
async fn provision_credentials(host: &str, base: &PathBuf) -> BoxResult<(u64, String, String)> {
    if let (Ok(uid), Ok(token)) = (std::env::var("BENCH_UID"), std::env::var("BENCH_TOKEN")) {
        let uid: u64 = uid.parse()?;
        let device = env_str("BENCH_DEVICE", "bench-device");
        return Ok((uid, token, device));
    }
    let dir = base.join("provision");
    fs::create_dir_all(&dir)?;
    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![endpoint(TransportProtocol::Tcp, host)],
        connection_timeout_secs: 30,
        data_dir: dir.to_string_lossy().to_string(),
    });
    sdk.connect().await?;
    let suffix = unique_suffix();
    let login = sdk
        .register(
            format!("bench_{suffix}"),
            "password123".to_string(),
            format!("bench-device-{suffix}"),
        )
        .await?;
    sdk.shutdown().await;
    Ok((login.user_id, login.token, login.device_id))
}

/// 一轮冷重连：全新 SDK 实例 + 全新 data_dir，测 connect / auth 两段。
async fn one_round(
    protocol: TransportProtocol,
    host: &str,
    base: &PathBuf,
    round: usize,
    creds: &(u64, String, String),
) -> BoxResult<Sample> {
    let dir = base.join(format!("{protocol:?}-{round}"));
    fs::create_dir_all(&dir)?;
    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![endpoint(protocol, host)],
        connection_timeout_secs: 30,
        data_dir: dir.to_string_lossy().to_string(),
    });

    let t0 = Instant::now();
    sdk.connect().await?;
    let connect = t0.elapsed();

    let t1 = Instant::now();
    sdk.authenticate(creds.0, creds.1.clone(), creds.2.clone())
        .await?;
    let auth = t1.elapsed();

    sdk.shutdown().await;
    Ok(Sample { connect, auth })
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn report(label: &str, samples: &[Sample]) {
    if samples.is_empty() {
        println!("{label:<10} no samples");
        return;
    }
    let mut totals: Vec<f64> = samples.iter().map(|s| ms(s.total())).collect();
    totals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum: f64 = totals.iter().sum();
    let mean = sum / totals.len() as f64;
    let median = totals[totals.len() / 2];
    let connect_mean: f64 =
        samples.iter().map(|s| ms(s.connect)).sum::<f64>() / samples.len() as f64;
    let auth_mean: f64 = samples.iter().map(|s| ms(s.auth)).sum::<f64>() / samples.len() as f64;
    println!(
        "{label:<10} n={:<3} total mean={mean:>8.1}ms median={median:>8.1}ms \
         min={:>8.1}ms max={:>8.1}ms  (connect={connect_mean:>7.1}ms auth={auth_mean:>7.1}ms)",
        totals.len(),
        totals[0],
        totals[totals.len() - 1],
    );
}

#[tokio::main]
async fn main() -> BoxResult<()> {
    let host = env_str("BENCH_HOST", "127.0.0.1");
    let rounds: usize = env_num("BENCH_ROUNDS", 10);
    let base = std::env::temp_dir().join(format!("privchat-bench-{}", unique_suffix()));
    fs::create_dir_all(&base)?;

    println!("host={host} rounds={rounds}");
    let creds = provision_credentials(&host, &base).await?;
    println!("provisioned uid={}", creds.0);

    // 交替跑，避免服务端预热 / 网络状态漂移偏向先跑的那个协议。
    let mut tcp = Vec::new();
    let mut ws = Vec::new();
    for round in 0..rounds {
        match one_round(TransportProtocol::Tcp, &host, &base, round, &creds).await {
            Ok(s) => tcp.push(s),
            Err(e) => println!("tcp round {round} failed: {e}"),
        }
        match one_round(TransportProtocol::WebSocket, &host, &base, round, &creds).await {
            Ok(s) => ws.push(s),
            Err(e) => println!("ws  round {round} failed: {e}"),
        }
    }

    println!();
    report("tcp", &tcp);
    report("websocket", &ws);

    let _ = fs::remove_dir_all(&base);
    Ok(())
}
