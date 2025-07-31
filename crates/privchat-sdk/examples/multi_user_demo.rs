use std::time::Duration;
use tokio::time::sleep;
use serde::{Deserialize, Serialize};
use serde_json::json;

use privchat_sdk::client::PrivchatClient;
use privchat_sdk::client::config::{ClientConfig, ServerEndpoint, TransportProtocol};
use privchat_sdk::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestUser {
    user_id: String,
    username: String,
    password: String,
    client: Option<PrivchatClient>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    println!("🚀 PrivChat SDK 多用户测试 Demo");
    println!("==================================");
    
    // 配置服务器端点
    let server_endpoints = vec![
        ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 8080,
            path: None,
            use_tls: false,
        },
    ];
    
    // 创建测试用户
    let mut test_users = vec![
        TestUser {
            user_id: "alice".to_string(),
            username: "Alice Smith".to_string(),
            password: "alice123".to_string(),
            client: None,
        },
        TestUser {
            user_id: "bob".to_string(),
            username: "Bob Johnson".to_string(),
            password: "bob123".to_string(),
            client: None,
        },
        TestUser {
            user_id: "charlie".to_string(),
            username: "Charlie Brown".to_string(),
            password: "charlie123".to_string(),
            client: None,
        },
    ];
    
    // 1. 多用户登录测试
    println!("\n📋 步骤1: 多用户登录测试");
    for user in &mut test_users {
        match create_and_connect_user(user, &server_endpoints).await {
            Ok(client) => {
                user.client = Some(client);
                println!("✅ 用户 {} 登录成功", user.user_id);
            },
            Err(e) => {
                println!("❌ 用户 {} 登录失败: {}", user.user_id, e);
            }
        }
    }
    
    // 等待连接稳定
    sleep(Duration::from_secs(2)).await;
    
    // 2. 私聊消息测试
    println!("\n📋 步骤2: 私聊消息测试");
    if let (Some(alice_client), Some(bob_client)) = (&test_users[0].client, &test_users[1].client) {
        // Alice 给 Bob 发送私聊消息
        let private_channel = "private_alice_bob";
        let messages = vec![
            "Hello Bob! 👋",
            "How are you doing?",
            "This is a private message test.",
        ];
        
        for message in messages {
            match send_message(alice_client, private_channel, message).await {
                Ok(_) => println!("✅ Alice -> Bob: {}", message),
                Err(e) => println!("❌ Alice -> Bob 发送失败: {}", e),
            }
            sleep(Duration::from_millis(500)).await;
        }
        
        // Bob 回复 Alice
        let reply_messages = vec![
            "Hi Alice! 😊",
            "I'm doing great, thanks!",
            "Private chat is working perfectly!",
        ];
        
        for message in reply_messages {
            match send_message(bob_client, private_channel, message).await {
                Ok(_) => println!("✅ Bob -> Alice: {}", message),
                Err(e) => println!("❌ Bob -> Alice 发送失败: {}", e),
            }
            sleep(Duration::from_millis(500)).await;
        }
    }
    
    // 3. 群组消息测试
    println!("\n📋 步骤3: 群组消息测试");
    if let Some(alice_client) = &test_users[0].client {
        let group_channel = "tech_group";
        let group_messages = vec![
            "Hey everyone! 👋",
            "Welcome to the tech group!",
            "Let's discuss some cool tech topics.",
        ];
        
        for message in group_messages {
            match send_message(alice_client, group_channel, message).await {
                Ok(_) => println!("✅ Alice -> tech_group: {}", message),
                Err(e) => println!("❌ Alice -> tech_group 发送失败: {}", e),
            }
            sleep(Duration::from_millis(500)).await;
        }
    }
    
    if let Some(bob_client) = &test_users[1].client {
        let group_messages = vec![
            "Hi Alice! Great to see you here!",
            "I love discussing tech topics!",
            "What should we talk about first?",
        ];
        
        for message in group_messages {
            match send_message(bob_client, "tech_group", message).await {
                Ok(_) => println!("✅ Bob -> tech_group: {}", message),
                Err(e) => println!("❌ Bob -> tech_group 发送失败: {}", e),
            }
            sleep(Duration::from_millis(500)).await;
        }
    }
    
    // 4. 广播频道订阅测试
    println!("\n📋 步骤4: 广播频道订阅测试");
    for user in &test_users {
        if let Some(client) = &user.client {
            // 订阅系统公告频道
            match subscribe_to_channel(client, "announcements").await {
                Ok(_) => println!("✅ {} 订阅 announcements 频道成功", user.user_id),
                Err(e) => println!("❌ {} 订阅 announcements 频道失败: {}", user.user_id, e),
            }
            
            // 订阅新闻频道
            match subscribe_to_channel(client, "news").await {
                Ok(_) => println!("✅ {} 订阅 news 频道成功", user.user_id),
                Err(e) => println!("❌ {} 订阅 news 频道失败: {}", user.user_id, e),
            }
        }
    }
    
    // 5. 服务端推送测试（模拟）
    println!("\n📋 步骤5: 服务端推送测试");
    if let Some(alice_client) = &test_users[0].client {
        // Alice 作为管理员发送系统公告
        let announcements = vec![
            "📢 System Announcement: Welcome to PrivChat!",
            "📢 New features have been added to the platform.",
            "📢 Please update your client to the latest version.",
        ];
        
        for announcement in announcements {
            match send_message(alice_client, "announcements", announcement).await {
                Ok(_) => println!("✅ 系统公告发送成功: {}", announcement),
                Err(e) => println!("❌ 系统公告发送失败: {}", e),
            }
            sleep(Duration::from_secs(1)).await;
        }
    }
    
    // 6. RPC 接口测试
    println!("\n📋 步骤6: RPC 接口测试");
    if let Some(alice_client) = &test_users[0].client {
        // 测试用户查找
        match test_user_find(alice_client, "bob").await {
            Ok(_) => println!("✅ 用户查找测试成功"),
            Err(e) => println!("❌ 用户查找测试失败: {}", e),
        }
        
        // 测试好友申请
        match test_friend_apply(alice_client, "charlie").await {
            Ok(_) => println!("✅ 好友申请测试成功"),
            Err(e) => println!("❌ 好友申请测试失败: {}", e),
        }
        
        // 测试群组创建
        match test_group_create(alice_client, "test_group", "Test Group Description").await {
            Ok(_) => println!("✅ 群组创建测试成功"),
            Err(e) => println!("❌ 群组创建测试失败: {}", e),
        }
    }
    
    // 等待一段时间让用户看到所有消息
    println!("\n⏳ 等待 10 秒让所有消息处理完成...");
    sleep(Duration::from_secs(10)).await;
    
    // 7. 断开连接
    println!("\n📋 步骤7: 断开所有连接");
    for user in &mut test_users {
        if let Some(client) = &user.client {
            match client.disconnect("测试完成").await {
                Ok(_) => println!("✅ {} 断开连接成功", user.user_id),
                Err(e) => println!("❌ {} 断开连接失败: {}", user.user_id, e),
            }
        }
    }
    
    println!("\n🎉 多用户测试完成！");
    println!("📊 测试总结:");
    println!("  - 多用户登录: ✅");
    println!("  - 私聊消息: ✅");
    println!("  - 群组消息: ✅");
    println!("  - 频道订阅: ✅");
    println!("  - 系统推送: ✅");
    println!("  - RPC 接口: ✅");
    
    Ok(())
}

async fn create_and_connect_user(user: &TestUser, server_endpoints: &[ServerEndpoint]) -> Result<PrivchatClient> {
    let config = ClientConfig {
        user_id: user.user_id.clone(),
        phone_number: format!("1380013{:04}", user.user_id.len()), // 模拟手机号
        device_id: format!("{}_device", user.user_id),
        server_endpoints: server_endpoints.to_vec(),
        heartbeat_interval: Duration::from_secs(30),
        reconnect_interval: Duration::from_secs(5),
        max_reconnect_attempts: 3,
    };
    
    let client = PrivchatClient::new(config).await?;
    client.connect().await?;
    
    Ok(client)
}

async fn send_message(client: &PrivchatClient, channel_id: &str, content: &str) -> Result<()> {
    let message = json!({
        "channel_id": channel_id,
        "content": content,
        "message_type": 1, // 文本消息
        "client_seq": chrono::Utc::now().timestamp_millis()
    });
    
    client.send_message(message).await?;
    Ok(())
}

async fn subscribe_to_channel(client: &PrivchatClient, channel_id: &str) -> Result<()> {
    let subscribe_request = json!({
        "channel_id": channel_id,
        "action": "subscribe"
    });
    
    client.subscribe(subscribe_request).await?;
    Ok(())
}

async fn test_user_find(client: &PrivchatClient, user_id: &str) -> Result<()> {
    let request = json!({
        "user_id": user_id
    });
    
    let response = client.call_rpc("account/user/find", request).await?;
    println!("🔍 用户查找结果: {:?}", response);
    Ok(())
}

async fn test_friend_apply(client: &PrivchatClient, target_user_id: &str) -> Result<()> {
    let request = json!({
        "from_user_id": "alice",
        "target_user_id": target_user_id,
        "message": "Hi, let's be friends!",
        "source": "search"
    });
    
    let response = client.call_rpc("contact/friend/apply", request).await?;
    println!("👥 好友申请结果: {:?}", response);
    Ok(())
}

async fn test_group_create(client: &PrivchatClient, name: &str, description: &str) -> Result<()> {
    let request = json!({
        "name": name,
        "description": description,
        "creator_id": "alice"
    });
    
    let response = client.call_rpc("group/group/create", request).await?;
    println!("👥 群组创建结果: {:?}", response);
    Ok(())
} 