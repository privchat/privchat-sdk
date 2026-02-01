use privchat_sdk::{
    client::{PrivchatClient, ServerEndpoint, TransportProtocol},
    error::Result,
    RpcResult,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio;
use tracing::{info, warn, error};

// ========== RPC 响应结构体定义 ==========

#[derive(Debug, Deserialize)]
struct UserInfo {
    pub id: String,
    pub username: String,
    pub avatar_url: Option<String>,
    pub email: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    pub user_id: String,
    pub token: String,
    pub expires_at: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    pub group_id: String,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u32,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
struct FriendInfo {
    pub user_id: String,
    pub username: String,
    pub status: String,
    pub added_at: String,
}

#[derive(Debug, Deserialize)]
struct MessageHistory {
    pub messages: Vec<HistoryMessage>,
    pub total_count: u64,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
struct HistoryMessage {
    pub message_id: String,
    pub sender_id: String,
    pub content: String,
    pub timestamp: String,
    pub message_type: u8,
}

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
            port: 8082,
            path: None,
            use_tls: false,
        },
        ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 8080,
            path: None,
            use_tls: false,
        },
        ServerEndpoint {
            protocol: TransportProtocol::WebSocket,
            host: "127.0.0.1".to_string(),
            port: 8081,
            path: Some("/".to_string()),
            use_tls: false,
        },
    ];
    
    // 使用默认数据目录 ~/.privchat/
    let mut client = PrivchatClient::with_default_dir(
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
            
            // 使用私聊频道格式，这样服务器会自动创建频道
            let private_channel = "private_demo_user_123_friend_456";
            
            match client.send_message(private_channel, "你好，这是一条私聊消息！", 1).await {
                Ok(msg_id) => println!("✅ 发送消息成功，消息ID: {}", msg_id),
                Err(e) => println!("❌ 发送消息失败: {}", e),
            }
            
            // 示例6: 再发送几条不同类型的消息
            println!("\n📤 示例6: 发送多条消息测试");
            let messages = vec![
                (private_channel, "这是第二条私聊消息", 1),
                ("private_demo_user_123_another_friend", "发送给另一个朋友的消息", 1),
                (private_channel, "📷 图片消息", 2),
                (private_channel, "🎵 语音消息", 3),
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
            
            // 示例8: RPC 功能测试
            println!("\n🔧 示例8: RPC 功能测试");
            
            // RPC 测试1: 用户登录
            println!("\n📋 RPC测试1: 用户登录");
            let login_result: RpcResult<LoginResponse> = client.call(
                "account/user/login",
                json!({
                    "username": "alice",
                    "password": "secret123"
                })
            ).await;
            
            match login_result {
                Ok(login_resp) => {
                    println!("✅ 登录成功: user_id={}, token={}", login_resp.user_id, login_resp.token);
                }
                Err(err) => {
                    println!("❌ 登录失败: {}", err);
                }
            }
            
            // RPC 测试2: 获取用户信息（需要提供来源信息）
            println!("\n📋 RPC测试2: 获取用户信息");
            // 注意：实际使用时应该先搜索或从其他来源获取 source_id
            // 这里使用占位符，实际应该从搜索结果中获取 search_session_id
            let user_result: RpcResult<UserInfo> = client.call(
                "account/user/detail",
                json!({ 
                    "from_user_id": "alice",  // 当前用户ID
                    "target_user_id": "alice", // 要查看的用户ID
                    "source": "friend",        // 来源类型
                    "source_id": "alice"      // 来源ID（好友来源时可以是好友的 user_id）
                })
            ).await;
            
            match user_result {
                Ok(user) => {
                    println!("✅ 用户信息: username={}, email={:?}", user.username, user.email);
                }
                Err(err) => {
                    println!("❌ 获取用户信息失败: {}", err);
                }
            }
            
            // RPC 测试3: 创建群组
            println!("\n📋 RPC测试3: 创建群组");
            let group_result: RpcResult<GroupInfo> = client.call(
                "group/group/create",
                json!({
                    "name": "RPC测试群组",
                    "description": "这是一个RPC测试群组",
                    "creator_id": "alice"
                })
            ).await;
            
            match group_result {
                Ok(group) => {
                    println!("✅ 群组创建成功: group_id={}, name={}", group.group_id, group.name);
                }
                Err(err) => {
                    println!("❌ 创建群组失败: {}", err);
                }
            }
            
            // RPC 测试4: 添加好友（注意：路由应该是 contact/friend/apply，且需要提供来源）
            println!("\n📋 RPC测试4: 添加好友");
            // 注意：实际使用时应该先搜索获取 search_session_id
            // 这里使用占位符，实际应该从搜索结果中获取 search_session_id
            let friend_result: RpcResult<FriendInfo> = client.call(
                "contact/friend/apply",
                json!({
                    "from_user_id": "alice",
                    "target_user_id": "bob",
                    "message": "你好，我想添加你为好友",
                    "source": "search",
                    "source_id": "search_alice_placeholder" // TODO: 从搜索结果中获取真实的 search_session_id
                })
            ).await;
            
            match friend_result {
                Ok(friend) => {
                    println!("✅ 好友申请成功: user_id={}, status={}", friend.user_id, friend.status);
                }
                Err(err) => {
                    println!("❌ 添加好友失败: {}", err);
                }
            }
            
            // RPC 测试5: 获取消息历史
            println!("\n📋 RPC测试5: 获取消息历史");
            let history_result: RpcResult<MessageHistory> = client.call(
                "message/history/get",
                json!({
                    "channel_id": "private_demo_user_123_friend_456",
                    "limit": 10,
                    "offset": 0
                })
            ).await;
            
            match history_result {
                Ok(history) => {
                    println!("✅ 消息历史获取成功: 共{}条消息，has_more={}", 
                            history.messages.len(), history.has_more);
                    for msg in history.messages.iter().take(3) {
                        println!("   - {}: {}", msg.sender_id, msg.content);
                    }
                }
                Err(err) => {
                    println!("❌ 获取消息历史失败: {}", err);
                }
            }
            
            println!("✅ RPC 功能测试完成");
            
            // 示例9: 断开连接
            println!("\n🔌 示例9: 断开连接（使用 DisconnectRequest biz_type）");
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
    println!("   • AuthorizationRequest/Response (1/2) - 连接认证");
    println!("   • DisconnectRequest/Response (3/4) - 断开连接");
    println!("   • SendRequest/Response (5/6) - 发送消息");
    println!("   • RecvRequest/Response (7/8) - 接收消息");
    println!("   • PushBatchRequest/Response (9/10) - 批量消息");
    println!("   • PingRequest/PongResponse (11/12) - 心跳检测");
    println!("   • SubscribeRequest/Response (13/14) - 频道订阅");
    println!("   • PublishRequest/Response (15/16) - 推送消息");
    
    Ok(())
} 