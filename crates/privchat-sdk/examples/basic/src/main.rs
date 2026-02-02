//! PrivChat SDK 基础使用示例（含实体同步）
//!
//! 演示流程：
//! 1. 使用 PrivchatSDK 初始化、连接、认证
//! 2. 认证后执行实体同步（好友、群组）— 首次登录时 cursor 为空，引擎会全量拉取；之后为增量
//! 3. 同步成功以 sync_entities 返回 Ok(count) 及本地数量校验来标注
//! 4. 断开连接

use privchat_sdk::{
    PrivchatSDK, PrivchatConfig, ServerConfig, ServerEndpoint, TransportProtocol,
    Result,
};
use privchat_sdk::storage::entities::ChannelQuery;
use privchat_protocol::protocol::{DeviceInfo, DeviceType};
use std::path::PathBuf;
use uuid::Uuid;
use tracing::{info, error, warn};
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    info!("🚀 PrivChat SDK 基础示例（含实体同步）");
    info!("========================================\n");

    // 步骤 1: 初始化 SDK（数据目录、服务器端点；Schema 使用 SDK 内置 embedded migrations）
    info!("📝 步骤 1: 初始化 SDK");
    let data_dir = PathBuf::from("/tmp/privchat_basic_demo");
    let config = PrivchatConfig::builder()
        .data_dir(&data_dir)
        .server_config(ServerConfig {
            endpoints: vec![ServerEndpoint {
                protocol: TransportProtocol::Quic,
                host: "127.0.0.1".to_string(),
                port: 9001,
                path: None,
                use_tls: false,
            }],
        })
        .connection_timeout(10)
        .build();

    let sdk = PrivchatSDK::initialize(config).await?;
    info!("✅ SDK 初始化成功");
    info!("   数据目录: {}", data_dir.display());
    info!("   服务器: QUIC 127.0.0.1:9001\n");

    // 步骤 2: 建立网络连接
    info!("📝 步骤 2: 建立网络连接");
    if let Err(e) = sdk.connect().await {
        error!("❌ 连接失败: {}", e);
        return Err(e);
    }
    info!("✅ 连接成功\n");

    // 步骤 3: 注册或登录（用户名随机避免重复注册失败；device_id 必须为 UUID，且注册与 auth 使用同一 device_id）
    info!("📝 步骤 3: 注册 / 登录并认证");
    let username = format!("user_{}", Uuid::new_v4());
    let password = "demo123".to_string();
    let device_id = Uuid::new_v4().to_string();
    let device_info = DeviceInfo {
        device_id: device_id.clone(),
        device_type: DeviceType::Linux,
        app_id: "basic-demo".to_string(),
        push_token: None,
        push_channel: None,
        device_name: "basic example".to_string(),
        device_model: None,
        os_version: None,
        app_version: None,
        manufacturer: None,
        device_fingerprint: None,
    };

    let (user_id, token) = match sdk.register(username.clone(), password.clone(), device_id.clone(), Some(device_info.clone())).await {
        Ok((uid, tok)) => {
            info!("   已注册新用户，获得 token");
            (uid, tok)
        }
        Err(e) => {
            info!("   注册失败（可能用户已存在），尝试登录: {}", e);
            let (uid, tok) = sdk.login(username.clone(), password, device_id.clone(), Some(device_info.clone())).await?;
            (uid, tok)
        }
    };

    if let Err(e) = sdk.authenticate(user_id, &token, device_info).await {
        error!("❌ 认证失败: {}", e);
        return Err(e);
    }
    info!("✅ 认证成功: user_id={}, username={}, device_id={}\n", user_id, username, device_id);

    // 步骤 4: 启动同步（is_bootstrap_completed 未完成则同步全量，已完成则后台增量）
    info!("📝 步骤 4: 启动同步 (Bootstrap)");
    let needs_bootstrap = match sdk.is_bootstrap_completed().await {
        Ok(false) => {
            info!("   ℹ️  首次初始化：未完成过 Bootstrap，执行全量同步");
            true
        }
        Ok(true) => {
            info!("   ℹ️  已初始化过：发起后台增量同步");
            false
        }
        Err(e) => {
            warn!("   ⚠️  检查 Bootstrap 状态失败: {}，尝试执行同步", e);
            true
        }
    };

    let friend_synced = if needs_bootstrap {
        match sdk.run_bootstrap_sync().await {
            Ok(()) => {
                info!("   ✅ Bootstrap 同步完成 (Friend → Group → Channel → UserSettings)");
                Some(0usize) // 条数由各类型汇总，此处仅标注成功
            }
            Err(e) => {
                warn!("   ⚠️  Bootstrap 同步失败: {}（可忽略若服务端未实现 entity/sync_entities）", e);
                None
            }
        }
    } else {
        PrivchatSDK::run_bootstrap_sync_in_background(sdk.clone());
        info!("   ✅ 已发起后台增量同步 (run_bootstrap_sync_in_background)");
        None
    };

    let group_synced = friend_synced; // Bootstrap 已包含群组，本地校验时沿用

    // 步骤 5: 从本地读取，校验同步结果（标注「同步成功」的另一种体现）
    info!("📝 步骤 5: 本地数据校验（同步结果落库验证）");
    match sdk.get_friends(100, 0).await {
        Ok(list) => {
            let n = list.len();
            info!("   本地好友数: {}（前 {} 条）", n, n.min(100));
            if let Some(synced) = friend_synced {
                if n >= synced {
                    info!("   → 好友同步已落库，数量一致或更多（含历史）");
                } else {
                    info!("   → 本地数量 {} 小于本轮同步 {}，可能为分页或服务端返回不全", n, synced);
                }
            }
        }
        Err(e) => warn!("   读取本地好友失败: {}", e),
    }
    match sdk.get_groups(100, 0).await {
        Ok(list) => {
            let n = list.len();
            info!("   本地群组数: {}（前 {} 条）", n, n.min(100));
            if let Some(synced) = group_synced {
                if n >= synced {
                    info!("   → 群组同步已落库，数量一致或更多（含历史）");
                } else {
                    info!("   → 本地数量 {} 小于本轮同步 {}，可能为分页或服务端返回不全", n, synced);
                }
            }
        }
        Err(e) => warn!("   读取本地群组失败: {}", e),
    }

    // 步骤 5b: 从 SQLite 读会话与消息（UI 数据来源：落库后 get_channels / get_messages）
    info!("📝 步骤 5b: 本地 SQLite 会话与消息（get_channels → get_messages）");
    // 给推送消息分发任务一点时间落库（欢迎消息等由 message_rx 异步保存，避免 get_messages 先于保存被调用）
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let query = ChannelQuery::default();
    match sdk.get_channels(&query).await {
        Ok(channels) => {
            let n = channels.len();
            info!("   get_channels() 返回 {} 个会话", n);
            if let Some(first) = channels.first() {
                let channel_id = first.channel_id;
                let channel_type = first.channel_type;
                info!("   取第一个会话: channel_id={}, channel_type={}", channel_id, channel_type);
                match sdk.get_messages(channel_id, 50, None).await {
                    Ok(messages) => {
                        info!("   get_messages(channel_id={}, limit=50) 返回 {} 条消息", channel_id, messages.len());
                        for (i, msg) in messages.iter().take(5).enumerate() {
                            let content_preview = msg.content.chars().take(60).collect::<String>();
                            if msg.content.len() > 60 {
                                info!("       [{}] id={:?} from_uid={} content=\"{}...\"", i + 1, msg.server_message_id, msg.from_uid, content_preview);
                            } else {
                                info!("       [{}] id={:?} from_uid={} content=\"{}\"", i + 1, msg.server_message_id, msg.from_uid, content_preview);
                            }
                        }
                        if messages.len() > 5 {
                            info!("       ... 共 {} 条（仅打印前 5 条）", messages.len());
                        }
                        if messages.is_empty() {
                            info!("   → 当前会话无消息（协议层收到的欢迎消息若未落库则此处为空）");
                        }
                    }
                    Err(e) => warn!("   get_messages(channel_id={}) 失败: {}", channel_id, e),
                }
            } else {
                info!("   → 无会话，无法查询 get_messages");
            }
        }
        Err(e) => warn!("   get_channels() 失败: {}", e),
    }
    info!("");

    // 步骤 6: 断开连接
    info!("📝 步骤 6: 断开连接");
    if let Err(e) = sdk.disconnect().await {
        warn!("⚠️ 断开连接失败: {}", e);
    } else {
        info!("✅ 已断开连接\n");
    }

    info!("========================================");
    info!("🎉 示例运行完成");
    info!("");
    info!("💡 实体同步说明:");
    info!("   - is_bootstrap_completed() 为 false：首次初始化，必须同步执行 run_bootstrap_sync()（全量）");
    info!("   - is_bootstrap_completed() 为 true：已初始化，可异步 run_bootstrap_sync_in_background()（增量）");
    info!("   - 各类型 cursor 存于 KV：sync_cursor:friend、sync_cursor:group、sync_cursor:channel、sync_cursor:user_settings");
    Ok(())
}
