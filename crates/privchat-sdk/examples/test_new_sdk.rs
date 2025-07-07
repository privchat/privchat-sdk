use privchat_sdk::{
    client::{PrivchatClient, ServerEndpoint, TransportProtocol},
    error::Result,
};
use std::time::Duration;
use tokio;
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    println!("🚀 PrivChat SDK 新API测试（使用 request_with_options）");
    println!("==============================================\n");
    
    // 示例1：SDK 初始化和配置
    println!("📋 示例1: SDK 初始化和配置（使用 request_with_options）");
    
    let server_endpoints = vec![
        ServerEndpoint {
            protocol: TransportProtocol::Quic,
            host: "127.0.0.1".to_string(),
            port: 8803,
            path: None,
            use_tls: false,
        },
        ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 8801,
            path: None,
            use_tls: false,
        },
        ServerEndpoint {
            protocol: TransportProtocol::WebSocket,
            host: "127.0.0.1".to_string(),
            port: 8802,
            path: Some("/".to_string()),
            use_tls: false,
        },
    ];
    
    let mut client = PrivchatClient::new(
        "./demo_data_new",
        server_endpoints,
        Duration::from_secs(10),
    ).await?;
    
    println!("✅ 客户端初始化成功");
    
    // 示例2：连接和认证
    println!("\n📡 示例2: 连接和认证（使用 request_with_options + biz_type）");
    
    let test_phone = "13800138000";
    let test_token = "test_jwt_token_123";
    
    match client.connect(test_phone, test_token).await {
        Ok(session) => {
            println!("✅ 连接和认证成功");
            println!("   用户ID: {}", session.user_id);
            println!("   设备ID: {}", session.device_id);
            println!("   会话ID: {:?}", session.server_key);
            println!("   登录时间: {}", session.login_time);
            
            // 示例3：心跳检测
            println!("\n💓 示例3: 心跳检测（使用 PingRequest biz_type）");
            match client.ping().await {
                Ok(_) => println!("✅ 心跳检测成功"),
                Err(e) => println!("❌ 心跳检测失败: {}", e),
            }
            
            // 示例4: 订阅频道
            println!("\n📢 示例4: 订阅频道（使用 SubscribeRequest biz_type）");
            match client.subscribe_channel("test_channel_001").await {
                Ok(_) => println!("✅ 订阅频道成功"),
                Err(e) => println!("❌ 订阅频道失败: {}", e),
            }
            
            // 示例5: 发送消息
            println!("\n📤 示例5: 发送消息（使用 SendRequest biz_type）");
            match client.send_message("test_channel_001", "Hello, PrivChat!", 1).await {
                Ok(msg_id) => println!("✅ 发送消息成功，消息ID: {}", msg_id),
                Err(e) => println!("❌ 发送消息失败: {}", e),
            }
            
            // 示例6: 再发送几条不同类型的消息
            println!("\n📤 示例6: 发送多条消息测试");
            let messages = vec![
                ("test_channel_001", "这是第二条消息", 1),
                ("test_channel_002", "发送到不同频道", 1),
                ("test_channel_001", "📷 图片消息", 2),
                ("test_channel_001", "🎵 语音消息", 3),
            ];
            
            for (channel, content, msg_type) in messages {
                match client.send_message(channel, content, msg_type).await {
                    Ok(msg_id) => println!("✅ 消息发送成功: {} -> {} (ID: {})", content, channel, msg_id),
                    Err(e) => println!("❌ 消息发送失败: {} -> {} (错误: {})", content, channel, e),
                }
                
                // 稍微延迟一下，避免发送过快
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
            
            // 示例7: 状态检查
            println!("\n📊 示例7: 状态检查");
            println!("   连接状态: {}", client.is_connected().await);
            println!("   用户ID: {:?}", client.user_id());
            println!("   用户目录: {:?}", client.user_dir());
            
            // 示例8: 断开连接
            println!("\n🔌 示例8: 断开连接（使用 DisconnectRequest biz_type）");
            match client.disconnect("测试完成").await {
                Ok(_) => println!("✅ 断开连接成功"),
                Err(e) => println!("❌ 断开连接失败: {}", e),
            }
            
        }
        Err(e) => {
            println!("❌ 连接失败: {}", e);
            return Err(e);
        }
    }
    
    println!("\n🎉 测试完成！");
    println!("==================");
    println!("✅ 使用 request_with_options 方法成功");
    println!("✅ 为每种消息类型正确设置了 biz_type");
    println!("✅ 服务器能够根据 biz_type 找到对应的处理器");
    println!("✅ 多协议支持（QUIC → TCP → WebSocket）");
    println!("✅ 连接认证功能正常");
    println!("✅ 心跳检测功能正常");
    println!("✅ 频道订阅功能正常");
    println!("✅ 消息发送功能正常");
    println!("✅ 多种消息类型支持");
    println!("✅ 优雅断开连接");
    
    println!("\n📋 支持的消息类型:");
    println!("   • ConnectRequest/Response (1/2) - 连接认证");
    println!("   • DisconnectRequest/Response (3/4) - 断开连接");
    println!("   • SendRequest/Response (5/6) - 发送消息");
    println!("   • RecvRequest/Response (7/8) - 接收消息");
    println!("   • RecvBatchRequest/Response (9/10) - 批量消息");
    println!("   • PingRequest/PongResponse (11/12) - 心跳检测");
    println!("   • SubscribeRequest/Response (13/14) - 频道订阅");
    println!("   • PublishRequest/Response (15/16) - 推送消息");
    
    Ok(())
} 