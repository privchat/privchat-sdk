//! 推送测试场景
//! 
//! 各种推送场景的测试用例

use super::device_simulator::{DeviceSimulator, DeviceState};
use super::TestConfig;
use privchat_sdk::error::Result;
use tracing::{info, warn};
use std::time::Duration;

/// 运行所有测试场景
pub async fn run_all_scenarios(config: TestConfig) -> Result<()> {
    info!("🚀 开始运行推送测试场景");
    
    // 先注册用户A和用户B
    println!("\n📝 准备测试用户...");
    let user_a_id = register_user(&config, &config.user_a_username, &config.user_a_password).await?;
    let user_b_id = register_user(&config, &config.user_b_username, &config.user_b_password).await?;
    println!("✅ 用户A ID: {}", user_a_id);
    println!("✅ 用户B ID: {}", user_b_id);
    
    // 场景 1: 用户B全部设备在线 → 不推送
    test_scenario_1(&config, user_a_id, user_b_id).await?;
    
    // 场景 2: 用户B全部设备离线 → 推送
    test_scenario_2(&config, user_a_id, user_b_id).await?;
    
    // 场景 3: 用户B部分设备在线 → 只给离线设备推送
    test_scenario_3(&config, user_a_id, user_b_id).await?;
    
    // 场景 4: 用户B设备 apns_armed=true → 推送
    test_scenario_4(&config, user_a_id, user_b_id).await?;
    
    // 场景 5: 用户B设备 apns_armed=false → 不推送
    test_scenario_5(&config, user_a_id, user_b_id).await?;
    
    // 场景 6: 消息发送成功 → 取消 Push Intent
    test_scenario_6(&config, user_a_id, user_b_id).await?;
    
    // 场景 7: 消息撤销 → 撤销 Push Intent
    test_scenario_7(&config, user_a_id, user_b_id).await?;
    
    // 场景 8: 用户B设备上线 → 取消 Push Intent
    test_scenario_8(&config, user_a_id, user_b_id).await?;
    
    Ok(())
}

/// 注册用户（如果已存在则跳过）
async fn register_user(config: &TestConfig, username: &str, password: &str) -> Result<u64> {
    use super::device_simulator::DeviceSimulator;
    
    let device_id = format!("register_{}", uuid::Uuid::new_v4());
    let mut device = DeviceSimulator::new(device_id);
    
    // 尝试注册（如果失败可能是用户已存在，尝试登录）
    match device.register_and_connect(&config.server_url, username, password).await {
        Ok(_) => {
            let user_id = device.user_id.unwrap();
            device.disconnect().await?;
            Ok(user_id)
        }
        Err(_) => {
            // 用户可能已存在，尝试登录
            device.login_and_connect(&config.server_url, username, password).await?;
            let user_id = device.user_id.unwrap();
            device.disconnect().await?;
            Ok(user_id)
        }
    }
}

/// 场景 1: 用户B全部设备在线 → 不推送
async fn test_scenario_1(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 1: 用户B全部设备在线 → 不推送");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的2个设备
    let mut device_b1 = DeviceSimulator::new("device_b_001".to_string());
    let mut device_b2 = DeviceSimulator::new("device_b_002".to_string());
    
    // 3. 所有设备都连接
    info!("[场景1] 连接所有设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    device_b1.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    device_b2.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景1] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B
    info!("[场景1] 用户A发送消息给用户B...");
    device_a.send_message(channel_id, "场景1测试消息：用户B全部设备在线，应该不推送").await?;
    
    // 6. 观察日志：应该看到 "User {} is online, skip push"
    info!("[场景1] 观察服务端日志：应该看到 'User {{}} is online, skip push'");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 7. 清理
    device_a.disconnect().await?;
    device_b1.disconnect().await?;
    device_b2.disconnect().await?;
    
    // 等待清理完成
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    println!("✅ 场景 1 完成");
    Ok(())
}

/// 场景 2: 用户B全部设备离线 → 推送
async fn test_scenario_2(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 2: 用户B全部设备离线 → 推送");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的2个设备（不连接）
    let device_b1 = DeviceSimulator::new("device_b_001".to_string());
    let device_b2 = DeviceSimulator::new("device_b_002".to_string());
    
    // 3. 只连接用户A
    info!("[场景2] 连接用户A设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景2] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B（用户B离线）
    info!("[场景2] 用户A发送消息给用户B（用户B离线）...");
    device_a.send_message(channel_id, "场景2测试消息：用户B离线，应该推送").await?;
    
    // 5. 观察日志：应该看到 "User {} is offline, generating push intent"
    info!("[场景2] 观察服务端日志：应该看到 'User {{}} is offline, generating push intent'");
    info!("[场景2] 观察服务端日志：应该看到 'PUSH WORKER.*Processing intent'");
    info!("[场景2] 观察服务端日志：应该看到 'Provider.*send'");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 6. 清理
    device_a.disconnect().await?;
    
    println!("✅ 场景 2 完成");
    Ok(())
}

/// 场景 3: 用户B部分设备在线 → 只给离线设备推送
async fn test_scenario_3(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 3: 用户B部分设备在线 → 只给离线设备推送");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的2个设备
    let mut device_b1 = DeviceSimulator::new("device_b_001".to_string());
    let device_b2 = DeviceSimulator::new("device_b_002".to_string());  // 离线
    
    // 3. 连接用户A和用户B的设备1
    info!("[场景3] 连接用户A和用户B的设备1...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    device_b1.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景3] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B（设备1在线，设备2离线）
    info!("[场景3] 用户A发送消息给用户B（设备1在线，设备2离线）...");
    device_a.send_message(channel_id, "场景3测试消息：设备1在线，设备2离线，应该只给设备2推送").await?;
    
    // 5. 观察日志：
    // - 应该看到消息通过长连接发送到设备1
    // - 应该看到为设备2生成 Push Intent
    info!("[场景3] 观察服务端日志：");
    info!("  - 应该看到消息发送到设备1（长连接）");
    info!("  - 应该看到为设备2生成 Push Intent");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 6. 清理
    device_a.disconnect().await?;
    device_b1.disconnect().await?;
    
    println!("✅ 场景 3 完成");
    Ok(())
}

/// 场景 4: 用户B设备 apns_armed=true → 推送
async fn test_scenario_4(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 4: 用户B设备 apns_armed=true → 推送");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的设备
    let mut device_b = DeviceSimulator::new("device_b_001".to_string());
    
    // 3. 连接所有设备
    info!("[场景4] 连接所有设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    device_b.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 用户B设备切换到后台（apns_armed=true）
    info!("[场景4] 用户B设备切换到后台（apns_armed=true）...");
    device_b.switch_to_background().await?;
    
    // 等待状态更新
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 5. 断开用户B设备（模拟连接失败）
    info!("[场景4] 断开用户B设备（模拟连接失败）...");
    device_b.disconnect().await?;
    
    // 等待状态更新
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 6. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景4] 私聊频道ID: {}", channel_id);
    
    // 7. 用户A发送消息给用户B（设备 apns_armed=true, connected=false）
    info!("[场景4] 用户A发送消息给用户B（设备 apns_armed=true, connected=false）...");
    device_a.send_message(channel_id, "场景4测试消息：设备 apns_armed=true，应该推送").await?;
    
    // 7. 观察日志：应该看到生成 Push Intent
    info!("[场景4] 观察服务端日志：应该看到生成 Push Intent");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 8. 清理
    device_a.disconnect().await?;
    
    println!("✅ 场景 4 完成");
    Ok(())
}

/// 场景 5: 用户B设备 apns_armed=false → 不推送
async fn test_scenario_5(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 5: 用户B设备 apns_armed=false → 不推送");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的设备
    let mut device_b = DeviceSimulator::new("device_b_001".to_string());
    
    // 3. 连接所有设备
    info!("[场景5] 连接所有设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    device_b.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 断开用户B设备（apns_armed=false，默认值）
    info!("[场景5] 断开用户B设备（apns_armed=false）...");
    device_b.disconnect().await?;
    
    // 等待状态更新
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 5. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景5] 私聊频道ID: {}", channel_id);
    
    // 6. 用户A发送消息给用户B（设备 apns_armed=false, connected=false）
    info!("[场景5] 用户A发送消息给用户B（设备 apns_armed=false, connected=false）...");
    device_a.send_message(channel_id, "场景5测试消息：设备 apns_armed=false，不应该推送").await?;
    
    // 6. 观察日志：不应该看到生成 Push Intent
    info!("[场景5] 观察服务端日志：不应该看到生成 Push Intent");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 7. 清理
    device_a.disconnect().await?;
    
    println!("✅ 场景 5 完成");
    Ok(())
}

/// 场景 6: 消息发送成功 → 取消 Push Intent
async fn test_scenario_6(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 6: 消息发送成功 → 取消 Push Intent");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的设备（离线）
    let mut device_b = DeviceSimulator::new("device_b_001".to_string());
    
    // 3. 只连接用户A
    info!("[场景6] 连接用户A设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景6] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B（用户B离线）
    info!("[场景6] 用户A发送消息给用户B（用户B离线）...");
    device_a.send_message(channel_id, "场景6测试消息：用户B离线，生成 Push Intent").await?;
    
    // 等待 Push Intent 生成
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 6. 用户B设备上线（在 Worker 处理前）
    info!("[场景6] 用户B设备上线（在 Worker 处理前）...");
    device_b.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 6. 观察日志：
    // - 应该看到 MessageDelivered 事件
    // - 应该看到 Intent 被取消
    info!("[场景6] 观察服务端日志：");
    info!("  - 应该看到 'MessageDelivered.*published'");
    info!("  - 应该看到 'Intent.*marked as cancelled'");
    info!("  - 应该看到 'Intent.*is cancelled.*skipping'");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 7. 清理
    device_a.disconnect().await?;
    device_b.disconnect().await?;
    
    println!("✅ 场景 6 完成");
    Ok(())
}

/// 场景 7: 消息撤销 → 撤销 Push Intent
async fn test_scenario_7(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 7: 消息撤销 → 撤销 Push Intent");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的设备（离线）
    let device_b = DeviceSimulator::new("device_b_001".to_string());
    
    // 3. 只连接用户A
    info!("[场景7] 连接用户A设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景7] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B（用户B离线）
    info!("[场景7] 用户A发送消息给用户B（用户B离线）...");
    let message_id = device_a.send_message(channel_id, "场景7测试消息：用户B离线，生成 Push Intent").await?;
    
    // 等待 Push Intent 生成
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 6. 用户A撤销消息（在 Worker 处理前）
    info!("[场景7] 用户A撤销消息（在 Worker 处理前）...");
    device_a.revoke_message(channel_id, message_id).await?;
    
    // 6. 观察日志：
    // - 应该看到 MessageRevoked 事件
    // - 应该看到 Intent 被撤销
    info!("[场景7] 观察服务端日志：");
    info!("  - 应该看到 'MessageRevoked.*published'");
    info!("  - 应该看到 'Intent.*marked as revoked'");
    info!("  - 应该看到 'Intent.*is revoked.*skipping'");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 7. 清理
    device_a.disconnect().await?;
    
    println!("✅ 场景 7 完成");
    Ok(())
}

/// 场景 8: 用户B设备上线 → 取消 Push Intent
async fn test_scenario_8(config: &TestConfig, user_a_id: u64, user_b_id: u64) -> Result<()> {
    println!("\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("📋 场景 8: 用户B设备上线 → 取消 Push Intent");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    
    // 1. 创建用户A的设备
    let mut device_a = DeviceSimulator::new("device_a_001".to_string());
    
    // 2. 创建用户B的设备（离线）
    let mut device_b = DeviceSimulator::new("device_b_001".to_string());
    
    // 3. 只连接用户A
    info!("[场景8] 连接用户A设备...");
    device_a.login_and_connect(&config.server_url, &config.user_a_username, &config.user_a_password).await?;
    
    // 等待连接稳定
    tokio::time::sleep(Duration::from_secs(2)).await;
    
    // 4. 获取或创建私聊频道
    let channel_id = device_a.get_or_create_direct_channel(user_b_id).await?;
    info!("[场景8] 私聊频道ID: {}", channel_id);
    
    // 5. 用户A发送消息给用户B（用户B离线）
    info!("[场景8] 用户A发送消息给用户B（用户B离线）...");
    device_a.send_message(channel_id, "场景8测试消息：用户B离线，生成 Push Intent").await?;
    
    // 等待 Push Intent 生成
    tokio::time::sleep(Duration::from_secs(1)).await;
    
    // 6. 用户B设备上线（在 Worker 处理前）
    info!("[场景8] 用户B设备上线（在 Worker 处理前）...");
    device_b.login_and_connect(&config.server_url, &config.user_b_username, &config.user_b_password).await?;
    
    // 6. 观察日志：
    // - 应该看到 DeviceOnline 事件
    // - 应该看到 Intent 被取消
    info!("[场景8] 观察服务端日志：");
    info!("  - 应该看到 'DeviceOnline.*published'");
    info!("  - 应该看到 'Intent.*marked as cancelled'");
    info!("  - 应该看到 'Intent.*is cancelled.*skipping'");
    tokio::time::sleep(Duration::from_secs(3)).await;
    
    // 7. 清理
    device_a.disconnect().await?;
    device_b.disconnect().await?;
    
    println!("✅ 场景 8 完成");
    Ok(())
}
