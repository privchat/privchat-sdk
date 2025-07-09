use privchat_sdk::{
    PrivchatClient, ServerEndpoint, TransportProtocol, RpcResult,
    error::Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{info, error};

// ========== 定义响应结构体 ==========

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
    pub status: String, // "pending", "accepted", "rejected"
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
    
    println!("🚀 PrivChat SDK RPC 调用示例");
    println!("===============================\n");
    
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
    
    // 创建客户端
    let mut client = PrivchatClient::new(
        "./demo_data_rpc",
        server_endpoints,
        Duration::from_secs(10),
    ).await?;
    
    println!("✅ 客户端初始化成功");
    
    // 连接到服务器
    match client.connect("13800138000", "test_token_123").await {
        Ok(session) => {
            println!("✅ 连接成功，用户ID: {}", session.user_id);
        }
        Err(e) => {
            error!("❌ 连接失败: {}", e);
            return Ok(());
        }
    }
    
    println!("\n🔧 开始 RPC 调用测试...\n");
    
    // ========== 示例1：用户登录 ==========
    println!("📋 示例1: 用户登录");
    let login_result: RpcResult<LoginResponse> = client.call(
        "account/user/login",
        json!({
            "username": "alice",
            "password": "secret123"
        })
    ).await;
    
    match login_result {
        Ok(login_resp) => {
            info!("✅ 登录成功: user_id={}, token={}", login_resp.user_id, login_resp.token);
        }
        Err(err) => {
            error!("❌ 登录失败: {}", err);
        }
    }
    
    // ========== 示例2：获取用户信息 ==========
    println!("\n📋 示例2: 获取用户信息");
    let user_result: RpcResult<UserInfo> = client.call(
        "account/user/find",
        json!({ "user_id": "123" })
    ).await;
    
    match user_result {
        Ok(user) => {
            info!("✅ 用户信息: username={}, email={:?}", user.username, user.email);
        }
        Err(err) => {
            error!("❌ 获取用户信息失败: {}", err);
        }
    }
    
    // ========== 示例3：创建群组 ==========
    println!("\n📋 示例3: 创建群组");
    let group_result: RpcResult<GroupInfo> = client.call(
        "group/group/create",
        json!({
            "name": "我的测试群组",
            "description": "这是一个RPC测试群组",
            "creator_id": "123"
        })
    ).await;
    
    match group_result {
        Ok(group) => {
            info!("✅ 群组创建成功: group_id={}, name={}", group.group_id, group.name);
            
            // ========== 示例4：添加群成员 ==========
            println!("\n📋 示例4: 添加群成员");
            let add_result: RpcResult<serde_json::Value> = client.call(
                "group/group/add",
                json!({
                    "group_id": group.group_id,
                    "user_id": "456",
                    "inviter_id": "123"
                })
            ).await;
            
            match add_result {
                Ok(result) => {
                    info!("✅ 添加群成员成功: {:?}", result);
                }
                Err(err) => {
                    error!("❌ 添加群成员失败: {}", err);
                }
            }
        }
        Err(err) => {
            error!("❌ 创建群组失败: {}", err);
        }
    }
    
    // ========== 示例5：申请好友 ==========
    println!("\n📋 示例5: 申请好友");
    let friend_result: RpcResult<FriendInfo> = client.call(
        "contact/friend/apply",
        json!({
            "target_user_id": "789",
            "message": "你好，我想加你为好友",
            "source": "search"
        })
    ).await;
    
    match friend_result {
        Ok(friend) => {
            info!("✅ 好友申请发送成功: user_id={}, status={}", friend.user_id, friend.status);
        }
        Err(err) => {
            error!("❌ 好友申请失败: {}", err);
        }
    }
    
    // ========== 示例6：获取消息历史 ==========
    println!("\n📋 示例6: 获取消息历史");
    let history_result: RpcResult<MessageHistory> = client.call(
        "message/history/get",
        json!({
            "session_id": "session_123",
            "limit": 20,
            "offset": 0
        })
    ).await;
    
    match history_result {
        Ok(history) => {
            info!("✅ 获取消息历史成功: 消息数量={}, 总数={}", 
                  history.messages.len(), history.total_count);
            for msg in &history.messages[..std::cmp::min(3, history.messages.len())] {
                info!("  消息: {} -> {}", msg.sender_id, msg.content);
            }
        }
        Err(err) => {
            error!("❌ 获取消息历史失败: {}", err);
        }
    }
    
    // ========== 示例7：错误处理演示 ==========
    println!("\n📋 示例7: 错误处理演示");
    let error_result: RpcResult<serde_json::Value> = client.call(
        "invalid/route/test",
        json!({ "test": "data" })
    ).await;
    
    match error_result {
        Ok(_) => {
            info!("意外成功");
        }
        Err(err) => {
            info!("✅ 错误处理正常: [code={}] {}", err.code, err.message);
        }
    }
    
    println!("\n🎉 RPC 调用测试完成！");
    
    // 断开连接
    client.disconnect("测试完成").await?;
    println!("✅ 连接已断开");
    
    Ok(())
} 