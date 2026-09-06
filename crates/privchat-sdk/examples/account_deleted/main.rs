//! 账号注销后的会话闭环验证。
//!
//! 注销必须让**已经建立的连接**也失效，而不只是关掉登录入口。这个例子把连接
//! 一直握在手里，注销发生在连接存活期间——单看「重新登录被拒」是验不出这件事的。
//!
//! 跑法（凭据由外部脚本注册后注入）：
//!   PRIVCHAT_HOST=... PRIVCHAT_TCP_PORT=9001 PRIVCHAT_SPKI_PINS=...
//!   PRIVCHAT_UID_A=... PRIVCHAT_TOKEN_A=... PRIVCHAT_DEVICE_A=...
//!   cargo run --release --example account_deleted
//!
//! 它连上之后每 3 秒调一次**真实受保护 RPC**并打印结果，持续 60 秒。
//! 外部脚本在中途调 delete-account，观察这里从成功变成失败。
//! 🔴 判据不能用 logout 这种「即使鉴权过也几乎没有副作用」的接口。

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};
use std::time::Duration;

type BoxResult<T> = Result<T, Box<dyn std::error::Error>>;

#[tokio::main]
async fn main() -> BoxResult<()> {
    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let tcp_port: u16 = std::env::var("PRIVCHAT_TCP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9001);
    let uid: u64 = std::env::var("PRIVCHAT_UID_A")?.parse()?;
    let token = std::env::var("PRIVCHAT_TOKEN_A")?;
    let device = std::env::var("PRIVCHAT_DEVICE_A")?;

    let dir = std::env::temp_dir().join(format!("privchat-deleted-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let spki_pins: Vec<String> = std::env::var("PRIVCHAT_SPKI_PINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();

    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: host.clone(),
            port: tcp_port,
            path: None,
            use_tls: true,
        }],
        connection_timeout_secs: 30,
        data_dir: dir.to_string_lossy().to_string(),
        spki_pins,
    });

    println!("1) connect + authenticate uid={uid}");
    sdk.connect().await?;
    sdk.authenticate(uid, token, device.clone()).await?;
    println!("   已建立连接");

    println!("\n2) 连接存活期间反复调受保护 RPC（外部脚本会在中途注销这个账号）");
    let mut first_failure_at: Option<u64> = None;
    for tick in 0..20u64 {
        let secs = tick * 3;
        // account/user/detail 是真实业务查询：鉴权不过就拿不到数据，
        // 不像 logout 那样「过没过都返回成功」。
        let r = sdk
            .rpc_call(
                "account/user/detail".to_string(),
                serde_json::json!({ "user_id": uid }).to_string(),
            )
            .await;
        match r {
            Ok(_) => println!("   t+{secs:>3}s  RPC 成功"),
            Err(e) => {
                println!("   t+{secs:>3}s  RPC 失败: {e}");
                if first_failure_at.is_none() {
                    first_failure_at = Some(secs);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    match first_failure_at {
        Some(s) => println!("\n✅ 已建立的连接在注销后 {s}s 内失效"),
        None => println!("\n🔴 整整 60 秒里这条已建立的连接一直可用——注销没有终止它"),
    }
    Ok(())
}
