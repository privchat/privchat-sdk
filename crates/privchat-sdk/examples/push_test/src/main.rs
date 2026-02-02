//! PrivChat 推送系统测试
//! 
//! 模拟场景：
//! - 用户A：1个模拟设备
//! - 用户B：2个模拟设备
//! 
//! 测试各种消息发送场景，验证推送状态和服务端推送日志

mod test_scenarios;
mod device_simulator;

use privchat_sdk::PrivchatSDK;
use privchat_sdk::error::Result;
use tracing_subscriber;
use std::time::Duration;

/// 测试配置
struct TestConfig {
    server_url: String,
    user_a_username: String,
    user_a_password: String,
    user_b_username: String,
    user_b_password: String,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            server_url: std::env::var("PRIVCHAT_SERVER_URL")
                .unwrap_or_else(|_| "ws://127.0.0.1:9080".to_string()),
            user_a_username: "push_test_user_a".to_string(),
            user_a_password: "test_password_123".to_string(),
            user_b_username: "push_test_user_b".to_string(),
            user_b_password: "test_password_123".to_string(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .with_level(true)
        .with_env_filter("push_test=debug,privchat_sdk=info,privchat_server=info")
        .init();
    
    println!("\n🚀 PrivChat 推送系统测试");
    println!("====================================");
    println!("📋 测试场景:");
    println!("  用户A: 1个设备（发送方）");
    println!("  用户B: 2个设备（接收方）");
    println!();
    println!("🧪 测试用例:");
    println!("  1️⃣  用户B全部设备在线 → 不推送");
    println!("  2️⃣  用户B全部设备离线 → 推送");
    println!("  3️⃣  用户B部分设备在线 → 只给离线设备推送");
    println!("  4️⃣  用户B设备 apns_armed=true → 推送");
    println!("  5️⃣  用户B设备 apns_armed=false → 不推送");
    println!("  6️⃣  消息发送成功 → 取消 Push Intent");
    println!("  7️⃣  消息撤销 → 撤销 Push Intent");
    println!("  8️⃣  用户B设备上线 → 取消 Push Intent");
    println!();
    
    let config = TestConfig::default();
    
    // 运行所有测试场景
    test_scenarios::run_all_scenarios(config).await?;
    
    println!("\n🎉 所有测试完成!");
    
    Ok(())
}
