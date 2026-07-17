// P1-14 reconnect-storm 压测（privchat-docs spec/06-ops/PRIVCHAT_SERVER_REMEDIATION_TASKS.md）。
//
// 场景：N 个客户端各持有 K 个 channel，全部在线后杀掉 server 再拉起，
// 度量重连风暴下的恢复表现。A/B 用不同 server binary 各跑一次对比：
//
//   STORM_DB_URL=postgres://... \
//   STORM_SERVER_BIN=../privchat-server/target/debug/privchat \
//   STORM_LABEL=new cargo run --example reconnect_storm
//
//   STORM_SERVER_BIN=../privchat-server-storm-old/target/debug/privchat \
//   STORM_LABEL=old cargo run --example reconnect_storm
//
// 其余 env（默认值）：STORM_SERVER_CWD=../privchat-server、
// STORM_SERVER_CONFIG=config.storm.toml、STORM_CLIENTS=10、
// STORM_CHANNELS_PER_USER=200、STORM_HOST=127.0.0.1、STORM_TCP_PORT=9501。
//
// 输出：恢复时间分布、每客户端 resume 轮数（单轮化证据）、server 侧
// channel-sync SQL 的 rows_returned / elapsed 汇总（下推增量化证据）、
// entity/sync_entities 请求数、SERVER_BUSY 计数。
//
// 风暴触发默认走 SDK 自动重连（InboundDisconnected → jitter backoff → 重连 →
// resume sync），恢复时刻的散布即 jitter 摊开效果；STORM_NUDGE=1 改为 harness
// 并发重新 authenticate（同秒脉冲，压力上界）。
//
// 踩坑备忘：harness 异常退出残留的 storm server 会占住端口，下一轮 wait_port
// 连上僵尸进程导致 A/B 全部失真（且客户端连接不断开，会误判成"SDK 被动检测
// 失效"）。现已用端口预检 + ChildGuard(Drop kill) 双保险。
//
// 依赖本机 psql 可用（种子/清理 channel 数据直接走 SQL）。

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

const SEED_ID_BASE: u64 = 6_600_000_000;

struct Env {
    label: String,
    server_bin: PathBuf,
    server_cwd: PathBuf,
    server_config: String,
    db_url: String,
    host: String,
    tcp_port: u16,
    clients: usize,
    channels_per_user: usize,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn load_env() -> BoxResult<Env> {
    let db_url = std::env::var("STORM_DB_URL")
        .map_err(|_| boxed("STORM_DB_URL is required (postgres url for seeding)"))?;
    Ok(Env {
        label: env_or("STORM_LABEL", "run"),
        server_bin: PathBuf::from(env_or(
            "STORM_SERVER_BIN",
            "../privchat-server/target/debug/privchat",
        ))
        .canonicalize()?,
        server_cwd: PathBuf::from(env_or("STORM_SERVER_CWD", "../privchat-server"))
            .canonicalize()?,
        server_config: env_or("STORM_SERVER_CONFIG", "config.storm.toml"),
        db_url,
        host: env_or("STORM_HOST", "127.0.0.1"),
        tcp_port: env_or("STORM_TCP_PORT", "9501").parse()?,
        clients: env_or("STORM_CLIENTS", "10").parse()?,
        channels_per_user: env_or("STORM_CHANNELS_PER_USER", "200").parse()?,
    })
}

fn boxed(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// server 要求 device_id 为 UUID 格式；用时间戳+序号拼一个 v4 形状的伪 UUID。
fn pseudo_uuid(idx: usize) -> String {
    let a = now_millis();
    let b = std::process::id() as u64;
    let mix = a.wrapping_mul(2654435761).wrapping_add(idx as u64 * 7919);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (a & 0xffff_ffff) as u32,
        (b & 0xffff) as u16,
        (idx & 0xfff) as u16,
        ((mix >> 12) & 0xfff) as u16,
        mix & 0xffff_ffff_ffff
    )
}

// ---------- server 进程管理 ----------

/// 兜底：任何路径退出（含 ? 提前返回）都杀掉 server 子进程，
/// 避免残留进程占住端口让下一轮 harness 连上僵尸 server。
struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn kill_now(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

fn spawn_server(env: &Env, round: u32) -> BoxResult<(Child, PathBuf)> {
    let stdout_path =
        std::env::temp_dir().join(format!("storm-{}-server-{}.stdout", env.label, round));
    let f = fs::File::create(&stdout_path)?;
    let child = Command::new(&env.server_bin)
        .arg("--config-file")
        .arg(&env.server_config)
        .current_dir(&env.server_cwd)
        .stdout(Stdio::from(f.try_clone()?))
        .stderr(Stdio::from(f))
        .spawn()?;
    Ok((child, stdout_path))
}

async fn wait_port(host: &str, port: u16, timeout: Duration) -> BoxResult<Instant> {
    let deadline = Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return Ok(Instant::now());
        }
        if Instant::now() > deadline {
            return Err(boxed(format!("server port {port} not ready in time")));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// ---------- psql 种子/清理 ----------

fn run_psql(db_url: &str, sql: &str) -> BoxResult<()> {
    let path = std::env::temp_dir().join(format!("storm-seed-{}.sql", std::process::id()));
    fs::write(&path, sql)?;
    let out = Command::new("psql")
        .arg(db_url)
        .arg("-q")
        .arg("-v")
        .arg("ON_ERROR_STOP=1")
        .arg("-f")
        .arg(&path)
        .output()?;
    let _ = fs::remove_file(&path);
    if !out.status.success() {
        return Err(boxed(format!(
            "psql failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

fn cleanup_seed_sql(env: &Env) -> String {
    let base = SEED_ID_BASE;
    let end = SEED_ID_BASE + (env.clients * env.channels_per_user) as u64 + 1_000;
    format!(
        "DELETE FROM privchat_user_channels WHERE channel_id BETWEEN {base} AND {end};\n\
         DELETE FROM privchat_channels WHERE channel_id BETWEEN {base} AND {end};\n\
         DELETE FROM privchat_groups WHERE group_id BETWEEN {base} AND {end};\n"
    )
}

fn seed_sql(env: &Env, user_ids: &[u64]) -> String {
    let mut sql = String::with_capacity(1 << 20);
    sql.push_str("BEGIN;\n");
    let mut id = SEED_ID_BASE;
    for (idx, uid) in user_ids.iter().enumerate() {
        for j in 0..env.channels_per_user {
            sql.push_str(&format!(
                "INSERT INTO privchat_groups (group_id, name, owner_id, qr_key) \
                 VALUES ({id}, 'storm-{idx}-{j}', {uid}, 'st{id:014}') ON CONFLICT DO NOTHING;\n"
            ));
            sql.push_str(&format!(
                "INSERT INTO privchat_channels (channel_id, channel_type, group_id) \
                 VALUES ({id}, 1, {id}) ON CONFLICT DO NOTHING;\n"
            ));
            sql.push_str(&format!(
                "INSERT INTO privchat_user_channels (user_id, channel_id) \
                 VALUES ({uid}, {id}) ON CONFLICT DO NOTHING;\n"
            ));
            id += 1;
        }
    }
    sql.push_str("COMMIT;\n");
    sql
}

// ---------- 客户端 ----------

struct StormClient {
    idx: usize,
    sdk: Arc<PrivchatSdk>,
    events: Arc<Mutex<Vec<(Instant, String)>>>,
    bootstrap_secs: f64,
    user_id: u64,
    token: String,
    device_id: String,
}

async fn provision_client(
    idx: usize,
    env: &Env,
    base_dir: &PathBuf,
    suffix: &str,
) -> BoxResult<(StormClient, u64)> {
    let data_dir = base_dir.join(format!("c{idx}"));
    fs::create_dir_all(&data_dir)?;
    let sdk = Arc::new(PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: env.host.clone(),
            port: env.tcp_port,
            path: None,
            use_tls: false,
        }],
        connection_timeout_secs: 30,
        data_dir: data_dir.to_string_lossy().to_string(),
    }));
    sdk.connect().await?;
    let username = format!("storm_{suffix}_{idx}");
    let device_id = pseudo_uuid(idx);
    let login = sdk
        .register(username, "password123".to_string(), device_id)
        .await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;

    // 事件收集：Debug 字符串足够做计数与时序，避免耦合具体枚举形态。
    let events: Arc<Mutex<Vec<(Instant, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let mut rx = sdk.subscribe_events();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let s = format!("{ev:?}");
                    sink.lock().unwrap().push((Instant::now(), s));
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    Ok((
        StormClient {
            idx,
            sdk,
            events,
            bootstrap_secs: 0.0,
            user_id: login.user_id,
            token: login.token,
            device_id: login.device_id,
        },
        login.user_id,
    ))
}

fn count_events_since(client: &StormClient, since: Instant, prefix: &str) -> usize {
    client
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, s)| *t >= since && s.starts_with(prefix))
        .count()
}

fn first_event_since(client: &StormClient, since: Instant, prefix: &str) -> Option<Instant> {
    client
        .events
        .lock()
        .unwrap()
        .iter()
        .find(|(t, s)| *t >= since && s.starts_with(prefix))
        .map(|(t, _)| *t)
}

// ---------- server 日志解析 ----------

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for ch in s.chars() {
        if in_esc {
            if ch == 'm' {
                in_esc = false;
            }
        } else if ch == '\u{1b}' {
            in_esc = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_field<T: std::str::FromStr>(line: &str, key: &str) -> Option<T> {
    let pos = line.find(key)? + key.len();
    let rest = &line[pos..];
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[derive(Default)]
struct ServerLogStats {
    sync_requests: usize,
    channel_query_count: usize,
    channel_query_rows: u64,
    channel_query_elapsed_ms: f64,
    channel_query_max_ms: f64,
    server_busy: usize,
}

fn parse_server_log(path: &PathBuf, from_offset: u64) -> ServerLogStats {
    let mut stats = ServerLogStats::default();
    let Ok(raw) = fs::read(path) else {
        return stats;
    };
    let slice = if (from_offset as usize) < raw.len() {
        &raw[from_offset as usize..]
    } else {
        return stats;
    };
    for line in String::from_utf8_lossy(slice).lines() {
        let line = strip_ansi(line);
        if line.contains("route=entity/sync_entities") && line.contains("RPC 调用:") {
            stats.sync_requests += 1;
        }
        if line.contains("SERVER_BUSY") {
            stats.server_busy += 1;
        }
        // channel entity sync 的主查询（新旧实现同样命中这个片段）
        if line.contains("rows_returned") && line.contains("FROM privchat_user_channels uc") {
            stats.channel_query_count += 1;
            if let Some(rows) = parse_field::<u64>(&line, "rows_returned=") {
                stats.channel_query_rows += rows;
            }
            if let Some(ms) = parse_field::<f64>(&line, "elapsed=") {
                stats.channel_query_elapsed_ms += ms;
                if ms > stats.channel_query_max_ms {
                    stats.channel_query_max_ms = ms;
                }
            }
        }
    }
    stats
}

// ---------- 主流程 ----------

#[tokio::main]
async fn main() -> BoxResult<()> {
    if std::env::var_os("NO_PROXY").is_none() {
        std::env::set_var("NO_PROXY", "127.0.0.1,localhost,::1");
    }
    let env = load_env()?;
    let server_log = env.server_cwd.join("logs/storm-server.log");
    println!("== reconnect storm [{}] ==", env.label);
    println!(
        "server_bin={} clients={} channels_per_user={}",
        env.server_bin.display(),
        env.clients,
        env.channels_per_user
    );

    // 预检：端口必须空闲。残留的 storm server（例如上一轮异常退出未清理）会让
    // 本轮 wait_port 连上僵尸进程，A/B 全部失真。
    if tokio::net::TcpStream::connect((env.host.as_str(), env.tcp_port))
        .await
        .is_ok()
    {
        return Err(boxed(format!(
            "port {} already in use — kill the leftover storm server first \
             (pkill -f config.storm.toml)",
            env.tcp_port
        )));
    }

    // 幂等：清掉上一轮种子。
    run_psql(&env.db_url, &cleanup_seed_sql(&env))?;

    // 第 1 次拉起 server。
    let (server_child, _stdout1) = spawn_server(&env, 1)?;
    let mut server = ChildGuard(Some(server_child));
    wait_port(&env.host, env.tcp_port, Duration::from_secs(60)).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let suffix = format!("{}{}", now_millis() % 100_000, std::process::id());
    let base_dir = std::env::temp_dir().join(format!("privchat-storm-{suffix}"));
    fs::create_dir_all(&base_dir)?;

    // 注册 + 认证 N 个客户端。
    let mut clients = Vec::with_capacity(env.clients);
    let mut user_ids = Vec::with_capacity(env.clients);
    for idx in 0..env.clients {
        let (c, uid) = provision_client(idx, &env, &base_dir, &suffix).await?;
        clients.push(c);
        user_ids.push(uid);
    }
    println!("provisioned {} clients: uids={:?}", clients.len(), user_ids);

    // 种子 channel（在 bootstrap 之前，让全量同步覆盖它们）。
    run_psql(&env.db_url, &seed_sql(&env, &user_ids))?;
    println!(
        "seeded {} channels ({} per user)",
        env.clients * env.channels_per_user,
        env.channels_per_user
    );

    // bootstrap：游标推到 head。
    for c in clients.iter_mut() {
        let t = Instant::now();
        c.sdk.run_bootstrap_sync().await?;
        c.bootstrap_secs = t.elapsed().as_secs_f64();
    }
    let boot: Vec<String> = clients
        .iter()
        .map(|c| format!("{:.2}s", c.bootstrap_secs))
        .collect();
    println!("bootstrap done: {}", boot.join(" "));
    tokio::time::sleep(Duration::from_secs(2)).await;

    // ---- 风暴：kill → 重启 ----
    let log_offset = fs::metadata(&server_log).map(|m| m.len()).unwrap_or(0);
    println!("killing server ...");
    let t_kill = Instant::now();
    server.kill_now();
    tokio::time::sleep(Duration::from_secs(3)).await;

    println!("restarting server ...");
    let (server2_child, _stdout2) = spawn_server(&env, 2)?;
    let mut server2 = ChildGuard(Some(server2_child));
    let t0 = wait_port(&env.host, env.tcp_port, Duration::from_secs(60)).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 风暴触发两种模式：
    // - 默认（auto）：什么都不做，靠 SDK 的 InboundDisconnected → jitter backoff →
    //   自动重连 → resume sync。这是最真实的重启风暴（含 jitter 摊开效果）。
    // - STORM_NUDGE=1：并发对每个客户端重新 authenticate（等价移动端 app 层
    //   token-refresh 重连 nudge），形成同秒 auth+resume 脉冲，压力上界。
    if env_or("STORM_NUDGE", "0") == "1" {
        let mut joins = Vec::new();
        for c in clients.iter() {
            let sdk = c.sdk.clone();
            let (uid, token, device) = (c.user_id, c.token.clone(), c.device_id.clone());
            let idx = c.idx;
            joins.push(tokio::spawn(async move {
                for attempt in 0..5 {
                    match sdk.authenticate(uid, token.clone(), device.clone()).await {
                        Ok(_) => return,
                        Err(e) => {
                            eprintln!("c{idx:02} re-auth attempt {attempt} failed: {e}");
                            tokio::time::sleep(Duration::from_secs(2)).await;
                        }
                    }
                }
            }));
        }
        for j in joins {
            let _ = j.await;
        }
    }

    // 等全部客户端完成 resume sync。
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut done_at: Vec<Option<Instant>> = vec![None; clients.len()];
    loop {
        for (i, c) in clients.iter().enumerate() {
            if done_at[i].is_none() {
                done_at[i] = first_event_since(c, t0, "ResumeSyncCompleted");
            }
        }
        if done_at.iter().all(|d| d.is_some()) {
            break;
        }
        if Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // ---- 汇总 ----
    let mut recover: Vec<f64> = Vec::new();
    let mut reconnect_at: Vec<f64> = Vec::new();
    println!("\nper-client (after restart):");
    for (i, c) in clients.iter().enumerate() {
        let recov = done_at[i].map(|t| (t - t0).as_secs_f64());
        let first_conn = first_event_since(c, t_kill, "ConnectionStateChanged").and_then(|_| {
            c.events
                .lock()
                .unwrap()
                .iter()
                .find(|(t, s)| {
                    *t >= t0
                        && s.starts_with("ConnectionStateChanged")
                        && s.contains("to: Connected")
                })
                .map(|(t, _)| (*t - t0).as_secs_f64())
        });
        let started = count_events_since(c, t0, "ResumeSyncStarted");
        let completed = count_events_since(c, t0, "ResumeSyncCompleted");
        println!(
            "  c{:02} reconnect={:>6} recover={:>6} resume_started={} resume_completed={}",
            c.idx,
            first_conn.map_or("--".into(), |v| format!("{v:.2}s")),
            recov.map_or("TIMEOUT".into(), |v| format!("{v:.2}s")),
            started,
            completed
        );
        if let Some(v) = recov {
            recover.push(v);
        }
        if let Some(v) = first_conn {
            reconnect_at.push(v);
        }
    }
    recover.sort_by(|a, b| a.partial_cmp(b).unwrap());
    reconnect_at.sort_by(|a, b| a.partial_cmp(b).unwrap());

    tokio::time::sleep(Duration::from_secs(1)).await;
    let stats = parse_server_log(&server_log, log_offset);

    // P1-00 指标快照（/metrics 在 file server 端口暴露）：sync 维度直接来自
    // server 侧埋点，与日志解析互为印证。老 binary 无这些指标时输出为空。
    let metrics_port: u16 = env_or("STORM_METRICS_PORT", "9583").parse().unwrap_or(9583);
    if let Ok(out) = Command::new("curl")
        .arg("-s")
        .arg("--noproxy")
        .arg("*")
        .arg(format!("http://{}:{}/metrics", env.host, metrics_port))
        .output()
    {
        let body = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = body
            .lines()
            .filter(|l| l.starts_with("privchat_sync_entities"))
            .collect();
        if !lines.is_empty() {
            println!("\nserver /metrics (sync dimension):");
            for l in lines {
                println!("  {l}");
            }
        }
    }

    println!("\n== summary [{}] ==", env.label);
    println!(
        "clients={} channels_per_user={} recovered={}/{}",
        env.clients,
        env.channels_per_user,
        recover.len(),
        env.clients
    );
    if !recover.is_empty() {
        println!(
            "recovery_secs: min={:.2} median={:.2} max={:.2}",
            recover.first().unwrap(),
            recover[recover.len() / 2],
            recover.last().unwrap()
        );
    }
    if !reconnect_at.is_empty() {
        println!(
            "reconnect_spread_secs: first={:.2} last={:.2}",
            reconnect_at.first().unwrap(),
            reconnect_at.last().unwrap()
        );
    }
    println!(
        "server: sync_requests={} channel_queries={} channel_rows_fetched={} \
         channel_query_ms(total={:.1} max={:.2}) server_busy={}",
        stats.sync_requests,
        stats.channel_query_count,
        stats.channel_query_rows,
        stats.channel_query_elapsed_ms,
        stats.channel_query_max_ms,
        stats.server_busy
    );

    // machine-readable 行，便于 A/B 提取。
    println!(
        "STORM_RESULT label={} clients={} k={} recovered={} recover_max={:.2} \
         sync_requests={} channel_queries={} channel_rows={} channel_ms_total={:.1} \
         channel_ms_max={:.2} busy={}",
        env.label,
        env.clients,
        env.channels_per_user,
        recover.len(),
        recover.last().copied().unwrap_or(-1.0),
        stats.sync_requests,
        stats.channel_query_count,
        stats.channel_query_rows,
        stats.channel_query_elapsed_ms,
        stats.channel_query_max_ms,
        stats.server_busy
    );

    // ---- 清理 ----
    server2.kill_now();
    run_psql(&env.db_url, &cleanup_seed_sql(&env))?;
    let _ = fs::remove_dir_all(&base_dir);
    let mut stdout = std::io::stdout();
    let _ = stdout.flush();
    Ok(())
}
