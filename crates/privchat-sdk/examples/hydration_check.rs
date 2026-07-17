// P1-16 重启恢复完整性 E2E（privchat-docs spec/06-ops/PRIVCHAT_SERVER_REMEDIATION_TASKS.md）。
//
// 同一批数据跨 server 重启的 lazy-hydration 验收：造数 → SIGKILL 重启 → 重新
// authenticate → 六项断言（群名 / 全员禁言设置 / 成员与单人禁言 / direct channel
// 复用 / 群 last message / 禁言行为生效）。
//
//   STORM_DB_URL 不需要；数据全走 RPC。
//   STORM_SERVER_BIN=../privchat-server/target/debug/privchat \
//   cargo run --example hydration_check
//
// env（默认）：STORM_SERVER_CWD=../privchat-server、STORM_SERVER_CONFIG=config.storm.toml、
// STORM_HOST=127.0.0.1、STORM_TCP_PORT=9501。

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn pseudo_uuid(idx: usize) -> String {
    let a = now_millis();
    let mix = a.wrapping_mul(2654435761).wrapping_add(idx as u64 * 7919);
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (a & 0xffff_ffff) as u32,
        (std::process::id() & 0xffff) as u16,
        (idx & 0xfff) as u16,
        ((mix >> 12) & 0xfff) as u16,
        mix & 0xffff_ffff_ffff
    )
}

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

fn spawn_server(bin: &PathBuf, cwd: &PathBuf, config: &str, round: u32) -> BoxResult<Child> {
    let out = fs::File::create(std::env::temp_dir().join(format!("hydration-server-{round}.log")))?;
    Ok(Command::new(bin)
        .arg("--config-file")
        .arg(config)
        .current_dir(cwd)
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out))
        .spawn()?)
}

async fn wait_port(host: &str, port: u16) -> BoxResult<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::net::TcpStream::connect((host, port)).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err("server port not ready".into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

struct Client {
    sdk: PrivchatSdk,
    user_id: u64,
    token: String,
    device_id: String,
}

async fn register(host: &str, port: u16, tag: &str, idx: usize) -> BoxResult<Client> {
    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: host.to_string(),
            port,
            path: None,
            use_tls: false,
        }],
        connection_timeout_secs: 30,
        data_dir: format!("/tmp/hydration-{}-{}", now_millis(), tag),
    });
    sdk.connect().await?;
    let login = sdk
        .register(
            format!("hyd_{}_{}", now_millis() % 1_000_000, tag),
            "password123".into(),
            pseudo_uuid(idx),
        )
        .await?;
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;
    Ok(Client {
        sdk,
        user_id: login.user_id,
        token: login.token,
        device_id: login.device_id,
    })
}

async fn rpc(c: &Client, route: &str, body: serde_json::Value) -> BoxResult<serde_json::Value> {
    let resp = c.sdk.rpc_call(route.to_string(), body.to_string()).await?;
    Ok(serde_json::from_str(&resp)?)
}

async fn send_text(
    c: &Client,
    channel_id: u64,
    channel_type: u8,
    text: &str,
) -> BoxResult<serde_json::Value> {
    let pts = rpc(
        c,
        "sync/get_channel_pts",
        serde_json::json!({"channel_id": channel_id, "channel_type": channel_type}),
    )
    .await?;
    rpc(
        c,
        "sync/submit",
        serde_json::json!({
            "local_message_id": now_millis(),
            "channel_id": channel_id,
            "channel_type": channel_type,
            "last_pts": pts["current_pts"],
            "command_type": "text",
            "payload": {"content": text, "metadata": null},
            "client_timestamp": now_millis(),
        }),
    )
    .await
}

#[tokio::main]
async fn main() -> BoxResult<()> {
    let host = env_or("STORM_HOST", "127.0.0.1");
    let port: u16 = env_or("STORM_TCP_PORT", "9501").parse()?;
    let bin = PathBuf::from(env_or(
        "STORM_SERVER_BIN",
        "../privchat-server/target/debug/privchat",
    ))
    .canonicalize()?;
    let cwd = PathBuf::from(env_or("STORM_SERVER_CWD", "../privchat-server")).canonicalize()?;
    let config = env_or("STORM_SERVER_CONFIG", "config.storm.toml");

    if tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .is_ok()
    {
        return Err(format!("port {port} already in use — kill leftover storm server").into());
    }

    println!("== hydration check ==");
    let mut server = ChildGuard(Some(spawn_server(&bin, &cwd, &config, 1)?));
    wait_port(&host, port).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let a = register(&host, port, "a", 1).await?;
    let b = register(&host, port, "b", 2).await?;
    println!("registered a={} b={}", a.user_id, b.user_id);

    // ---- 造数 ----
    let group_name = format!("hyd-group-{}", now_millis() % 100_000);
    let created = rpc(
        &a,
        "group/group/create",
        serde_json::json!({
            "name": group_name,
            "description": "hydration check",
            "member_ids": [b.user_id],
            "creator_id": 0,
        }),
    )
    .await?;
    let group_id = created["group_id"].as_u64().ok_or("group_id missing")?;

    // 全员禁言开 + 单独禁言 B（10 分钟）
    rpc(
        &a,
        "group/settings/mute_all",
        serde_json::json!({"group_id": group_id, "operator_id": a.user_id.to_string(), "muted": true}),
    )
    .await?;
    rpc(
        &a,
        "group/member/mute",
        serde_json::json!({"group_id": group_id, "user_id": b.user_id, "mute_duration": 600, "operator_id": 0}),
    )
    .await?;

    // DM + 群消息（A 是 owner，不受 mute-all 限制）
    let direct = rpc(
        &a,
        "channel/direct/get_or_create",
        serde_json::json!({"target_user_id": b.user_id, "source": "hydration", "source_id": "t", "user_id": 0}),
    )
    .await?;
    let direct_id = direct["channel_id"]
        .as_u64()
        .ok_or("direct channel_id missing")?;
    let group_probe = format!("group-probe-{}", now_millis());
    send_text(&a, group_id, 2, &group_probe).await?;
    send_text(&a, direct_id, 1, &format!("dm-probe-{}", now_millis())).await?;
    println!("seeded: group={group_id} direct={direct_id}");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---- 重启 ----
    println!("restarting server ...");
    server.kill_now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut server2 = ChildGuard(Some(spawn_server(&bin, &cwd, &config, 2)?));
    wait_port(&host, port).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 重新认证（app 层 nudge 等价）
    a.sdk
        .authenticate(a.user_id, a.token.clone(), a.device_id.clone())
        .await?;
    b.sdk
        .authenticate(b.user_id, b.token.clone(), b.device_id.clone())
        .await?;

    // ---- 断言 ----
    let mut failures: Vec<String> = Vec::new();
    let mut check = |name: &str, ok: bool, detail: String| {
        println!(
            "  [{}] {} {}",
            if ok { "PASS" } else { "FAIL" },
            name,
            detail
        );
        if !ok {
            failures.push(format!("{name}: {detail}"));
        }
    };

    // 1. 群名
    let info = rpc(
        &a,
        "group/group/info",
        serde_json::json!({"group_id": group_id, "user_id": 0}),
    )
    .await?;
    let got_name = info
        .pointer("/group_info/name")
        .or_else(|| info.pointer("/name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    check(
        "group-name",
        got_name == group_name,
        format!("expected='{group_name}' actual='{got_name}'"),
    );

    // 2. 全员禁言设置
    let settings = rpc(
        &a,
        "group/settings/get",
        serde_json::json!({"group_id": group_id, "user_id": 0}),
    )
    .await?;
    let all_muted = settings
        .pointer("/settings/all_muted")
        .or_else(|| settings.pointer("/all_muted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    check(
        "settings-mute-all",
        all_muted,
        format!("all_muted={all_muted}"),
    );

    // 3. 成员数 + B 单独禁言状态
    let members = rpc(
        &a,
        "group/member/list",
        serde_json::json!({"group_id": group_id, "user_id": 0}),
    )
    .await?;
    let list = members
        .pointer("/members")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let b_muted = list
        .iter()
        .find(|m| m["user_id"].as_u64() == Some(b.user_id))
        .and_then(|m| m["is_muted"].as_bool())
        .unwrap_or(false);
    check(
        "members-and-mute",
        list.len() == 2 && b_muted,
        format!("members={} b_is_muted={}", list.len(), b_muted),
    );

    // 4. direct channel 复用
    let direct2 = rpc(
        &a,
        "channel/direct/get_or_create",
        serde_json::json!({"target_user_id": b.user_id, "source": "hydration", "source_id": "t2", "user_id": 0}),
    )
    .await?;
    let direct2_id = direct2["channel_id"].as_u64().unwrap_or(0);
    check(
        "direct-channel-reuse",
        direct2_id == direct_id,
        format!("before={direct_id} after={direct2_id}"),
    );

    // 5. 群 last message（channel entity sync since=0）
    let sync = rpc(
        &b,
        "entity/sync_entities",
        serde_json::json!({"entity_type": "channel", "since_version": 0, "limit": 200}),
    )
    .await?;
    let group_row_content = sync
        .pointer("/items")
        .and_then(|v| v.as_array())
        .and_then(|items| {
            items
                .iter()
                .find(|i| i["entity_id"].as_str() == Some(&group_id.to_string()))
        })
        .and_then(|i| i.pointer("/payload/last_msg_content"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    check(
        "group-last-message",
        group_row_content.contains(&group_probe),
        format!("expected contains '{group_probe}' actual='{group_row_content}'"),
    );

    // 6. 禁言行为生效：B 发群消息必须被拒
    let b_send = send_text(&b, group_id, 2, "should-be-rejected").await;
    check(
        "mute-enforced-after-restart",
        b_send.is_err(),
        format!("send result err={}", b_send.is_err()),
    );

    server2.kill_now();
    println!(
        "\n== hydration summary: {} ==",
        if failures.is_empty() {
            "ALL PASS"
        } else {
            "FAILURES"
        }
    );
    for f in &failures {
        println!("  - {f}");
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("{} hydration checks failed", failures.len()).into())
    }
}
