use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

#[tokio::main]
async fn main() -> BoxResult<()> {
    println!("\\nPrivChat SDK Basic Example (migrated)");
    println!("====================================");

    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let tcp_port = parse_env_u16("PRIVCHAT_TCP_PORT", 9001);
    let quic_port = parse_env_u16("PRIVCHAT_QUIC_PORT", 9001);
    let ws_port = parse_env_u16("PRIVCHAT_WS_PORT", 9080);

    let ts = now_millis();
    let data_dir = std::env::temp_dir().join(format!(
        "privchat-rust-basic-{}-{}",
        ts,
        std::process::id()
    ));
    std::fs::create_dir_all(&data_dir)?;

    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![
            ServerEndpoint {
                protocol: TransportProtocol::Quic,
                host: host.clone(),
                port: quic_port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::Tcp,
                host: host.clone(),
                port: tcp_port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::WebSocket,
                host,
                port: ws_port,
                path: Some("/".to_string()),
                use_tls: false,
            },
        ],
        connection_timeout_secs: 30,
        data_dir: path_to_string(&data_dir),
    });

    println!("1) connect");
    sdk.connect().await?;

    let suffix = format!("{}{}", ts % 100000, std::process::id());
    let username = format!("basic_{}", suffix);
    let password = "password123".to_string();
    let device_id = pseudo_uuid_v4_like();

    println!("2) register/login");
    let login = match sdk
        .register(username.clone(), password.clone(), device_id.clone())
        .await
    {
        Ok(out) => {
            println!("   registered: user_id={} username={}", out.user_id, username);
            out
        }
        Err(reg_err) => {
            println!("   register failed ({reg_err}), fallback to login");
            let out = sdk.login(username.clone(), password, device_id.clone()).await?;
            println!("   logged in: user_id={} username={}", out.user_id, username);
            out
        }
    };

    println!("3) authenticate");
    sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await?;

    println!("4) bootstrap sync");
    let bootstrap_before = sdk.is_bootstrap_completed().await?;
    if bootstrap_before {
        println!("   bootstrap already completed, still running one sync pass");
    }
    sdk.run_bootstrap_sync().await?;

    let snapshot = sdk.session_snapshot().await?;
    if let Some(snap) = snapshot {
        println!(
            "   session: user_id={} bootstrap_completed={}",
            snap.user_id, snap.bootstrap_completed
        );
    }

    println!("5) local store verification");
    let friends = sdk.list_friends(50, 0).await?;
    let groups = sdk.list_groups(50, 0).await?;
    let channels = sdk.list_channels(50, 0).await?;

    println!("   friends: {}", friends.len());
    println!("   groups: {}", groups.len());
    println!("   channels: {}", channels.len());

    if let Some(ch) = channels.first() {
        let msgs = sdk
            .list_messages(ch.channel_id, ch.channel_type, 20, 0)
            .await?;
        println!(
            "   first channel: id={} type={} messages={}",
            ch.channel_id,
            ch.channel_type,
            msgs.len()
        );
    } else {
        println!("   no channel in local store yet");
    }

    println!("6) disconnect");
    sdk.disconnect().await?;

    println!("\\nDone");
    Ok(())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn parse_env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(default)
}

fn path_to_string(path: &PathBuf) -> String {
    path.to_string_lossy().to_string()
}

fn pseudo_uuid_v4_like() -> String {
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u128;
    let x = ns ^ (pid << 32) ^ 0xa5a5_5a5a_dead_beef_u128;
    let part1 = (x >> 96) as u32;
    let part2 = (x >> 80) as u16;
    let part3 = (((x >> 64) as u16) & 0x0fff) | 0x4000;
    let part4 = (((x >> 48) as u16) & 0x3fff) | 0x8000;
    let part5 = (x & 0x0000_0000_0000_ffff_ffff_ffff_u128) as u64;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        part1, part2, part3, part4, part5
    )
}
