use privchat_sdk::{
    PrivchatSDK, SDKConfig,
    sdk::TransportProtocol,
    events::EventFilter,
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
        .data_dir("./demo_data")
        .assets_dir("./assets")  // 设置SQL脚本目录
        .servers(vec![
            "quic://127.0.0.1:8803",        // 优先使用QUIC（高性能）
            "tcp://127.0.0.1:8801",         // 降级到TCP（稳定）
            "wss://127.0.0.1:8802/path"     // 最后使用WebSocket（兼容性强）
        ])
        .connection_timeout(30)
        .heartbeat_interval(30)
        .debug_mode(true)
        .build();
    
    let sdk = PrivchatSDK::initialize(config).await?;
    
    println!("✅ SDK 初始化完成");
    
    // 显示服务器配置信息
    let server_config = &sdk.config().server_config;
    println!("📍 配置的服务器端点:");
    for (index, endpoint) in server_config.endpoints.iter().enumerate() {
        let protocol_name = match endpoint.protocol {
            TransportProtocol::Quic => "QUIC",
            TransportProtocol::Tcp => "TCP",
            TransportProtocol::WebSocket => "WebSocket",
        };
        let path = endpoint.path.as_deref().unwrap_or("");
        println!("  {}. {} 服务器: {}:{}{} (TLS: {})", 
            index + 1, protocol_name, endpoint.host, endpoint.port, path, endpoint.use_tls);
    }
    println!("📍 存储路径: {}", sdk.config().data_dir.display());
    if let Some(assets_dir) = &sdk.config().assets_dir {
        println!("📍 Assets目录: {}", assets_dir.display());
    }
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
    
    // 连接到服务器（现在需要提供用户ID和token）
    println!("🔗 正在连接到服务器...");
    println!("💡 尝试协议降级: QUIC → TCP → WebSocket");
    match sdk.connect("demo_user_123", "demo_token_456").await {
        Ok(_) => {
            println!("✅ 连接成功");
            if let Some(user_id) = sdk.user_id().await {
                println!("👤 当前用户: {}", user_id);
            }
        }
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
        Err(e) => {
            warn!("⚠️  消息编辑失败: {}", e);
            println!("💡 这是演示模式，编辑失败是正常的");
        }
    }
    
    // 演示事件过滤
    println!("🔍 演示事件过滤...");
    let event_filter = EventFilter::new()
        .with_event_types(vec!["message_received".to_string(), "typing_indicator".to_string()])
        .with_channel_ids(vec!["channel_demo".to_string()]);
    
    let mut filtered_receiver = sdk.events().subscribe_filtered(event_filter).await;
    
    // 启动过滤事件监听任务
    let filtered_task = tokio::spawn(async move {
        if let Ok(event) = tokio::time::timeout(Duration::from_millis(100), filtered_receiver.recv()).await {
            if let Ok(event) = event {
                println!("🔍 过滤事件: {}", event.event_type());
            }
        }
    });
    
    println!("✅ 事件过滤器设置完成");
    println!();
    
    // 示例7：SDK状态查询
    println!("📋 示例7: SDK状态查询");
    
    println!("📊 SDK 运行状态:");
    println!("  - 已初始化: {}", sdk.is_initialized().await);
    println!("  - 已连接: {}", sdk.is_connected().await);
    println!("  - 正在关闭: {}", sdk.is_shutting_down().await);
    
    if let Some(user_id) = sdk.user_id().await {
        println!("  - 当前用户: {}", user_id);
    } else {
        println!("  - 当前用户: 未登录");
    }
    
    println!("📊 配置信息:");
    println!("  - 连接超时: {}秒", sdk.config().connection_timeout);
    println!("  - 心跳间隔: {}秒", sdk.config().heartbeat_interval);
    println!("  - 调试模式: {}", sdk.config().debug_mode);
    println!("  - 队列大小: {}", sdk.config().queue_config.send_queue_size);
    println!("  - 工作线程: {}", sdk.config().queue_config.worker_threads);
    
    println!("📊 事件统计:");
    let event_stats = sdk.events().get_stats().await;
    println!("  - 总事件数: {}", event_stats.total_events);
    println!("  - 监听器数: {}", event_stats.listener_count);
    println!("  - 订阅者数: {}", sdk.events().subscriber_count());
    
    println!();
    
    // 示例8：错误处理和恢复
    println!("📋 示例8: 错误处理和恢复");
    
    // 演示错误处理
    println!("🔧 测试错误处理...");
    
    // 尝试在未连接状态下发送消息
    sdk.disconnect().await?;
    match sdk.send_message("test_channel", "这应该会失败").await {
        Ok(_) => println!("❌ 意外成功"),
        Err(e) => println!("✅ 正确捕获错误: {}", e),
    }
    
    // 演示重连机制（模拟）
    println!("🔄 模拟重连机制...");
    match sdk.connect("demo_user_123", "demo_token_456").await {
        Ok(_) => println!("✅ 重连成功"),
        Err(e) => {
            warn!("⚠️  重连失败: {}", e);
            println!("💡 这是演示模式，重连失败是正常的");
        }
    }
    
    println!();
    
    // 示例9：资源清理
    println!("📋 示例9: 资源清理");
    
    println!("🧹 正在清理资源...");
    
    // 停止事件监听任务
    event_task.abort();
    filtered_task.abort();
    
    // 断开连接
    sdk.disconnect().await?;
    println!("✅ 连接已断开");
    
    // 关闭SDK
    sdk.shutdown().await?;
    println!("✅ SDK 已关闭");
    
    println!("📊 最终状态:");
    println!("  - 已初始化: {}", sdk.is_initialized().await);
    println!("  - 已连接: {}", sdk.is_connected().await);
    
    println!("\n🎉 示例演示完成！");
    println!("================================");
    
    Ok(())
}
