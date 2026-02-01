//! 连接状态演示
//! 
//! 展示如何使用 SDK 的连接状态管理功能

use privchat_sdk::{PrivchatSDK, PrivchatConfig, ServerConfig, ServerEndpoint, TransportProtocol};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    println!("\n🚀 连接状态管理演示\n");
    println!("====================================\n");
    
    // 配置 SDK
    let config = PrivchatConfig {
        data_dir: PathBuf::from("/tmp/privchat_connection_demo"),
        assets_dir: None,
        server_config: ServerConfig {
            endpoints: vec![
                ServerEndpoint {
                    protocol: TransportProtocol::Quic,
                    host: "127.0.0.1".to_string(),
                    port: 8082,
                    path: None,
                    use_tls: false, // 测试环境
                },
            ],
        },
        connection_timeout: 10,
        heartbeat_interval: 30,
        retry_config: Default::default(),
        event_config: Default::default(),
        timezone_offset_seconds: None,
    };
    
    // 初始化 SDK
    println!("📦 正在初始化 SDK...");
    let sdk = PrivchatSDK::initialize(config).await?;
    println!("✅ SDK 初始化完成\n");
    
    // 打印初始状态
    println!("【初始状态】");
    sdk.log_connection_state().await;
    println!();
    
    // 模拟 JWT token（实际使用时需要从服务器获取）
    let token = generate_mock_jwt("1001");
    
    // 连接到服务器
    println!("🔌 正在连接到服务器...");
    match sdk.connect("1001", &token).await {
        Ok(()) => {
            println!("✅ 连接成功！\n");
            
            // 打印连接后的状态
            println!("【连接后状态】");
            sdk.log_connection_state().await;
            println!();
            
            // 获取状态详情
            let state = sdk.get_connection_state().await;
            println!("📊 连接详情：");
            println!("   协议: {}", state.protocol);
            println!("   状态: {}", state.status);
            println!("   服务器: {}", state.server.address);
            println!("   TLS: {}", if state.use_tls { "是" } else { "否" });
            
            if let Some(user) = &state.user {
                println!("   用户ID: {}", user.user_id);
                println!("   设备ID: {}", user.device_id);
                if let Some(session_id) = &user.session_id {
                    println!("   会话ID: {}", session_id);
                }
            }
            
            println!("   SDK版本: {}", state.sdk_version);
            println!("   平台: {}", state.platform);
            println!();
            
            // 模拟发送几条消息
            println!("📤 发送测试消息...");
            for i in 1..=3 {
                let content = format!("测试消息 #{}", i);
                match sdk.send_message("1002", &content).await {
                    Ok(msg_id) => println!("   ✅ 消息 #{} 已发送: {}", i, msg_id),
                    Err(e) => println!("   ❌ 消息 #{} 发送失败: {}", i, e),
                }
                sleep(Duration::from_millis(100)).await;
            }
            println!();
            
            // 等待一秒让统计更新
            sleep(Duration::from_secs(1)).await;
            
            // 再次打印状态（应该能看到统计信息）
            println!("【更新后状态】");
            sdk.log_connection_state().await;
            println!();
            
            // 获取JSON格式的状态
            let state = sdk.get_connection_state().await;
            if let Ok(json) = state.to_json_pretty() {
                println!("📋 JSON格式状态：");
                println!("{}", json);
                println!();
            }
            
            // 断开连接
            println!("🔌 正在断开连接...");
            sdk.disconnect().await?;
            println!("✅ 已断开连接\n");
            
            // 打印断开后的状态
            println!("【断开后状态】");
            sdk.log_connection_state().await;
            println!();
        }
        Err(e) => {
            println!("❌ 连接失败: {}\n", e);
            println!("💡 请确保服务器正在运行（端口 8082）\n");
        }
    }
    
    // 关闭 SDK
    sdk.shutdown().await?;
    
    println!("🎉 演示完成！\n");
    println!("====================================\n");
    
    Ok(())
}

/// 生成模拟的 JWT token（仅用于演示）
fn generate_mock_jwt(user_id: u64) -> String {
    use jsonwebtoken::{encode, Header, EncodingKey, Algorithm};
    use serde::{Serialize, Deserialize};
    
    #[derive(Debug, Serialize, Deserialize)]
    struct Claims {
        sub: String,
        exp: usize,
    }
    
    let claims = Claims {
        sub: user_id,
        exp: (chrono::Utc::now() + chrono::Duration::hours(24)).timestamp() as usize,
    };
    
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    
    encode(&header, &claims, &EncodingKey::from_secret("test_secret_key_for_demo_only".as_ref()))
        .unwrap_or_else(|_| "invalid_token".to_string())
}
