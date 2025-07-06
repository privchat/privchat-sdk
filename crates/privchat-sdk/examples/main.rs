use privchat_sdk::{
    PrivchatSDK, SDKConfig, 
    events::{SDKEvent, EventFilter, ConnectionState},
    error::Result,
};
use std::path::PathBuf;
use tokio;
use tracing::{info, warn, error};
use std::sync::Arc;
use tokio::sync::mpsc;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    println!("🚀 PrivChat SDK 统一接口示例");
    println!("================================\n");
    
    // 示例1：SDK 初始化和配置
    println!("📋 示例1: SDK 初始化和配置");
    let config = SDKConfig::builder()
        .server_url("wss://api.privchat.com/ws")
        .user_id("demo_user_123")
        .data_dir("./demo_data")
        .connection_timeout(30)
        .heartbeat_interval(30)
        .debug_mode(true)
        .build();
    
    let sdk = PrivchatSDK::initialize(config).await?;
    
    println!("✅ SDK 初始化完成");
    println!("📍 用户ID: {}", sdk.user_id());
    println!("📍 服务器地址: {}", sdk.config().server_url);
    println!("📍 存储路径: {}", sdk.config().data_dir.display());
    println!();
    
    // 示例2：事件监听设置
    println!("📋 示例2: 事件监听设置");
    
    // 创建事件接收器
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    
    // 注册各种事件监听器
    sdk.on_message_received({
        let tx = event_tx.clone();
        move |message| {
            let _ = tx.send(format!("📨 收到消息: {} -> {}", message.sender_id, message.content));
        }
    });
    
    sdk.on_connection_state_changed({
        let tx = event_tx.clone();
        move |is_connected| {
            let status = if is_connected { "🟢 已连接" } else { "🔴 已断开" };
            let _ = tx.send(format!("🔗 连接状态变化: {}", status));
        }
    });
    
    sdk.on_typing_indicator({
        let tx = event_tx.clone();
        move |user_id, session_id, is_typing| {
            let action = if is_typing { "开始输入" } else { "停止输入" };
            let _ = tx.send(format!("⌨️  {} 在 {} 中{}", user_id, session_id, action));
        }
    });
    
    sdk.on_reaction_changed({
        let tx = event_tx.clone();
        move |message_id, user_id, emoji, is_added| {
            let action = if is_added { "添加" } else { "移除" };
            let _ = tx.send(format!("😊 {} {}了表情 {} 在消息 {}", user_id, action, emoji, message_id));
        }
    });
    
    println!("✅ 事件监听器注册完成");
    println!();
    
    // 示例3：连接和认证
    println!("📋 示例3: 连接和认证");
    
    // 启动事件监听任务
    let event_task = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            println!("🔔 {}", event);
        }
    });
    
    // 连接到服务器
    println!("🔗 正在连接到服务器...");
    match sdk.connect("demo_token_456").await {
        Ok(_) => println!("✅ 连接成功"),
        Err(e) => {
            warn!("⚠️  连接失败: {}", e);
            println!("💡 这是演示模式，连接失败是正常的");
        }
    }
    
    // 等待一下让连接状态稳定
    tokio::time::sleep(Duration::from_millis(100)).await;
    println!();
    
    // 示例4：消息发送和接收
    println!("📋 示例4: 消息发送和接收");
    
    // 发送文本消息
    println!("📤 发送文本消息...");
    let message_result = sdk.send_message("channel_demo", "你好，这是一条测试消息！").await;
    match message_result {
        Ok(message_id) => {
            println!("✅ 消息发送成功，ID: {}", message_id);
            
            // 等待一下，然后标记为已读
            tokio::time::sleep(Duration::from_millis(500)).await;
            
            println!("👁️  标记消息为已读...");
            if let Err(e) = sdk.mark_as_read("channel_demo", message_id.clone()).await {
                warn!("⚠️  标记已读失败: {}", e);
            } else {
                println!("✅ 消息已标记为已读");
            }
        }
        Err(e) => {
            warn!("⚠️  消息发送失败: {}", e);
            println!("💡 这是演示模式，发送失败是正常的");
        }
    }
    
    // 发送另一条消息用于演示撤回
    println!("📤 发送消息用于演示撤回...");
    let rich_message_result = sdk.send_message("channel_demo", "这条消息将被撤回").await;
    match rich_message_result {
        Ok(message_id) => {
            println!("✅ 消息发送成功，ID: {}", message_id);
            
            // 演示消息撤回
            tokio::time::sleep(Duration::from_millis(500)).await;
            println!("🔄 撤回消息...");
            if let Err(e) = sdk.recall_message(&message_id).await {
                warn!("⚠️  消息撤回失败: {}", e);
            } else {
                println!("✅ 消息撤回成功");
            }
        }
        Err(e) => {
            warn!("⚠️  消息发送失败: {}", e);
        }
    }
    
    println!();
    
    // 示例5：实时交互功能
    println!("📋 示例5: 实时交互功能");
    
    // 演示正在输入指示器
    println!("⌨️  开始输入指示器...");
    if let Err(e) = sdk.start_typing("channel_demo").await {
        warn!("⚠️  开始输入失败: {}", e);
    } else {
        println!("✅ 输入指示器已启动");
        
        // 模拟输入过程
        tokio::time::sleep(Duration::from_millis(2000)).await;
        
        println!("⌨️  停止输入指示器...");
        if let Err(e) = sdk.stop_typing("channel_demo").await {
            warn!("⚠️  停止输入失败: {}", e);
        } else {
            println!("✅ 输入指示器已停止");
        }
    }
    
    // 演示表情反馈
    println!("😊 添加表情反馈...");
    if let Err(e) = sdk.add_reaction("demo_message_123", "👍").await {
        warn!("⚠️  添加表情失败: {}", e);
    } else {
        println!("✅ 表情反馈添加成功");
        
        // 等待一下，然后移除表情
        tokio::time::sleep(Duration::from_millis(1000)).await;
        
        println!("😊 移除表情反馈...");
        if let Err(e) = sdk.remove_reaction("demo_message_123", "👍").await {
            warn!("⚠️  移除表情失败: {}", e);
        } else {
            println!("✅ 表情反馈移除成功");
        }
    }
    
    println!();
    
    // 示例6：高级功能演示
    println!("📋 示例6: 高级功能演示");
    
    // 演示消息编辑
    println!("✏️  演示消息编辑...");
    let edit_result = sdk.edit_message("demo_message_456", "这是编辑后的消息内容").await;
    match edit_result {
        Ok(_) => println!("✅ 消息编辑成功"),
        Err(e) => warn!("⚠️  消息编辑失败: {}", e),
    }
    
    // 演示事件过滤
    println!("🔍 演示事件过滤...");
    let filter = EventFilter::new()
        .with_channel_ids(vec!["channel_demo".to_string()])
        .with_event_types(vec!["message_received".to_string(), "typing_indicator".to_string()]);
    
    println!("✅ 事件过滤器配置完成: {:?}", filter);
    
    println!();
    
    // 示例7：SDK 状态查询
    println!("📋 示例7: SDK 状态查询");
    
    println!("📊 SDK 状态信息:");
    println!("   初始化状态: {}", sdk.is_initialized().await);
    println!("   连接状态: {}", sdk.is_connected().await);
    println!("   用户ID: {}", sdk.user_id());
    println!("   服务器地址: {}", sdk.config().server_url);
    
    // 显示配置信息
    println!("📈 配置信息:");
    println!("   连接超时: {} 秒", sdk.config().connection_timeout);
    println!("   心跳间隔: {} 秒", sdk.config().heartbeat_interval);
    println!("   调试模式: {}", sdk.config().debug_mode);
    
    println!();
    
    // 示例8：错误处理和恢复
    println!("📋 示例8: 错误处理和恢复");
    
    // 演示网络错误恢复
    println!("🔧 模拟网络错误...");
    // 这里可以添加网络错误模拟代码
    
    println!("🔄 SDK 会自动处理网络错误和重连");
    println!("💡 所有操作都有超时和重试机制");
    
    println!();
    
    // 示例9：资源清理
    println!("📋 示例9: 资源清理");
    
    println!("🔌 断开连接...");
    if let Err(e) = sdk.disconnect().await {
        warn!("⚠️  断开连接失败: {}", e);
    } else {
        println!("✅ 连接已断开");
    }
    
    println!("🛑 关闭 SDK...");
    if let Err(e) = sdk.shutdown().await {
        warn!("⚠️  SDK 关闭失败: {}", e);
    } else {
        println!("✅ SDK 已关闭");
    }
    
    // 停止事件监听任务
    event_task.abort();
    
    println!();
    
    // 总结
    println!("🎉 PrivChat SDK 演示完成！");
    println!("================================");
    println!();
    println!("💡 功能特性总结:");
    println!("✅ 统一的 SDK 接口，简化集成");
    println!("✅ 完整的生命周期管理");
    println!("✅ 事件驱动的架构");
    println!("✅ 自动重连和错误恢复");
    println!("✅ 实时交互功能（输入指示器、表情反馈）");
    println!("✅ 高级消息功能（编辑、撤回、已读回执）");
    println!("✅ 灵活的配置系统");
    println!("✅ 异步优先设计");
    println!("✅ FFI 兼容接口");
    println!();
    println!("📚 接下来可以:");
    println!("1. 集成到实际的应用中");
    println!("2. 通过 FFI 接口在其他语言中使用");
    println!("3. 根据需要扩展更多功能");
    println!("4. 优化性能和资源使用");
    
    Ok(())
}
