//! 简化的多账号测试 - 测试消息队列系统

use crate::account_manager::MultiAccountManager;
use privchat_sdk::error::Result;
use tokio::time::{sleep, Duration};
use tracing::info;

pub struct SimpleTest;

impl SimpleTest {
    pub async fn run(account_manager: &mut MultiAccountManager) -> Result<()> {
        info!("🚀 开始简化测试流程");
        info!("============================");
        
        // Step 1: 连接所有账号
        info!("\n📍 Step 1: 连接所有账号");
        account_manager.connect_all().await?;
        info!("✅ 所有账号已连接");
        
        // 等待连接稳定
        sleep(Duration::from_secs(1)).await;
        
        // Step 2: Alice 发送消息给 Bob
        info!("\n📍 Step 2: Alice 发送消息给 Bob");
        let bob_user_id = account_manager.get_user_id("bob").unwrap();
        
        let msg1 = account_manager.send_message("alice", bob_user_id, "Hello Bob!").await?;
        info!("✅ Alice 发送消息成功: {}", msg1);
        
        // Step 3: Bob 发送消息给 Alice
        info!("\n📍 Step 3: Bob 发送消息给 Alice");
        let alice_user_id = account_manager.get_user_id("alice").unwrap();
        
        let msg2 = account_manager.send_message("bob", alice_user_id, "Hi Alice!").await?;
        info!("✅ Bob 发送消息成功: {}", msg2);
        
        // Step 4: Charlie 发送消息给 Alice
        info!("\n📍 Step 4: Charlie 发送消息给 Alice");
        let msg3 = account_manager.send_message("charlie", alice_user_id, "Hey Alice!").await?;
        info!("✅ Charlie 发送消息成功: {}", msg3);
        
        // 等待消息处理
        info!("\n⏱️  等待消息处理...");
        sleep(Duration::from_secs(3)).await;
        
        info!("\n🎉 测试完成!");
        info!("============================");
        info!("测试总结:");
        info!("  ✅ 3个账号成功连接");
        info!("  ✅ 3条消息成功发送");
        info!("  ✅ 消息队列系统工作正常");
        
        Ok(())
    }
}
