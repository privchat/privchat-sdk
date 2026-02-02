//! Phase 8 同步功能演示
//! 
//! 展示如何使用新的 pts-based 同步 API

use privchat_sdk::{PrivchatSDK, PrivchatConfig, ServerConfig, ServerEndpoint, TransportProtocol};
use std::path::PathBuf;
use tokio::time::{sleep, Duration};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    println!("========================================");
    println!("Phase 8: pts-Based 同步演示");
    println!("========================================\n");
    
    // === 1. 初始化 SDK ===
    
    let config = PrivchatConfig {
        data_dir: PathBuf::from("/tmp/data/phase8_demo"),
        assets_dir: None,  // SDK 使用 refinery 内置 migrations
        server_config: ServerConfig {
            endpoints: vec![
                ServerEndpoint {
                    protocol: TransportProtocol::Tcp,
                    host: "127.0.0.1".to_string(),
                    port: 8080,
                    path: None,
                    use_tls: false,
                }
            ],
        },
        connection_timeout: 30,
        heartbeat_interval: 60,
        retry_config: Default::default(),
        queue_config: Default::default(),
        event_config: Default::default(),
        timezone_offset_seconds: None,
        debug_mode: false,
    };
    
    let sdk = PrivchatSDK::initialize(config).await?;
    println!("✅ SDK 初始化完成\n");
    
    // === 2. 连接、注册和认证 ===
    
    println!("🔌 正在连接服务器...");
    sdk.connect().await?;
    println!("✅ 连接成功\n");
    
    println!("📝 正在注册用户...");
    let username = format!("sync_demo_{}", chrono::Utc::now().timestamp());
    let password = "test123456".to_string();
    let device_id = format!("device_{}", chrono::Utc::now().timestamp());
    
    let (user_id, token) = match sdk.register(username.clone(), password.clone(), device_id.clone(), None).await {
        Ok((user_id, token)) => {
            println!("✅ 注册成功: user_id={}, token={}", user_id, &token[..20]);
            (user_id, token)
        }
        Err(e) => {
            println!("⚠️  注册失败: {:?}", e);
            return Err(e.into());
        }
    };
    
    println!("\n🔐 正在认证...");
    let device_info = privchat_protocol::protocol::DeviceInfo {
        device_id: device_id.clone(),
        device_type: privchat_protocol::protocol::DeviceType::Web,
        app_id: "phase8_demo".to_string(),
        push_token: None,
        push_channel: None,
        device_name: "phase8_demo_rust".to_string(),
        device_model: Some("rust-sdk".to_string()),
        os_version: Some("1.0.0".to_string()),
        app_version: Some("1.0.0".to_string()),
        manufacturer: None,
        device_fingerprint: None,
    };
    sdk.authenticate(user_id, &token, device_info).await?;
    println!("✅ 认证成功\n");
    
    // 注意：连接成功后，SDK 会自动：
    // 1. 初始化 SyncEngine
    // 2. 触发初始同步（后台批量同步所有频道）
    
    println!("⏳ 等待初始同步完成...");
    sleep(Duration::from_secs(2)).await;
    
    // === 3. 查看同步状态 ===
    
    println!("\n========================================");
    println!("查看同步状态");
    println!("========================================\n");
    
    // 假设有一些频道（实际应用中会从会话列表获取）
    let test_channels = vec![
        (1001u64, 1u8), // 私聊
        (1002u64, 2u8), // 群聊
    ];
    
    for (channel_id, channel_type) in &test_channels {
        match sdk.get_channel_sync_state(*channel_id, *channel_type).await {
            Ok((local_pts, server_pts)) => {
                println!("频道 {} (类型 {}):", channel_id, channel_type);
                println!("  本地 pts:   {}", local_pts);
                println!("  服务器 pts: {}", server_pts);
                
                if local_pts < server_pts {
                    println!("  状态: ⚠️  需要同步（间隙: {}）", server_pts - local_pts);
                } else {
                    println!("  状态: ✅ 已同步");
                }
                println!();
            }
            Err(e) => {
                println!("⚠️  无法获取频道 {} 的同步状态: {:?}\n", channel_id, e);
            }
        }
    }
    
    // === 4. 手动同步单个频道 ===
    
    println!("========================================");
    println!("手动同步单个频道");
    println!("========================================\n");
    
    let channel_id = 1001u64;
    let channel_type = 1u8;
    
    println!("🔄 正在同步频道 {} (类型 {})...", channel_id, channel_type);
    
    match sdk.sync_channel(channel_id, channel_type).await {
        Ok(state) => {
            println!("✅ 同步完成:");
            println!("  本地 pts:   {}", state.local_pts);
            println!("  服务器 pts: {}", state.server_pts);
            println!("  状态: {:?}", state.state);
            println!("  最后同步: {}", chrono::DateTime::from_timestamp_millis(state.last_sync_at).unwrap());
        }
        Err(e) => {
            println!("❌ 同步失败: {:?}", e);
        }
    }
    
    // === 5. 批量同步所有频道 ===
    
    println!("\n========================================");
    println!("批量同步所有频道");
    println!("========================================\n");
    
    println!("🔄 正在同步所有频道...");
    
    match sdk.sync_all_channels().await {
        Ok(results) => {
            println!("✅ 批量同步完成，共 {} 个频道:\n", results.len());
            
            for state in results {
                let status = match state.state {
                    privchat_sdk::sync::SyncState::Synced => "✅ 已同步",
                    privchat_sdk::sync::SyncState::Syncing => "🔄 同步中",
                    privchat_sdk::sync::SyncState::HasGap { .. } => "⚠️  有间隙",
                    privchat_sdk::sync::SyncState::Failed { .. } => "❌ 失败",
                };
                
                println!("  频道 {} (类型 {}): {}", 
                         state.channel_id, 
                         state.channel_type, 
                         status);
                println!("    本地 pts: {}, 服务器 pts: {}", 
                         state.local_pts, 
                         state.server_pts);
            }
        }
        Err(e) => {
            println!("❌ 批量同步失败: {:?}", e);
        }
    }
    
    // === 6. 检查是否需要同步 ===
    
    println!("\n========================================");
    println!("检查是否需要同步");
    println!("========================================\n");
    
    for (channel_id, channel_type) in &test_channels {
        match sdk.needs_sync(*channel_id, *channel_type).await {
            Ok(needs_sync) => {
                if needs_sync {
                    println!("频道 {}: ⚠️  需要同步", channel_id);
                    
                    // 自动触发同步
                    println!("  → 触发同步...");
                    if let Ok(state) = sdk.sync_channel(*channel_id, *channel_type).await {
                        println!("  → ✅ 同步完成（state: {:?}）", state.state);
                    }
                } else {
                    println!("频道 {}: ✅ 已是最新", channel_id);
                }
            }
            Err(e) => {
                println!("频道 {}: ❌ 检查失败: {:?}", channel_id, e);
            }
        }
    }
    
    // === 7. 演示自动同步（消息推送时） ===
    
    println!("\n========================================");
    println!("自动同步演示");
    println!("========================================\n");
    
    println!("ℹ️  自动同步会在以下情况触发:");
    println!("  1. 连接成功后 → 后台批量同步所有频道");
    println!("  2. 收到消息推送时 → 检测 pts 间隙 → 自动补齐同步");
    println!("  3. 手动调用 sync_channel() 或 sync_all_channels()");
    
    println!("\n💡 提示:");
    println!("  - 本地 pts 存储在 channel 表的 last_msg_pts 字段");
    println!("  - 服务器 pts 通过 sync/get_channel_pts RPC 获取");
    println!("  - 间隙检测：local_pts < server_pts - 1");
    println!("  - 补齐同步：调用 sync/get_difference RPC 拉取缺失的 Commits");
    
    // === 8. 清理 ===
    
    println!("\n========================================");
    println!("清理");
    println!("========================================\n");
    
    println!("🛑 正在断开连接...");
    sdk.disconnect().await?;
    println!("✅ 已断开连接");
    
    println!("\n✅ 演示完成！");
    
    Ok(())
}
