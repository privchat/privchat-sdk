//! PrivChat 多账号协作测试 - V2版本（使用新SDK API）
//! 
//! V2版本：完整的20阶段测试，使用统一的 PrivchatSDK API
//! 
//! 历史版本：
//! - realistic_test_phases.rs (原版 3900+行) - 保留作为参考
//! - simple_test.rs (简化版 3阶段) - 用于快速验证

mod account_manager;
mod event_system;
mod types;
mod test_phases_v2;          // V2: 20个测试阶段（使用新SDK API）
mod test_coordinator_v2;     // V2: 测试协调器
// mod simple_test;          // 简化版（可选）
// mod realistic_test_phases;  // 原版（保留作为参考）
// mod test_coordinator;       // 原版协调器
// mod test_phases;            // 原版辅助

use crate::account_manager::MultiAccountManager;
use crate::test_coordinator_v2::TestCoordinatorV2;
use privchat_sdk::error::Result;
use tracing_subscriber;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志系统
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .with_level(true)
        .init();
    
    println!("\n🚀 PrivChat SDK 多账号协作测试 V2");
    println!("====================================");
    println!("📋 测试范围（完整21个阶段）:");
    println!("  0️⃣  用户注册和登录（内置账号系统）");
    println!("  1️⃣  用户认证和初始化");
    println!("  2️⃣  好友系统完整流程");
    println!("  3️⃣  群组系统工作流");
    println!("  4️⃣  混合场景测试");
    println!("  5️⃣  消息接收验证");
    println!("  6️⃣  表情包功能");
    println!("  7️⃣  会话列表和置顶");
    println!("  8️⃣  已读回执");
    println!("  9️⃣  文件上传");
    println!("  🔟 其他消息类型");
    println!("  1️⃣1️⃣ 消息历史查询");
    println!("  1️⃣2️⃣ 消息撤回");
    println!("  1️⃣3️⃣ 离线消息推送");
    println!("  1️⃣4️⃣ PTS同步");
    println!("  1️⃣5️⃣ 高级群组功能");
    println!("  1️⃣6️⃣ 消息回复");
    println!("  1️⃣7️⃣ 消息反应（Reaction）");
    println!("  1️⃣8️⃣ 黑名单");
    println!("  1️⃣9️⃣ @提及功能");
    println!("  2️⃣0️⃣ 非好友消息");
    println!();
    println!("💡 V2版本特点：");
    println!("   • 使用统一的 PrivchatSDK API");
    println!("   • 更简洁、易维护（约1000行 vs 原版3900+行）");
    println!("   • 专注于核心功能验证");
    println!();
    
    // 创建账号管理器
    let mut account_manager = MultiAccountManager::new().await?;
    
    // TODO: 阶段 0: 用户注册和登录测试（需要重构）
    println!("\n⚠️  阶段 0: 用户注册和登录（暂时跳过，需要重构）");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 创建测试协调器并运行所有阶段
    let mut coordinator = TestCoordinatorV2::new();
    coordinator.run_all_phases(&mut account_manager).await?;
    
    // 清理资源
    account_manager.cleanup().await?;
    
    println!("\n🎉 测试完成!");
    
    Ok(())
}
