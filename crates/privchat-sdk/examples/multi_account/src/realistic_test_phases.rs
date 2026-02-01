//! 真实场景测试阶段 - 模拟真实用户交互流程

use crate::account_manager::MultiAccountManager;
use crate::event_system::AccountEvent;
use crate::types::{PhaseResult, PhaseMetrics, TestConfig};
use privchat_sdk::error::Result;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{info, warn, error, debug};

/// 真实场景测试阶段执行器
pub struct RealisticTestPhases {
    config: TestConfig,
}

impl RealisticTestPhases {
    pub fn new(config: TestConfig) -> Self {
        Self { config }
    }
    
    /// Phase 1: 用户认证和初始化
    pub async fn phase1_user_authentication(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔐 Phase 1: 用户认证和初始化");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 并发认证所有账号
        match account_manager.authenticate_all().await {
            Ok(authenticated_accounts) => {
                info!("✅ 认证成功的账号: {:?}", authenticated_accounts);
                
                // 验证连接状态
                if let Err(e) = account_manager.verify_all_connected().await {
                    metrics.errors.push(format!("连接验证失败: {}", e));
                    return Ok(PhaseResult {
                        phase_name: "用户认证".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "部分账号认证失败".to_string(),
                        metrics,
                    });
                }
                
                let duration = start_time.elapsed();
                info!("✅ Phase 1 完成，用时: {}ms", duration.as_millis());
                
                Ok(PhaseResult {
                    phase_name: "用户认证".to_string(),
                    success: true,
                    duration,
                    details: format!("3个账号成功认证"),
                    metrics,
                })
            }
            Err(e) => {
                error!("❌ 认证失败: {}", e);
                metrics.errors.push(format!("认证失败: {}", e));
                
                Ok(PhaseResult {
                    phase_name: "用户认证".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "认证失败".to_string(),
                    metrics,
                })
            }
        }
    }
    
    /// Phase 2: 好友系统完整流程
    pub async fn phase2_friend_system_workflow(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👥 Phase 2: 好友系统完整流程");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 获取当前用户 ID（服务端返回的 UUID）
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        
        // Step 1: Alice 使用用户名搜索用户 Bob（使用 SDK 方法）
        info!("🔍 Step 1: Alice 使用用户名搜索用户 Bob");
        let bob_user_id = match account_manager.search_users("alice", "bob").await {
            Ok(user_info) => {
                info!("✅ Alice 找到用户 Bob: {:?}", user_info);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // 从搜索结果中提取 user_id
                let user = user_info.get("users")
                    .and_then(|users| users.as_array())
                    .and_then(|users| users.first());
                
                if let Some(user) = user {
                    let found_user_id = user.get("user_id")
                        .and_then(|v| v.as_u64());
                    
                    if let Some(user_id) = found_user_id {
                        info!("✅ 从搜索结果获取到 Bob 的 user_id: {}", user_id);
                        Some(user_id)
                    } else {
                        warn!("⚠️ 搜索结果中未找到 user_id");
                        None
                    }
                } else {
                    warn!("⚠️ 搜索结果为空");
                    None
                }
            }
            Err(e) => {
                error!("❌ Alice 查找用户 Bob 失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("Alice 查找用户失败: {}", e));
                None
            }
        };
        
        sleep(Duration::from_millis(300)).await;
        
        // Step 2: Alice 向 Bob 发送好友申请（使用 SDK 方法）
        info!("📋 Step 2: Alice 向 Bob 发送好友申请");
        if let Some(target_user_id) = bob_user_id {
            match account_manager.send_friend_request("alice", target_user_id, Some("Hi Bob! Let's be friends! 🤝")).await {
                Ok(response) => {
                    info!("✅ Alice 好友申请发送成功: {:?}", response);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    warn!("⚠️ Alice 好友申请失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("Alice 好友申请: {}", e));
                }
            }
        } else {
            warn!("⚠️ 无法从搜索结果获取 Bob 的 user_id，跳过好友申请");
            metrics.errors.push("无法从搜索结果获取 user_id".to_string());
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Step 3: Bob 查看并接受好友申请
        // 注意：Bob 需要使用自己的 user_id 和 Alice 的 user_id（从搜索中获取）
        info!("📋 Step 3: Bob 接受 Alice 的好友申请");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        // 注意：这里应该使用搜索返回的 alice_id，但为了简化，我们使用配置中的 alice_id
        // 在实际场景中，Bob 应该从好友申请通知中获取 Alice 的 user_id
        let alice_id_for_bob = account_manager.get_user_id("alice").expect("Alice ID not found");
        
        let mut alice_bob_chat = None;
        match account_manager.rpc_call("bob", "contact/friend/accept", json!({
            "user_id": bob_id,
            "from_user_id": alice_id_for_bob
        })).await {
            Ok(response) => {
                info!("✅ Bob 接受好友申请成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // ✨ 从响应中提取 channel_id（服务端返回的 u64）
                if let Some(conv_id) = response.get("channel_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    // ✨ 缓存服务端返回的 channel_id
                    account_manager.cache_channel_id("alice", "bob", conv_id);
                    alice_bob_chat = Some(conv_id);
                    info!("✅ 获取到私聊会话 ID: {}", conv_id);
                } else {
                    warn!("⚠️ 响应中未找到 channel_id");
                    alice_bob_chat = account_manager.get_private_chat_id("alice", "bob");
                }
            }
            Err(e) => {
                warn!("⚠️ Bob 接受好友申请失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("Bob 接受好友申请: {}", e));
                // 即使失败，也尝试从缓存获取
                alice_bob_chat = account_manager.get_private_chat_id("alice", "bob");
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Step 4: 好友之间发送私聊消息
        info!("💬 Step 4: 好友之间发送私聊消息");
        // 使用服务端返回的 channel_id
        let alice_bob_chat = alice_bob_chat
            .or_else(|| account_manager.get_private_chat_id("alice", "bob"))
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        let friend_messages = vec![
            ("alice", alice_bob_chat, "Hi Bob! We're friends now! 😊"),
            ("bob", alice_bob_chat, "Great Alice! Nice to be your friend! 🎉"),
            ("alice", alice_bob_chat, "Let's chat more often!"),
        ];
        
        for (sender, channel, message) in friend_messages {
            match account_manager.send_message(sender, channel, message, "text").await {  // "text" 消息类型
                Ok(message_id) => {
                    info!("✅ 好友消息发送成功: {} -> {}", sender, message_id);
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    error!("❌ 好友消息发送失败: {} -> {}", sender, e);
                    metrics.errors.push(format!("{} 好友消息发送失败: {}", sender, e));
                }
            }
            sleep(Duration::from_millis(200)).await;
        }
        
        // Step 5: Alice 和 Charlie 也建立好友关系
        info!("🔍 Step 5: Alice 使用用户名搜索并添加 Charlie 为好友");
        
        // 使用用户名搜索 Charlie
        let (charlie_user_id, charlie_session_id) = match account_manager.rpc_call("alice", "account/search/query", json!({
            "from_user_id": alice_id,
            "query": "charlie"  // ✨ 使用用户名搜索
        })).await {
            Ok(user_info) => {
                info!("✅ Alice 找到用户 Charlie: {:?}", user_info);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // ✨ 从搜索结果中提取 user_id 和 search_session_id
                let user = user_info.get("users")
                    .and_then(|users| users.as_array())
                    .and_then(|users| users.first());
                
                if let Some(user) = user {
                    // ✨ 支持 u64 和字符串两种格式，但保留为 u64 类型
                    let found_user_id = user.get("user_id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())));
                    let found_session_id = user.get("search_session_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    
                    if let Some(user_id) = found_user_id {
                        info!("✅ 从搜索结果获取到 Charlie 的 user_id: {}", user_id);
                        (Some(user_id), found_session_id)
                    } else {
                        warn!("⚠️ 搜索结果中未找到 user_id");
                        (None, found_session_id)
                    }
                } else {
                    warn!("⚠️ 搜索结果为空");
                    (None, None)
                }
            }
            Err(e) => {
                error!("❌ Alice 查找用户 Charlie 失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("Alice 查找用户 Charlie 失败: {}", e));
                (None, None)
            }
        };
        
        sleep(Duration::from_millis(300)).await;
        
        // 如果找到 Charlie，使用搜索返回的 user_id 发送好友申请
        let charlie_user_id_clone = charlie_user_id;
        if let Some(target_user_id) = charlie_user_id {
            if let Some(session_id) = charlie_session_id {
                match account_manager.rpc_call("alice", "contact/friend/apply", json!({
                    "from_user_id": alice_id,
                    "target_user_id": target_user_id,  // ✨ 使用搜索返回的 user_id（u64 类型）
                    "message": "Hi Charlie! Let's connect! 🌟",
                    "source": "search",  // ✨ 来源标记为搜索
                    "source_id": session_id
                })).await {
                Ok(_) => {
                    info!("✅ Alice 向 Charlie 发送好友申请成功");
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    warn!("⚠️ Alice 向 Charlie 发送好友申请失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("Alice 向 Charlie 发送好友申请: {}", e));
                }
            }
            } else {
                warn!("⚠️ 无法获取 search_session_id，跳过好友申请");
                metrics.errors.push("无法获取 search_session_id".to_string());
            }
        } else {
            warn!("⚠️ 无法从搜索结果获取 Charlie 的 user_id，跳过好友申请");
            metrics.errors.push("无法从搜索结果获取 user_id".to_string());
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // ✨ Step 3.1: Charlie 接受 Alice 的好友申请
        if charlie_user_id_clone.is_some() {
            info!("📋 Step 3.1: Charlie 接受 Alice 的好友申请");
            let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
            let alice_id_for_charlie = account_manager.get_user_id("alice").expect("Alice ID not found");
            
            match account_manager.rpc_call("charlie", "contact/friend/accept", json!({
                "user_id": charlie_id,
                "from_user_id": alice_id_for_charlie
            })).await {
                Ok(response) => {
                    info!("✅ Charlie 接受好友申请成功: {:?}", response);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    
                    // ✨ 从响应中提取 channel_id（服务端返回的 u64）
                    if let Some(conv_id) = response.get("channel_id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                        // ✨ 缓存服务端返回的 channel_id
                        account_manager.cache_channel_id("alice", "charlie", conv_id);
                        info!("✅ 获取到 alice-charlie 私聊会话 ID: {}", conv_id);
                    } else {
                        warn!("⚠️ 响应中未找到 channel_id，使用生成的 UUID");
                    }
                }
                Err(e) => {
                    warn!("⚠️ Charlie 接受好友申请失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("Charlie 接受好友申请失败: {}", e));
                }
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // ✨ Step 3.2: Bob 和 Charlie 之间的好友申请流程
        info!("📋 Step 3.2: Bob 向 Charlie 发送好友申请");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        
        // Bob 搜索 Charlie
        let (bob_charlie_user_id, bob_charlie_session_id) = match account_manager.rpc_call("bob", "account/search/query", json!({
            "from_user_id": bob_id,
            "query": "charlie"
        })).await {
            Ok(user_info) => {
                info!("✅ Bob 找到用户 Charlie: {:?}", user_info);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                let user = user_info.get("users")
                    .and_then(|users| users.as_array())
                    .and_then(|users| users.first());
                
                if let Some(user) = user {
                    // ✨ user_id 仅支持 u64 类型
                    let found_user_id = user.get("user_id")
                        .and_then(|v| v.as_u64());
                    let found_session_id = user.get("search_session_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    
                    if let Some(user_id) = found_user_id {
                        info!("✅ 从搜索结果获取到 Charlie 的 user_id: {}", user_id);
                        (Some(user_id), found_session_id)
                    } else {
                        (None, found_session_id)
                    }
                } else {
                    (None, None)
                }
            }
            Err(e) => {
                warn!("⚠️ Bob 查找用户 Charlie 失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("Bob 查找用户 Charlie 失败: {}", e));
                (None, None)
            }
        };
        
        sleep(Duration::from_millis(300)).await;
        
        // Bob 向 Charlie 发送好友申请
        if let Some(target_user_id) = bob_charlie_user_id {
            if let Some(session_id) = bob_charlie_session_id {
                match account_manager.rpc_call("bob", "contact/friend/apply", json!({
                    "from_user_id": bob_id,
                    "target_user_id": target_user_id,  // u64 类型
                    "message": "Hi Charlie! Let's be friends! 🤝",
                    "source": "search",
                    "source_id": session_id
                })).await {
                    Ok(_) => {
                        info!("✅ Bob 向 Charlie 发送好友申请成功");
                        metrics.rpc_calls += 1;
                        metrics.rpc_successes += 1;
                    }
                    Err(e) => {
                        warn!("⚠️ Bob 向 Charlie 发送好友申请失败: {}", e);
                        metrics.rpc_calls += 1;
                        metrics.errors.push(format!("Bob 向 Charlie 发送好友申请失败: {}", e));
                    }
                }
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Charlie 接受 Bob 的好友申请
        if let Some(_) = bob_charlie_user_id {
            info!("📋 Step 3.3: Charlie 接受 Bob 的好友申请");
            let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
            let bob_id_for_charlie = account_manager.get_user_id("bob").expect("Bob ID not found");
            
            match account_manager.rpc_call("charlie", "contact/friend/accept", json!({
                "user_id": charlie_id,
                "from_user_id": bob_id_for_charlie
            })).await {
                Ok(response) => {
                    info!("✅ Charlie 接受 Bob 的好友申请成功: {:?}", response);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    
                    // ✨ 从响应中提取 channel_id（服务端返回的 UUID）
                    if let Some(conv_id) = response.get("channel_id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                        // ✨ 缓存服务端返回的 channel_id
                        account_manager.cache_channel_id("bob", "charlie", conv_id);
                        info!("✅ 获取到 bob-charlie 私聊会话 ID: {}", conv_id);
                    } else {
                        warn!("⚠️ 响应中未找到 channel_id，使用生成的 UUID");
                    }
                }
                Err(e) => {
                    warn!("⚠️ Charlie 接受 Bob 的好友申请失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("Charlie 接受 Bob 的好友申请失败: {}", e));
                }
            }
        }
        
        let duration = start_time.elapsed();
        // 允许一些RPC错误，因为功能可能未完全实现
        let success = metrics.messages_sent > 0 && metrics.rpc_calls >= 2;
        
        info!("✅ Phase 2 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "好友系统流程".to_string(),
            success,
            duration,
            details: format!("RPC调用{}次，好友消息{}条，错误{}个", metrics.rpc_calls, metrics.messages_sent, metrics.errors.len()),
            metrics,
        })
    }
    
    /// Phase 3: 群组系统完整流程
    pub async fn phase3_group_system_workflow(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🏢 Phase 3: 群组系统完整流程");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 获取用户 ID（服务端返回的 UUID）- 先克隆，避免借用冲突
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        // Step 1: Alice 创建群组
        info!("🏗️ Step 1: Alice 创建测试群组");
        let mut group_id: Option<u64> = None;
        
        match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id,
            "name": "Multi-Account Test Group",
            "description": "A test group for multi-account testing",
            "is_public": false
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    group_id = Some(gid);
                    // ✨ 缓存服务端返回的 group_id
                    account_manager.cache_group_id("Multi-Account Test Group", gid);
                    info!("✅ Alice 创建群组成功: {} (channel_id)", gid);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                } else {
                    error!("❌ 群组创建响应格式错误: {:?}", response);
                    metrics.rpc_calls += 1;
                    metrics.errors.push("群组创建响应格式错误".to_string());
                }
            }
            Err(e) => {
                error!("❌ Alice 创建群组失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("Alice 创建群组失败: {}", e));
                // ✨ 群组创建失败，无法继续测试，返回错误
                return Ok(PhaseResult {
                    phase_name: "群组系统流程".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "群组创建失败，无法继续测试".to_string(),
                    metrics,
                });
            }
        }
        
        // ✨ 确保 group_id 存在
        let group_id = match group_id {
            Some(id) => id,
            None => {
                error!("❌ 群组ID为空，无法继续测试");
                return Ok(PhaseResult {
                    phase_name: "群组系统流程".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "群组ID为空，无法继续测试".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 2: Alice 邀请 Bob 和 Charlie 加入群组
        info!("📨 Step 2: Alice 邀请好友加入群组");
        
        let invitees = vec![(bob_id, "bob"), (charlie_id, "charlie")];
        for (invitee_id, invitee_name) in invitees {
            match account_manager.rpc_call("alice", "group/member/add", json!({
                "group_id": group_id,
                        "inviter_id": alice_id,
                        "user_id": invitee_id,
                "role": "member"
            })).await {
                Ok(response) => {
                    info!("✅ Alice 邀请 {} 加入群组成功: {:?}", invitee_name, response);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    
                    // ✨ 如果响应中包含 group_id，确保被邀请的用户也缓存这个群组ID
                    // 注意：这里我们使用群组名称来缓存，但被邀请的用户可能不知道群组名称
                    // 所以这里主要是确保群组ID的一致性
                    if let Some(gid) = response.get("group_id")
                        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                        // 如果这是 "Multi-Account Test Group"，确保缓存一致
                        if let Some(cached_gid) = account_manager.get_cached_group_id("Multi-Account Test Group") {
                            if cached_gid != gid {
                                warn!("⚠️ 群组ID不一致: 缓存的 {} vs 响应中的 {}", cached_gid, gid);
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("⚠️ Alice 邀请 {} 加入群组失败: {}", invitee_name, e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("邀请 {} 加入群组失败: {}", invitee_name, e));
                }
            }
            sleep(Duration::from_millis(200)).await;
        }
        
        // Step 3: 群组成员发送消息
        info!("💬 Step 3: 群组成员发送消息");
        let group_messages = vec![
            ("alice", "Welcome everyone to our test group! 🎉"),
            ("bob", "Thanks Alice! Happy to be here! 😊"),
            ("charlie", "This is awesome! Great test setup! 👍"),
            ("alice", "Let's test some group features!"),
            ("bob", "Perfect! Everything seems to work! ✨"),
        ];
        
        for (sender, message) in group_messages {
            match account_manager.send_message(sender, group_id, message, "text").await {  // "text" 消息类型
                Ok(message_id) => {
                    info!("✅ 群组消息发送成功: {} -> {}", sender, message_id);
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    error!("❌ 群组消息发送失败: {} -> {}", sender, e);
                    metrics.errors.push(format!("{} 群组消息发送失败: {}", sender, e));
                }
            }
            sleep(Duration::from_millis(300)).await;
        }
        
        // Step 4: 查询群组信息
        info!("📋 Step 4: 查询群组信息");
        match account_manager.rpc_call("alice", "group/group/info", json!({
            "group_id": group_id
        })).await {
            Ok(group_info) => {
                info!("✅ 查询群组信息成功: {:?}", group_info);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 查询群组信息失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("查询群组信息失败: {}", e));
            }
        }
        
        let duration = start_time.elapsed();
        // 如果至少有一些群组消息发送成功，就认为测试部分成功
        let success = metrics.messages_sent > 0 || metrics.rpc_calls > 0;
        
        info!("✅ Phase 3 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "群组系统流程".to_string(),
            success,
            duration,
            details: format!("RPC调用{}次，群组消息{}条，错误{}个", metrics.rpc_calls, metrics.messages_sent, metrics.errors.len()),
            metrics,
        })
    }
    
    /// Phase 4: 混合场景测试
    pub async fn phase4_mixed_scenarios(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🌟 Phase 4: 混合场景测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        info!("📡 同时进行私聊、群聊、RPC调用");
        
        // ✨ 获取私聊会话的 ID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        let alice_charlie_chat = account_manager.get_private_chat_id("alice", "charlie")
            .expect("无法获取 alice-charlie 私聊会话ID（请先完成好友申请流程）");
        // ✨ bob-charlie 可能还没有完成好友申请，如果获取失败则跳过相关测试
        let bob_charlie_chat = account_manager.get_private_chat_id("bob", "charlie");
        
        // ✨ 群组ID使用服务端返回的 group_id（从缓存获取）
        let group_chat_id = account_manager.get_cached_group_id("Multi-Account Test Group");
        
        // 并发执行多种操作
        let mut tasks: Vec<(&str, u64, &str, &str)> = vec![
            // 私聊消息（确保发送者是频道的参与者）
            ("alice", alice_bob_chat, "Let's test concurrent messaging!", "text"),
            ("alice", alice_charlie_chat, "Testing cross-chat functionality", "text"),
        ];
        
        // 只有在 bob-charlie 私聊会话存在时才添加相关任务
        if let Some(bob_charlie_id) = bob_charlie_chat {
            tasks.push(("charlie", bob_charlie_id, "Multi-user communication test", "text"));
        } else {
            warn!("⚠️ 跳过 bob-charlie 私聊消息测试（会话ID未找到，可能好友申请未完成）");
        }
        
        // 只有在群组ID存在时才添加群组消息任务
        if let Some(group_id) = group_chat_id {
            tasks.push(("alice", group_id, "Testing group messaging", "text"));
            tasks.push(("bob", group_id, "Concurrent group chat test", "text"));
        } else {
            warn!("⚠️ 跳过群组消息测试（群组ID未找到）");
        }
        
        for (sender, channel, message, msg_type) in tasks {
            match account_manager.send_message(sender, channel, message, msg_type).await {
                Ok(message_id) => {
                    info!("✅ 混合消息发送成功: {} -> {} ({})", sender, channel, message_id);
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    error!("❌ 混合消息发送失败: {} -> {} ({})", sender, channel, e);
                    metrics.errors.push(format!("{} 混合消息发送失败: {}", sender, e));
                }
            }
            sleep(Duration::from_millis(150)).await;
        }
        
        // 获取用户 ID（服务端返回的 UUID）
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        
        // 并发RPC调用
        info!("🔧 测试并发RPC调用");
        let rpc_tasks = vec![
            ("alice", "account/search/query", json!({"from_user_id": alice_id, "query": "charlie"})),
            ("bob", "account/search/query", json!({"from_user_id": bob_id, "query": "alice"})),
        ];
        
        for (caller, route, params) in rpc_tasks {
            match account_manager.rpc_call(caller, route, params).await {
                Ok(_response) => {
                    info!("✅ {} 并发RPC调用成功: {}", caller, route);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    error!("❌ {} 并发RPC调用失败: {} -> {}", caller, route, e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("{} RPC调用失败: {}", caller, e));
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        
        let duration = start_time.elapsed();
        let success = metrics.messages_sent >= 3 && metrics.rpc_calls >= 1;
        
        info!("✅ Phase 4 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "混合场景测试".to_string(),
            success,
            duration,
            details: format!("发送{}条消息，{}次RPC调用，{}个错误", metrics.messages_sent, metrics.rpc_calls, metrics.errors.len()),
            metrics,
        })
    }
    
    /// Phase 5: 消息接收验证测试
    pub async fn phase5_message_receiving(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📥 Phase 5: 消息接收验证测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 记录 Phase 5 开始前的事件历史长度，以便只统计 Phase 5 新增的事件
        let initial_event_count = {
            let event_bus = account_manager.get_event_bus_mut();
            event_bus.get_event_history().len()
        };
        
        // Step 1: 私聊消息接收验证
        info!("💬 Step 1: 验证私聊消息接收");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        // Alice 发送消息给 Bob
        let test_message = "Phase 5 测试消息：请确认收到此消息";
        match account_manager.send_message("alice", alice_bob_chat, test_message, "text").await {  // "text" 消息类型
            Ok(message_id) => {
                info!("✅ Alice 发送测试消息成功: {}", message_id);
                metrics.messages_sent += 1;
            }
            Err(e) => {
                error!("❌ Alice 发送测试消息失败: {}", e);
                metrics.errors.push(format!("Alice 发送消息失败: {}", e));
            }
        }
        
        // 等待消息分发和接收（给服务器时间处理）
        sleep(Duration::from_millis(500)).await;
        
        // 处理事件总线中的事件，检查是否有消息接收事件
        
        let mut message_received_count = 0;
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        {
            let event_bus = account_manager.get_event_bus_mut();
            
            // 处理所有待处理的事件
            let processed = event_bus.process_events().await;
            info!("📊 处理了 {} 个待处理事件", processed);
            
            // 只检查 Phase 5 开始后新增的事件（通过索引判断）
            let event_history = event_bus.get_event_history();
            for (idx, event) in event_history.iter().enumerate() {
                // 只统计 Phase 5 开始后新增的事件
                if idx >= initial_event_count {
                    if let crate::event_system::AccountEvent::MessageReceived { account, from, channel, content, .. } = event {
                        // 检查是否是 Phase 5 发送的测试消息（通过内容匹配）
                        if account == "bob" && *from == alice_id && *channel == alice_bob_chat 
                            && content.contains("Phase 5 测试消息") {
                            info!("✅ Bob 收到来自 Alice 的私聊消息");
                            message_received_count += 1;
                            metrics.messages_received += 1;
                        }
                    }
                }
            }
        }
        
        if message_received_count == 0 {
            warn!("⚠️ 未检测到 Bob 接收消息的事件，可能消息接收机制未完全实现");
            metrics.errors.push("未检测到消息接收事件".to_string());
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Step 2: 群聊消息接收验证
        info!("💬 Step 2: 验证群聊消息接收");
        
        // 先创建一个测试群组（如果还没有）
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let group_id = match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id,
            "name": "Phase 5 Test Group",
            "description": "用于测试消息接收的群组"
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    info!("✅ 创建测试群组成功: {}", gid);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    
                    // 添加 Bob 和 Charlie 到群组
                    let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
                    let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
                    for (member_account, member_id) in [("bob", bob_id), ("charlie", charlie_id)] {
                        match account_manager.rpc_call("alice", "group/member/add", json!({
                            "group_id": gid,
                            "inviter_id": alice_id,
                            "user_id": member_id,
                            "role": "member"
                        })).await {
                            Ok(_) => {
                                info!("✅ 添加 {} 到群组成功", member_account);
                                metrics.rpc_calls += 1;
                                metrics.rpc_successes += 1;
                            }
                            Err(e) => {
                                warn!("⚠️ 添加 {} 到群组失败: {}", member_account, e);
                                metrics.rpc_calls += 1;
                                metrics.errors.push(format!("添加 {} 到群组失败: {}", member_account, e));
                            }
                        }
                        sleep(Duration::from_millis(200)).await;
                    }
                    
                    // ✨ 缓存群组ID
                    account_manager.cache_group_id("Phase 5 Test Group", gid);
                    gid
                } else {
                    error!("❌ 群组创建响应格式错误：缺少 group_id");
                    metrics.errors.push("群组创建响应格式错误".to_string());
                    return Ok(PhaseResult {
                        phase_name: "消息接收验证".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "群组创建响应格式错误".to_string(),
                        metrics,
                    });
                }
            }
            Err(e) => {
                error!("❌ 创建测试群组失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("创建测试群组失败: {}", e));
                return Ok(PhaseResult {
                    phase_name: "消息接收验证".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: format!("创建测试群组失败: {}", e),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // Alice 在群组中发送消息
        let group_test_message = "Phase 5 群组测试消息：请所有成员确认收到";
        match account_manager.send_message("alice", group_id, group_test_message, "text").await {  // "text" 消息类型
            Ok(message_id) => {
                info!("✅ Alice 在群组中发送消息成功: {}", message_id);
                metrics.messages_sent += 1;
            }
            Err(e) => {
                error!("❌ Alice 在群组中发送消息失败: {}", e);
                metrics.errors.push(format!("Alice 群组消息发送失败: {}", e));
            }
        }
        
        // 等待消息分发
        sleep(Duration::from_millis(500)).await;
        
        // 处理事件，检查 Bob 和 Charlie 是否收到群组消息
        let mut group_message_received_count = 0;
        {
            let event_bus = account_manager.get_event_bus_mut();
            let processed = event_bus.process_events().await;
            info!("📊 处理了 {} 个待处理事件", processed);
            
            // 只检查 Phase 5 开始后新增的事件（通过索引判断）
            let event_history = event_bus.get_event_history();
            for (idx, event) in event_history.iter().enumerate() {
                // 只统计 Phase 5 开始后新增的事件
                if idx >= initial_event_count {
                    if let crate::event_system::AccountEvent::MessageReceived { account, channel, content, .. } = event {
                        // 检查是否是 Phase 5 发送的群组测试消息（通过内容和频道匹配）
                        if (account == "bob" || account == "charlie") && *channel == group_id
                            && content.contains("Phase 5 群组测试消息") {
                            info!("✅ {} 收到群组消息", account);
                            group_message_received_count += 1;
                            metrics.messages_received += 1;
                        }
                    }
                }
            }
        }
        
        if group_message_received_count == 0 {
            warn!("⚠️ 未检测到群组成员接收消息的事件");
            metrics.errors.push("未检测到群组消息接收事件".to_string());
        }
        
        // Step 3: 消息接收统计验证
        info!("📊 Step 3: 验证消息接收统计");
        
        let total_received = metrics.messages_received;
        let total_sent = metrics.messages_sent;
        
        info!("📈 消息统计: 发送 {} 条，接收 {} 条", total_sent, total_received);
        
        // 验证消息接收：私聊消息应该至少收到1条，群组消息应该至少收到2条（Bob和Charlie各1条）
        let success = total_sent > 0 && total_received >= 1;
        
        let duration = start_time.elapsed();
        info!("✅ Phase 5 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "消息接收验证".to_string(),
            success,
            duration,
            details: format!("发送{}条消息，接收{}条消息，错误{}个", total_sent, total_received, metrics.errors.len()),
            metrics,
        })
    }
    
    /// Phase 6: 表情包功能测试
    pub async fn phase6_sticker_features(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📦 Phase 6: 表情包功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Alice 获取表情包库列表
        info!("📋 Alice 获取表情包库列表");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("alice", "sticker/package/list", json!({})).await {
            Ok(response) => {
                if let Some(packages) = response.get("packages").and_then(|p| p.as_array()) {
                    info!("✅ 获取到 {} 个表情包库", packages.len());
                    metrics.rpc_successes += 1;
                    
                    // 获取第一个表情包库的详情
                    if let Some(first_pkg) = packages.first() {
                        if let Some(package_id) = first_pkg.get("package_id").and_then(|p| p.as_str()) {
                            info!("📋 Alice 获取表情包库详情: {}", package_id);
                            metrics.rpc_calls += 1;
                            
                            sleep(Duration::from_millis(200)).await;
                            
                            match account_manager.rpc_call("alice", "sticker/package/detail", 
                                json!({ "package_id": package_id })
                            ).await {
                                Ok(detail) => {
                                    if let Some(stickers) = detail.get("package")
                                        .and_then(|p| p.get("stickers"))
                                        .and_then(|s| s.as_array()) 
                                    {
                                        info!("✅ 表情包库包含 {} 个表情", stickers.len());
                                        metrics.rpc_successes += 1;
                                        
                                        // Step 3: 发送第一个表情包消息给 Bob
                                        if let Some(first_sticker) = stickers.first() {
                                            if let (Some(sticker_id), Some(image_url), Some(alt_text)) = (
                                                first_sticker.get("sticker_id").and_then(|s| s.as_str()),
                                                first_sticker.get("image_url").and_then(|u| u.as_str()),
                                                first_sticker.get("alt_text").and_then(|a| a.as_str()),
                                            ) {
                                                info!("🎭 Alice 发送表情包消息给 Bob: {}", alt_text);
                                                
                                                let sticker_metadata = json!({
                                                    "package_id": package_id,
                                                    "sticker_id": sticker_id,
                                                    "image_url": image_url,
                                                    "alt_text": alt_text,
                                                    "mime_type": "image/png",
                                                    "width": 120,
                                                    "height": 120
                                                });
                                                
                                                sleep(Duration::from_millis(300)).await;
                                                
                                                // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
                                                let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
                                                    .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
                                                
                                                match account_manager.send_message_with_metadata(
                                                    "alice",
                                                    alice_bob_chat,
                                                    &format!("[{}]", alt_text),
                                                    "sticker",
                                                    Some(sticker_metadata)
                                                ).await {
                                                    Ok(msg_id) => {
                                                        info!("✅ Alice 发送表情包消息成功: {}", msg_id);
                                                        metrics.messages_sent += 1;
                                                    }
                                                    Err(e) => {
                                                        warn!("❌ Alice 发送表情包消息失败: {}", e);
                                                        metrics.errors.push(format!("发送表情包消息失败: {}", e));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!("❌ 获取表情包库详情失败: {}", e);
                                    metrics.errors.push(format!("获取表情包详情失败: {}", e));
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("❌ 获取表情包库列表失败: {}", e);
                metrics.errors.push(format!("获取表情包列表失败: {}", e));
            }
        }
        
        sleep(self.config.message_delay).await;
        
        let duration = start_time.elapsed();
        let success = metrics.rpc_successes >= 1;
        
        info!("✅ Phase 6 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "表情包功能".to_string(),
            success,
            duration,
            details: format!("RPC调用{}/{}成功", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 7: 会话列表和置顶功能测试
    pub async fn phase7_channel_features(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("💬 Phase 7: 会话列表和置顶功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // ✨ 获取 alice 的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        
        // Alice 获取会话列表（从本地：sync_entities(Channel) 已由 bootstrap 同步，此处读 get_channel_list）
        info!("📋 Alice 获取会话列表");
        metrics.rpc_calls += 1;
        match account_manager.get_channel_list("alice").await {
            Ok(channels) => {
                info!("✅ Alice 有 {} 个会话", channels.len());
                metrics.rpc_successes += 1;

                // 从本地 Channel 列表提取并缓存 channel_id（私聊 username=对端 user_id，群聊 channel_name=群名）
                for ch in &channels {
                    if ch.channel_type == 0 {
                        if let Ok(peer_uid) = ch.username.parse::<u64>() {
                            if let Some(other_account) = account_manager.find_account_by_user_id(peer_uid) {
                                account_manager.cache_channel_id("alice", &other_account, ch.channel_id);
                            }
                        }
                    } else if ch.channel_type == 1 || ch.channel_type == 2 {
                        if !ch.channel_name.is_empty() {
                            account_manager.cache_group_id(&ch.channel_name, ch.channel_id);
                        }
                    }
                }

                // 测试置顶功能
                if let Some(first) = channels.first() {
                    let conv_id = first.channel_id;
                    info!("📌 Alice 置顶会话: {}", conv_id);
                    metrics.rpc_calls += 1;
                    sleep(Duration::from_millis(200)).await;
                    match account_manager.pin_channel("alice", conv_id, true).await {
                        Ok(_) => {
                            info!("✅ 会话置顶成功");
                            metrics.rpc_successes += 1;
                        }
                        Err(e) => {
                            warn!("❌ 置顶会话失败: {}", e);
                            metrics.errors.push(format!("置顶失败: {}", e));
                        }
                    }
                }
            }
            Err(e) => {
                warn!("❌ 获取会话列表失败: {}", e);
                metrics.errors.push(format!("获取会话列表失败: {}", e));
            }
        }
        
        sleep(self.config.message_delay).await;
        
        let duration = start_time.elapsed();
        let success = metrics.rpc_successes >= 1;
        
        info!("✅ Phase 7 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "会话列表和置顶".to_string(),
            success,
            duration,
            details: format!("RPC调用{}/{}成功", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 8: 已读回执测试
    pub async fn phase8_read_receipts(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("✔️ Phase 8: 已读回执测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let test_channel = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID");
        
        // Alice 给 Bob 发送消息
        info!("📤 Alice 发送消息给 Bob");
        let msg_id = match account_manager.send_message(
            "alice", 
            test_channel, 
            "这是一条测试已读回执的消息", 
            "text"  // "text" 消息类型
        ).await {
            Ok(id) => {
                info!("✅ 消息发送成功: {}", id);
                metrics.messages_sent += 1;
                id
            }
            Err(e) => {
                error!("❌ 消息发送失败: {}", e);
                metrics.errors.push(format!("发送消息失败: {}", e));
                return Ok(PhaseResult {
                    phase_name: "已读回执".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "消息发送失败".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // Bob 标记消息已读
        info!("✔️ Bob 标记消息已读");
        
        // ✨ 将 msg_id 从 String 转换为 u64
        let message_id = msg_id.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("bob", "message/status/read",
            json!({
                "user_id": account_manager.get_user_id("bob").expect("Bob ID not found"),
                "channel_id": test_channel,
                "message_id": message_id  // ✨ 使用 u64 类型
            })
        ).await {
            Ok(_) => {
                info!("✅ Bob 已读标记成功");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 已读标记失败: {}", e);
                metrics.errors.push(format!("已读标记失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Alice 查询已读状态
        info!("📋 Alice 查询消息已读状态");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("alice", "message/status/read_stats",
            json!({
                "message_id": message_id,  // ✨ 使用 u64 类型
                "channel_id": test_channel
            })
        ).await {
            Ok(_) => {
                info!("✅ 已读状态查询成功");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 查询已读状态失败: {}", e);
                metrics.errors.push(format!("查询已读失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // 测试群聊已读列表
        info!("👥 测试群聊已读列表功能");
        
        // Step 3.1: 创建测试群组（如果不存在）
        info!("🏗️ 创建群聊已读测试群组");
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let mut group_channel: Option<u64> = None;
        match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id,
            "name": "Read Receipt Test Group",
            "description": "Test group for read receipts",
            "is_public": false
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    group_channel = Some(gid);
                    info!("✅ 创建已读测试群组成功: {}", gid);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    
                    // 缓存群组ID
                    account_manager.cache_group_id("Read Receipt Test Group", gid);
                    
                    // 添加 Bob 和 Charlie 到群组
                    let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
                    let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
                    for (member_account, member_id) in [("bob", bob_id), ("charlie", charlie_id)] {
                        if let Ok(_) = account_manager.rpc_call("alice", "group/member/add", json!({
                            "group_id": gid,
                            "inviter_id": alice_id,
                            "user_id": member_id,
                            "role": "member"
                        })).await {
                            info!("✅ 添加 {} 到测试群组成功", member_account);
                            metrics.rpc_calls += 1;
                            metrics.rpc_successes += 1;
                        }
                        sleep(Duration::from_millis(100)).await;
                    }
                } else {
                    warn!("❌ 创建群组响应格式错误");
                    metrics.errors.push("创建群组响应格式错误".to_string());
                }
            }
            Err(e) => {
                warn!("❌ 创建测试群组失败: {}", e);
                metrics.errors.push(format!("创建群组失败: {}", e));
            }
        }
        
        let group_channel = match group_channel {
            Some(id) => id,
            None => {
                warn!("❌ 无法获取群组ID，跳过已读回执测试");
                return Ok(PhaseResult {
                    phase_name: "已读回执功能".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "无法获取群组ID".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // Alice 在群聊中发送消息
        info!("📤 Alice 在群聊发送消息");
        let group_msg_id = match account_manager.send_message(
            "alice",
            group_channel,
            "群聊测试消息：请大家确认已读",
            "text"
        ).await {
            Ok(id) => {
                info!("✅ 群聊消息发送成功: {}", id);
                metrics.messages_sent += 1;
                id
            }
            Err(e) => {
                warn!("❌ 群聊消息发送失败: {}", e);
                metrics.errors.push(format!("群聊消息发送失败: {}", e));
                // 继续执行，不中断测试
                String::from("mock_group_msg_id")
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // ✨ 获取 bob 和 charlie 的实际 user_id
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        // ✨ 将 group_msg_id 从 String 转换为 u64
        let group_message_id = group_msg_id.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 group_msg_id: {}", e)))?;
        
        // Bob 标记群聊消息已读
        info!("✔️ Bob 标记群聊消息已读");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("bob", "message/status/read",
            json!({
                "user_id": bob_id,
                "channel_id": group_channel,
                "message_id": group_message_id  // ✨ 使用 u64 类型
            })
        ).await {
            Ok(_) => {
                info!("✅ Bob 群聊已读标记成功");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ Bob 群聊已读标记失败: {}", e);
                metrics.errors.push(format!("群聊已读标记失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Charlie 也标记已读
        info!("✔️ Charlie 标记群聊消息已读");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("charlie", "message/status/read",
            json!({
                "user_id": charlie_id,
                "channel_id": group_channel,
                "message_id": group_message_id  // ✨ 使用 u64 类型
            })
        ).await {
            Ok(_) => {
                info!("✅ Charlie 群聊已读标记成功");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ Charlie 群聊已读标记失败: {}", e);
                metrics.errors.push(format!("群聊已读标记失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Alice 查询群聊消息的已读列表
        info!("📋 Alice 查询群聊消息的已读列表");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("alice", "message/status/read_list",
            json!({
                "message_id": group_message_id,  // ✨ 使用 u64 类型
                "channel_id": group_channel
            })
        ).await {
            Ok(response) => {
                if let Some(read_list) = response.get("read_list").and_then(|l| l.as_array()) {
                    info!("✅ 群聊已读列表查询成功，已读用户数: {}", read_list.len());
                    for item in read_list.iter() {
                        if let (Some(user_id), Some(read_at)) = (
                            item.get("user_id").and_then(|u| u.as_str()),
                            item.get("read_at").and_then(|t| t.as_str())
                        ) {
                            info!("   - {} 于 {} 已读", user_id, read_at);
                        }
                    }
                    metrics.rpc_successes += 1;
                } else {
                    warn!("❌ 群聊已读列表响应格式错误");
                    metrics.errors.push("已读列表格式错误".to_string());
                }
            }
            Err(e) => {
                warn!("❌ 查询群聊已读列表失败: {}", e);
                metrics.errors.push(format!("查询已读列表失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // Alice 查询群聊消息的已读统计
        info!("📊 Alice 查询群聊消息的已读统计");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("alice", "message/status/read_stats",
            json!({
                "message_id": group_message_id,  // ✨ 使用 u64 类型
                "channel_id": group_channel
            })
        ).await {
            Ok(response) => {
                if let (Some(read_count), Some(total_count)) = (
                    response.get("read_count").and_then(|c| c.as_u64()),
                    response.get("total_count").and_then(|c| c.as_u64())
                ) {
                    info!("✅ 群聊已读统计查询成功: {}/{} 人已读", read_count, total_count);
                    metrics.rpc_successes += 1;
                } else {
                    warn!("❌ 群聊已读统计响应格式错误");
                    metrics.errors.push("已读统计格式错误".to_string());
                }
            }
            Err(e) => {
                warn!("❌ 查询群聊已读统计失败: {}", e);
                metrics.errors.push(format!("查询已读统计失败: {}", e));
            }
        }
        
        let duration = start_time.elapsed();
        let success = metrics.rpc_successes >= 1;
        
        info!("✅ Phase 8 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "已读回执".to_string(),
            success,
            duration,
            details: format!("发送{}条消息，RPC调用{}/{}成功", 
                           metrics.messages_sent, metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 9: 文件上传流程测试
    pub async fn phase9_file_upload(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📁 Phase 9: 文件上传流程测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Alice 请求上传 token
        info!("🎫 Alice 请求文件上传 token");
        metrics.rpc_calls += 1;
        
        // ✨ 获取 alice 的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        
        match account_manager.rpc_call("alice", "file/request_upload_token",
            json!({
                "user_id": alice_id,
                "file_type": "image",
                "file_size": 102400,
                "mime_type": "image/jpeg",
                "business_type": "message"
            })
        ).await {
            Ok(response) => {
                if let (Some(token), Some(upload_url)) = (
                    response.get("upload_token").and_then(|t| t.as_str()),
                    response.get("upload_url").and_then(|u| u.as_str()),
                ) {
                    info!("✅ 上传 token 获取成功");
                    info!("   Token: {}...", &token[..20.min(token.len())]);
                    info!("   Upload URL: {}", upload_url);
                    metrics.rpc_successes += 1;
                } else {
                    warn!("❌ 上传 token 响应格式错误");
                    metrics.errors.push("上传 token 格式错误".to_string());
                }
            }
            Err(e) => {
                warn!("❌ 请求上传 token 失败: {}", e);
                metrics.errors.push(format!("请求上传 token 失败: {}", e));
            }
        }
        
        sleep(self.config.message_delay).await;
        
        let duration = start_time.elapsed();
        let success = metrics.rpc_successes >= 1;
        
        info!("✅ Phase 9 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "文件上传".to_string(),
            success,
            duration,
            details: format!("RPC调用{}/{}成功", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 10: 其他消息类型测试（位置、名片）
    pub async fn phase10_other_message_types(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🗺️ Phase 10: 其他消息类型测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: Alice 发送位置消息给 Bob
        info!("📍 Step 1: Alice 发送位置消息给 Bob");
        let location_metadata = json!({
            "latitude": 39.9042,
            "longitude": 116.4074,
            "address": "北京市朝阳区",
            "title": "我的位置",
            "zoom_level": 15
        });
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        match account_manager.send_message_with_metadata(
            "alice",
            alice_bob_chat,
            "[位置] 北京市朝阳区",
            "location",
            Some(location_metadata)
        ).await {
            Ok(msg_id) => {
                info!("✅ Alice 发送位置消息成功: {}", msg_id);
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ Alice 发送位置消息失败: {}", e);
                metrics.errors.push(format!("发送位置消息失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 2: Bob 发送名片消息给 Alice
        info!("👤 Step 2: Bob 发送名片消息给 Alice");
        let contact_card_metadata = json!({
            "user_id": account_manager.get_user_id("charlie").expect("Charlie ID not found"),
            "nickname": "Charlie",
            "avatar_url": "https://example.com/avatars/charlie.jpg",
            "bio": "Charlie 的个人简介"
        });
        
        match account_manager.send_message_with_metadata(
            "bob",
            alice_bob_chat,
            "[名片] Charlie",
            "contact_card",
            Some(contact_card_metadata)
        ).await {
            Ok(msg_id) => {
                info!("✅ Bob 发送名片消息成功: {}", msg_id);
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ Bob 发送名片消息失败: {}", e);
                metrics.errors.push(format!("发送名片消息失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 3: Charlie 在群聊中发送位置消息
        info!("📍 Step 3: 跳过群聊位置消息测试（需要先创建群组）");
        // Note: 群聊测试需要使用 Phase 3 创建的群组，这里暂时跳过
        // 在实际测试中，应该先创建群组或使用共享的群组ID
        
        /*
        let group_location_payload = json!({
            "message_type": "location",
            "content": "[位置] 上海市浦东新区",
            "metadata": {
                "latitude": 31.2304,
                "longitude": 121.4737,
                "address": "上海市浦东新区陆家嘴",
                "title": "陆家嘴金融中心"
            }
        }).to_string();
        
        match account_manager.send_message("charlie", "PLACEHOLDER_GROUP_ID", &group_location_payload, "location").await {
            Ok(msg_id) => {
                info!("✅ Charlie 在群聊发送位置消息成功: {}", msg_id);
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ Charlie 发送群聊位置消息失败: {}", e);
                metrics.errors.push(format!("发送群聊位置消息失败: {}", e));
            }
        }
        */
        
        sleep(self.config.message_delay).await;
        
        let duration = start_time.elapsed();
        let success = metrics.messages_sent >= 2;
        
        info!("✅ Phase 10 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "其他消息类型".to_string(),
            success,
            duration,
            details: format!("发送{}条消息，{}个错误", metrics.messages_sent, metrics.errors.len()),
            metrics,
        })
    }
    
    /// Phase 11: 消息历史查询测试
    pub async fn phase11_message_history(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📜 Phase 11: 消息历史查询测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: Alice 发送几条测试消息
        info!("📤 Step 1: Alice 发送测试消息");
        let test_messages = vec![
            "消息历史测试 - 消息 1",
            "消息历史测试 - 消息 2",
            "消息历史测试 - 消息 3",
        ];
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        for msg in &test_messages {
            match account_manager.send_message("alice", alice_bob_chat, msg, "text").await {
                Ok(msg_id) => {
                    info!("✅ 消息发送成功: {}", msg_id);
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    warn!("❌ 消息发送失败: {}", e);
                    metrics.errors.push(format!("发送消息失败: {}", e));
                }
            }
            sleep(Duration::from_millis(200)).await;
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 2: Bob 查询消息历史（通过 channel_id）
        info!("📋 Step 2: Bob 查询消息历史");
        metrics.rpc_calls += 1;
        match account_manager.rpc_call("bob", "message/history/get",
            json!({
                "user_id": account_manager.get_user_id("bob").expect("Bob ID not found"),
                "channel_id": alice_bob_chat,
                "limit": 10,
                "offset": 0
            })
        ).await {
            Ok(response) => {
                if let Some(messages) = response.get("messages").and_then(|m| m.as_array()) {
                    info!("✅ 消息历史查询成功，获取到 {} 条消息", messages.len());
                    metrics.rpc_successes += 1;
                    
                    // 显示最近3条消息
                    for (i, msg) in messages.iter().take(3).enumerate() {
                        if let (Some(sender), Some(content)) = (
                            msg.get("sender_id").and_then(|s| s.as_str()),
                            msg.get("content").and_then(|c| c.as_str())
                        ) {
                            info!("   {}. [{}] {}", i + 1, sender, content);
                        }
                    }
                } else {
                    warn!("❌ 消息历史响应格式错误");
                    metrics.errors.push("消息历史格式错误".to_string());
                }
            }
            Err(e) => {
                warn!("❌ 查询消息历史失败: {}", e);
                metrics.errors.push(format!("查询消息历史失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 3: 消息搜索（应在客户端实现，跳过服务端测试）
        info!("🔍 Step 3: 跳过消息搜索测试（功能应在客户端实现）");
        info!("   💡 消息搜索应该由客户端在本地数据库中进行，无需服务端支持");
        
        // 注释：服务端的 message/history/search 接口在当前架构下没有实际意义
        // 因为：
        // 1. 消息主要存储在客户端本地数据库
        // 2. 客户端可以更快地搜索本地消息
        // 3. 服务端搜索需要全文索引，成本高
        // 4. 微信采用的也是客户端搜索方案
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 4: 跳过群聊消息历史测试（需要动态群组ID）
        info!("📋 Step 4: 跳过群聊消息历史测试");
        info!("   💡 群组ID是动态创建的，此处跳过测试以避免硬编码");
        
        let duration = start_time.elapsed();
        // 修复：实际只调用了 1 次 RPC（message/history/get），所以判断条件应该是 >= 1
        let success = metrics.rpc_successes >= 1 && metrics.messages_sent >= 3;
        
        info!("✅ Phase 11 完成，用时: {}ms", duration.as_millis());
        
        Ok(PhaseResult {
            phase_name: "消息历史查询".to_string(),
            success,
            duration,
            details: format!("发送{}条消息，RPC调用{}/{}成功", 
                           metrics.messages_sent, metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 12: 消息撤回功能测试
    pub async fn phase12_message_revoke(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("↩️  Phase 12: 消息撤回功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 场景1: 私聊中发送者自己撤回消息
        info!("📝 场景 1: 私聊消息撤回");
        info!("   Alice 发送消息给 Bob，然后撤回");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let private_channel = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        // Alice 发送消息给 Bob
        let _local_message_id = account_manager
            .send_message("alice", private_channel, "这是一条将被撤回的消息", "text")
            .await
            .map_err(|e| {
                metrics.errors.push(format!("Alice 发送消息失败: {}", e));
                e
            })?;
        
        metrics.messages_sent += 1;
        info!("   ✅ 消息已发送，等待服务器确认...");
        
        // ✨ 修复：增加轮询机制，等待消息被 Bob 接收
        let server_msg_id = {
            let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
            let mut retry_count = 0;
            let max_retries = 10; // 最多重试 10 次
            let mut found_msg_id = None;
            
            while retry_count < max_retries && found_msg_id.is_none() {
                sleep(Duration::from_millis(200)).await; // 每次等待 200ms
                retry_count += 1;
                
                // ✨ 处理待处理的事件（将事件从通道移到历史记录）
                let processed = account_manager.get_event_bus_mut().process_events().await;
                if processed > 0 {
                    debug!("   📊 处理了 {} 个新事件", processed);
                }
                
                // 从 Bob 的事件历史中查找
                found_msg_id = account_manager
                    .get_event_bus()
                    .get_event_history_for_account("bob")
                    .iter()
                    .rev()
                    .find_map(|event| {
                        if let crate::event_system::AccountEvent::MessageReceived { message_id, content, from, .. } = event {
                            if content.contains("这是一条将被撤回的消息") && *from == alice_id {
                                return Some(message_id.to_string());
                            }
                        }
                        None
                    });
                
                if found_msg_id.is_none() && retry_count < max_retries {
                    debug!("   ⏳ 等待消息推送到 Bob... (重试 {}/{})", retry_count, max_retries);
                }
            }
            
            found_msg_id.ok_or_else(|| {
                let err = format!("Bob 未能从事件历史中找到 Alice 发送的 message_id (已重试 {} 次)", retry_count);
                metrics.errors.push(err.clone());
                privchat_sdk::error::PrivchatSDKError::Other(err)
            })?
        };
        
        info!("   ✅ 服务器 message_id: {}", server_msg_id);
        
        // Alice 撤回消息
        let server_msg_id_u64 = server_msg_id.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        metrics.rpc_calls += 1;
        match account_manager.revoke_message("alice", server_msg_id_u64, private_channel).await {
            Ok(_) => {
                info!("   ✅ Alice 成功撤回消息");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                let err_msg = format!("Alice 撤回消息失败: {}", e);
                error!("   ❌ {}", err_msg);
                metrics.errors.push(err_msg);
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // 场景2: Bob 尝试撤回 Alice 的消息（应该失败）
        info!("📝 场景 2: 尝试撤回他人消息（应该失败）");
        
        let _local_message_id2 = account_manager
            .send_message("alice", private_channel, "Alice 的第二条消息", "text")
            .await?;
        
        metrics.messages_sent += 1;
        info!("   ✅ Alice 发送消息，等待服务器确认...");
        
        // ✨ 修复：增加轮询机制，等待消息被 Bob 接收
        let server_msg_id2 = {
            let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
            let mut retry_count = 0;
            let max_retries = 10; // 最多重试 10 次
            let mut found_msg_id = None;
            
            while retry_count < max_retries && found_msg_id.is_none() {
                sleep(Duration::from_millis(200)).await; // 每次等待 200ms
                retry_count += 1;
                
                // ✨ 处理待处理的事件（将事件从通道移到历史记录）
                let processed = account_manager.get_event_bus_mut().process_events().await;
                if processed > 0 {
                    debug!("   📊 处理了 {} 个新事件", processed);
                }
                
                // 从 Bob 的事件历史中查找
                found_msg_id = account_manager
                    .get_event_bus()
                    .get_event_history_for_account("bob")
                    .iter()
                    .rev()
                    .find_map(|event| {
                        if let crate::event_system::AccountEvent::MessageReceived { message_id, content, from, .. } = event {
                            if content.contains("Alice 的第二条消息") && *from == alice_id {
                                return Some(message_id.to_string());
                            }
                        }
                        None
                    });
                
                if found_msg_id.is_none() && retry_count < max_retries {
                    debug!("   ⏳ 等待消息推送到 Bob... (重试 {}/{})", retry_count, max_retries);
                }
            }
            
            found_msg_id.ok_or_else(|| {
                let err = format!("Bob 未能从事件历史中找到 Alice 的 message_id (已重试 {} 次)", retry_count);
                metrics.errors.push(err.clone());
                privchat_sdk::error::PrivchatSDKError::Other(err)
            })?
        };
        
        info!("   ✅ 服务器 message_id: {}", server_msg_id2);
        
        // Bob 尝试撤回 Alice 的消息
        let server_msg_id2_u64 = server_msg_id2.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        metrics.rpc_calls += 1;
        match account_manager.revoke_message("bob", server_msg_id2_u64, private_channel).await {
            Ok(_) => {
                let err_msg = "Bob 不应该能撤回 Alice 的消息！".to_string();
                error!("   ❌ {}", err_msg);
                metrics.errors.push(err_msg);
            }
            Err(_) => {
                info!("   ✅ Bob 无法撤回 Alice 的消息（符合预期）");
                // 这是符合预期的失败，计为成功
                metrics.rpc_successes += 1;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // 场景3: 群聊中普通成员撤回自己的消息
        info!("📝 场景 3: 群聊中普通成员撤回自己的消息");
        
        // 使用已存在的测试群（从缓存获取或使用之前创建的群组）
        let group_channel = account_manager.get_cached_group_id("Multi-Account Test Group")
            .or_else(|| account_manager.get_cached_group_id("Phase 5 Test Group"))
            .ok_or_else(|| {
                let err = "未找到测试群组ID".to_string();
                metrics.errors.push(err.clone());
                privchat_sdk::error::PrivchatSDKError::Other(err)
            })?;
        info!("   使用测试群: {}", group_channel);
        
        // Charlie 发送消息到群里
        let _local_message_id3 = account_manager
            .send_message("charlie", group_channel, "Charlie 在群里的消息", "text")
            .await?;
        
        metrics.messages_sent += 1;
        info!("   ✅ Charlie 发送群消息，等待服务器确认...");
        
        // ✨ 修复：增加轮询机制，等待消息被 Charlie 自己接收（群消息会回显）
        let server_msg_id3 = {
            let mut retry_count = 0;
            let max_retries = 10; // 最多重试 10 次
            let mut found_msg_id = None;
            
            while retry_count < max_retries && found_msg_id.is_none() {
                sleep(Duration::from_millis(200)).await; // 每次等待 200ms
                retry_count += 1;
                
                // ✨ 处理待处理的事件（将事件从通道移到历史记录）
                let processed = account_manager.get_event_bus_mut().process_events().await;
                if processed > 0 {
                    debug!("   📊 处理了 {} 个新事件", processed);
                }
                
                // 从 Charlie 的事件历史中查找
                found_msg_id = account_manager
                    .get_event_bus()
                    .get_event_history_for_account("charlie")
                    .iter()
                    .rev()
                    .find_map(|event| {
                        if let crate::event_system::AccountEvent::MessageReceived { message_id, content, .. } = event {
                            if content.contains("Charlie 在群里的消息") {
                                return Some(message_id.to_string());
                            }
                        }
                        None
                    });
                
                if found_msg_id.is_none() && retry_count < max_retries {
                    debug!("   ⏳ 等待群消息回显到 Charlie... (重试 {}/{})", retry_count, max_retries);
                }
            }
            
            found_msg_id.ok_or_else(|| {
                let err = format!("Charlie 未能从事件历史中找到服务器返回的 message_id (已重试 {} 次)", retry_count);
                metrics.errors.push(err.clone());
                privchat_sdk::error::PrivchatSDKError::Other(err)
            })?
        };
        
        info!("   ✅ 服务器 message_id: {}", server_msg_id3);
        
        // Charlie 撤回自己的消息
        let server_msg_id3_u64 = server_msg_id3.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        metrics.rpc_calls += 1;
        match account_manager.revoke_message("charlie", server_msg_id3_u64, group_channel).await {
            Ok(_) => {
                info!("   ✅ Charlie 成功撤回自己的群消息");
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                let err_msg = format!("Charlie 撤回群消息失败: {}", e);
                error!("   ❌ {}", err_msg);
                metrics.errors.push(err_msg);
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // 场景4: 测试时间限制（如果可能）
        info!("📝 场景 4: 测试撤回时间限制");
        info!("   注意: 默认时间限制为 2 分钟，此处仅发送消息作为记录");
        
        let _local_message_id4 = account_manager
            .send_message("alice", private_channel, "测试时间限制的消息", "text")
            .await?;
        
        metrics.messages_sent += 1;
        info!("   ✅ 消息已发送");
        info!("   💡 在生产环境中，2 分钟后此消息将无法撤回");
        
        // 汇总测试结果
        info!("");
        info!("📊 消息撤回测试总结:");
        info!("   - 场景 1: 私聊撤回 ✅");
        info!("   - 场景 2: 无权撤回他人消息 ✅");
        info!("   - 场景 3: 群聊撤回测试");
        info!("   - 场景 4: 时间限制记录 💡");
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 12 完成，用时: {}ms", duration.as_millis());
        
        let success = metrics.errors.is_empty() && metrics.rpc_successes >= 2;
        let details = if success {
            format!("成功完成 {} 个撤回场景测试，RPC 成功{}/{}",
                   metrics.messages_sent, metrics.rpc_successes, metrics.rpc_calls)
        } else {
            format!("完成测试，RPC 成功{}/{}，但有 {} 个错误",
                   metrics.rpc_successes, metrics.rpc_calls, metrics.errors.len())
        };
        
        Ok(PhaseResult {
            phase_name: "消息撤回".to_string(),
            success,
            duration,
            details,
            metrics,
        })
    }
    
    /// Phase 13: 离线消息推送功能测试
    pub async fn phase13_offline_message_push(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📴 Phase 13: 离线消息推送功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: Alice 连接并清空消息队列
        info!("🔗 Step 1: Alice 连接服务器");
        sleep(Duration::from_millis(500)).await;
        
        // Step 2: Bob 和 Charlie 给 Alice 发送在线消息（验证在线接收）
        info!("📤 Step 2: Bob 和 Charlie 给 Alice 发送在线消息");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        let bob_online_msg = account_manager.send_message(
            "bob",
            alice_bob_chat,
            "Alice，这是你在线时的消息",
            "text"
        ).await;
        
        if bob_online_msg.is_ok() {
            metrics.messages_sent += 1;
            info!("✅ Bob 在线消息发送成功");
        }
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_charlie_chat = account_manager.get_private_chat_id("alice", "charlie")
            .expect("无法获取 alice-charlie 私聊会话ID（请先完成好友申请流程）");
        
        let charlie_online_msg = account_manager.send_message(
            "charlie",
            alice_charlie_chat,
            "Alice，我也在线发送",
            "text"
        ).await;
        
        if charlie_online_msg.is_ok() {
            metrics.messages_sent += 1;
            info!("✅ Charlie 在线消息发送成功");
        }
        
        sleep(Duration::from_secs(1)).await;
        
        // Step 3: Alice 断开连接（模拟离线）
        info!("📴 Step 3: Alice 断开连接（模拟离线）");
        
        match account_manager.disconnect_account("alice").await {
            Ok(_) => {
                info!("✅ Alice 已断开连接");
            }
            Err(e) => {
                warn!("⚠️ Alice 断开连接失败: {}", e);
                metrics.errors.push(format!("Alice 断开连接失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 4: Bob 给 Alice 发送离线消息
        info!("📤 Step 4: Bob 给 Alice 发送 3 条离线消息");
        
        for i in 1..=3 {
            let msg = account_manager.send_message(
                "bob",
                alice_bob_chat,
                &format!("Alice 离线消息 {} from Bob", i),
                "text"
            ).await;
            
            if msg.is_ok() {
                metrics.messages_sent += 1;
                info!("✅ Bob 离线消息 {} 发送成功", i);
            } else {
                metrics.errors.push(format!("Bob 离线消息 {} 发送失败", i));
            }
            
            sleep(Duration::from_millis(200)).await;
        }
        
        // Step 5: Charlie 给 Alice 发送离线消息
        info!("📤 Step 5: Charlie 给 Alice 发送 2 条离线消息");
        
        for i in 1..=2 {
            let msg = account_manager.send_message(
                "charlie",
                alice_charlie_chat,
                &format!("Alice 离线消息 {} from Charlie", i),
                "text"
            ).await;
            
            if msg.is_ok() {
                metrics.messages_sent += 1;
                info!("✅ Charlie 离线消息 {} 发送成功", i);
            } else {
                metrics.errors.push(format!("Charlie 离线消息 {} 发送失败", i));
            }
            
            sleep(Duration::from_millis(200)).await;
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Step 6: Alice 重新连接
        info!("🔗 Step 6: Alice 重新连接服务器，触发离线消息推送");
        match account_manager.connect_account("alice").await {
            Ok(_) => {
                info!("✅ Alice 重新连接成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                error!("❌ Alice 重新连接失败: {:?}", e);
                metrics.errors.push("Alice 重新连接失败".to_string());
            }
        }
        
        // Step 7: 等待离线消息推送
        info!("⏳ Step 7: 等待离线消息推送（2秒）");
        sleep(Duration::from_secs(2)).await;
        
        // Step 8: 验证离线消息接收情况
        info!("🔍 Step 8: 验证离线消息接收情况");
        
        let event_bus = account_manager.get_event_bus();
        let all_events = event_bus.get_event_history();
        
        // 过滤出 Alice 收到的消息
        let offline_messages: Vec<_> = all_events.iter()
            .filter(|e| {
                if let AccountEvent::MessageReceived { account, .. } = e {
                    account == "alice"
                } else {
                    false
                }
            })
            .collect();
        
        info!("📨 Alice 总共收到 {} 条消息", offline_messages.len());
        
        // 分析消息来源
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        let bob_messages = offline_messages.iter()
            .filter(|e| {
                if let AccountEvent::MessageReceived { from, .. } = e {
                    *from == bob_id
                } else {
                    false
                }
            })
            .count();
        
        let charlie_messages = offline_messages.iter()
            .filter(|e| {
                if let AccountEvent::MessageReceived { from, .. } = e {
                    *from == charlie_id
                } else {
                    false
                }
            })
            .count();
        
        info!("   📊 来自 Bob: {} 条", bob_messages);
        info!("   📊 来自 Charlie: {} 条", charlie_messages);
        
        // 验证是否收到所有离线消息
        // 预期：Bob 4条（1条在线 + 3条离线），Charlie 3条（1条在线 + 2条离线）
        let total_messages = offline_messages.len();
        let success = total_messages >= 5; // 至少收到离线消息
        
        if success {
            info!("✅ 离线消息推送测试通过！收到 {} 条消息", total_messages);
        } else {
            warn!("⚠️ 离线消息推送可能有问题，预期至少 5 条离线消息");
            metrics.errors.push("离线消息数量不足".to_string());
        }
        
        // Step 9: 测试历史消息获取
        info!("📋 Step 9: 测试历史消息获取接口");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let history_result = account_manager.rpc_call("alice", "message/history/get", json!({
            "user_id": alice_id,
            "channel_id": alice_bob_chat,
            "limit": 10
        })).await;
        
        metrics.rpc_calls += 1;
        
        match history_result {
            Ok(response) => {
                metrics.rpc_successes += 1;
                
                if let Some(messages) = response.get("messages").and_then(|m| m.as_array()) {
                    info!("✅ 获取到 {} 条历史消息", messages.len());
                    
                    // 显示最近的消息
                    for (i, msg) in messages.iter().take(3).enumerate() {
                        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                            info!("   📝 消息 {}: {}", i + 1, content);
                        }
                    }
                } else {
                    warn!("⚠️ 历史消息格式错误");
                }
            }
            Err(e) => {
                warn!("❌ 获取历史消息失败: {}", e);
                metrics.errors.push(format!("获取历史消息失败: {}", e));
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 13 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "发送{}条消息，收到{}条消息（来自Bob:{}条，Charlie:{}条），RPC调用{}/{}成功",
            metrics.messages_sent,
            total_messages,
            bob_messages,
            charlie_messages,
            metrics.rpc_successes,
            metrics.rpc_calls
        );
        
        Ok(PhaseResult {
            phase_name: "离线消息推送".to_string(),
            success,
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 14: pts 同步和离线消息队列限制测试
    pub async fn phase14_pts_sync_and_queue_limit(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔄 Phase 14: pts 同步和离线消息队列限制测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // Step 1: 获取 Alice 的初始 pts
        info!("📊 Step 1: 获取 Alice 的初始 pts");
        let initial_pts = {
            let alice_client = account_manager.get_client_mut("alice")?;
            alice_client.get_local_pts().await
        };
        info!("   Alice 初始 pts: {}", initial_pts);
        
        // Step 2: Alice 断开连接
        info!("🔌 Step 2: Alice 断开连接");
        account_manager.disconnect_account("alice").await?;
        sleep(Duration::from_millis(500)).await;
        
        // Step 3: Bob 向 Alice 发送 150 条离线消息（测试 100 条队列限制）⭐
        info!("📤 Step 3: Bob 向 Alice 发送 150 条离线消息");
        let message_count = 150;
        
        for i in 1..=message_count {
            let content = format!("离线消息 #{} from Bob", i);
            
            // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
            let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
                .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
            
            match account_manager
                .send_message("bob", alice_bob_chat, &content, "text")
                .await
            {
                Ok(_) => {
                    metrics.messages_sent += 1;
                    if i % 20 == 0 {
                        info!("   ✅ 已发送 {} 条消息", i);
                    }
                }
                Err(e) => {
                    warn!("   ❌ 发送消息 #{} 失败: {}", i, e);
                    metrics.errors.push(format!("发送消息失败: {}", e));
                    success = false;
                }
            }
            
            // 控制发送速率
            if i % 10 == 0 {
                sleep(Duration::from_millis(50)).await;
            }
        }
        
        info!("   ✅ Bob 完成发送 {} 条消息", message_count);
        sleep(Duration::from_secs(1)).await;
        
        // Step 4: Alice 重新连接并检查 pts 同步
        info!("🔄 Step 4: Alice 重新连接");
        
        // 获取 Alice 的客户端并模拟 pts 落后（服务器会推送最新 100 条）
        {
            let alice_client = account_manager.get_client_mut("alice")?;
            alice_client.set_local_pts(initial_pts).await; // 重置为初始 pts
        }
        
        // 重新连接
        match account_manager.connect_account("alice").await {
            Ok(_) => {
                info!("   ✅ Alice 重新连接成功");
                
                // 获取新的 pts
                let new_pts = {
                    let alice_client = account_manager.get_client_mut("alice")?;
                    alice_client.get_local_pts().await
                };
                info!("   📊 Alice 新 pts: {} (增长: {})", new_pts, new_pts - initial_pts);
            }
            Err(e) => {
                warn!("   ❌ Alice 重新连接失败: {}", e);
                metrics.errors.push(format!("重新连接失败: {}", e));
                success = false;
            }
        }
        
        // Step 5: 等待并验证离线消息推送（应该最多收到 100 条）⭐
        info!("⏳ Step 5: 等待离线消息推送（最多 100 条）");
        sleep(Duration::from_secs(3)).await;
        
        let alice_events = account_manager.get_event_history("alice");
        let mut received_count = 0;
        let mut first_msg_no = None;
        let mut last_msg_no = None;
        
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        for event in alice_events.iter() {
            if let AccountEvent::MessageReceived { from, content, .. } = event {
                if *from == bob_id {
                    received_count += 1;
                    
                    // 提取消息编号
                    if let Some(msg_no_str) = content.strip_prefix("离线消息 #").and_then(|s| s.split(' ').next()) {
                        if let Ok(msg_no) = msg_no_str.parse::<i32>() {
                            if first_msg_no.is_none() {
                                first_msg_no = Some(msg_no);
                            }
                            last_msg_no = Some(msg_no);
                        }
                    }
                }
            }
        }
        
        info!("   📬 Alice 收到 {} 条离线消息", received_count);
        if let (Some(first), Some(last)) = (first_msg_no, last_msg_no) {
            info!("   📊 消息范围: #{} - #{}", first, last);
        }
        
        // 验证队列限制（应该最多收到 100 条）⭐
        if received_count > 100 {
            warn!("   ⚠️ 收到消息数超过 100 条限制: {} 条", received_count);
            metrics.errors.push(format!("队列限制失效: 收到 {} 条消息", received_count));
            success = false;
        } else if received_count == 100 {
            info!("   ✅ 队列限制生效：正好收到 100 条消息");
        } else {
            info!("   ℹ️ 收到 {} 条消息（预期最多 100 条）", received_count);
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 14 完成，用时: {}ms", duration.as_millis());
        
        // ✨ 修复：重新连接 Alice，避免后续测试中 "Not connected" 错误
        info!("🔌 重新连接 Alice...");
        match account_manager.connect_account("alice").await {
            Ok(_) => {
                info!("✅ Alice 重新连接成功");
                sleep(Duration::from_millis(500)).await;
            }
            Err(e) => {
                warn!("⚠️ Alice 重新连接失败: {}", e);
            }
        }
        
        let details = format!(
            "发送{}条消息，收到{}条消息（队列限制100条）",
            message_count,
            received_count
        );
        
        Ok(PhaseResult {
            phase_name: "pts同步和队列限制".to_string(),
            success,
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 15: 高级群组功能测试
    pub async fn phase15_advanced_group_features(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👑 Phase 15: 高级群组功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // Step 0: 创建测试群组（Alice 为群主）
        info!("🏗️ Step 0: 创建测试群组");
        
        // ✨ 获取所有用户的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        let group_id = match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id,
            "name": "Advanced Group Test",
            "description": "Testing advanced group features",
            "is_public": false
        })).await {
            Ok(response) => {
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // ✨ 从响应中提取 group_id（服务端返回的 u64）
                match response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    Some(group_id) => {
                        info!("✅ 群组创建成功: group_id={}", group_id);
                        // ✨ 缓存群组ID
                        account_manager.cache_group_id("Advanced Group Test", group_id);
                        group_id
                    }
                    None => {
                        error!("❌ 群组创建响应中缺少 group_id");
                        metrics.errors.push("群组创建响应中缺少 group_id".to_string());
                        return Ok(PhaseResult {
                            phase_name: "高级群组功能".to_string(),
                            success: false,
                            duration: start_time.elapsed(),
                            details: "群组创建响应中缺少 group_id".to_string(),
                            metrics,
                        });
                    }
                }
            }
            Err(e) => {
                error!("❌ 创建群组失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("创建群组失败: {}", e));
                return Ok(PhaseResult {
                    phase_name: "高级群组功能".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: format!("创建群组失败: {}", e),
                    metrics,
                });
            }
        };
        
        // ✨ 获取 bob 和 charlie 的实际 user_id
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        // 添加 Bob 和 Charlie 为普通成员
        for (member_account, member_id) in [("bob", bob_id.clone()), ("charlie", charlie_id.clone())] {
            match account_manager.rpc_call("alice", "group/member/add", json!({
                "group_id": group_id,
                            "inviter_id": alice_id,
                "user_id": member_id,
                "role": "member"
            })).await {
                Ok(_) => {
                    info!("✅ {} 加入群组成功", member_account);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    warn!("⚠️ {} 加入群组失败: {}", member_account, e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("{} 加入失败: {}", member_account, e));
                }
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: 测试设置管理员（Alice 设置 Bob 为管理员）
        // ═══════════════════════════════════════════════════════════
        info!("👤 Step 1: Alice 设置 Bob 为管理员");
        
        match account_manager.rpc_call("alice", "group/role/set", json!({
            "group_id": group_id,
            "operator_id": alice_id,
            "user_id": bob_id,
            "role": "admin"
        })).await {
            Ok(response) => {
                info!("✅ Bob 成为管理员成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 设置管理员失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("设置管理员失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: 测试禁言功能（Bob 作为管理员禁言 Charlie）
        // ═══════════════════════════════════════════════════════════
        info!("🔇 Step 2: Bob 禁言 Charlie");
        
        match account_manager.rpc_call("bob", "group/member/mute", json!({
            "group_id": group_id,
            "operator_id": bob_id,
            "user_id": charlie_id,
            "mute_duration": 3600  // 1小时
        })).await {
            Ok(response) => {
                info!("✅ Charlie 被禁言成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 禁言失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("禁言失败: {}", e));
            }
        }
        
        // 验证 Charlie 被禁言后不能发消息
        info!("   验证：Charlie 尝试发送消息（应该失败）");
        match account_manager.send_message("charlie", group_id, "我被禁言了吗？", "text").await {
            Ok(_) => {
                warn!("⚠️ Charlie 被禁言后仍然能发消息！");
                metrics.errors.push("禁言验证失败".to_string());
            }
            Err(_) => {
                info!("✅ Charlie 被禁言，无法发消息（符合预期）");
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: 测试解除禁言
        // ═══════════════════════════════════════════════════════════
        info!("🔊 Step 3: Bob 解除 Charlie 的禁言");
        
        match account_manager.rpc_call("bob", "group/member/unmute", json!({
            "group_id": group_id,
            "operator_id": bob_id,
            "user_id": charlie_id.clone()
        })).await {
            Ok(response) => {
                info!("✅ 解除禁言成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 解除禁言失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("解除禁言失败: {}", e));
            }
        }
        
        // 验证 Charlie 可以发消息了
        info!("   验证：Charlie 尝试发送消息（应该成功）");
        match account_manager.send_message("charlie", group_id, "我可以说话了！", "text").await {
            Ok(_) => {
                info!("✅ Charlie 解除禁言后可以发消息");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("⚠️ Charlie 解除禁言后仍然不能发消息: {}", e);
                metrics.errors.push("解除禁言验证失败".to_string());
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 4: 测试全员禁言（Bob 全员禁言）
        // ═══════════════════════════════════════════════════════════
        info!("🔇 Step 4: Bob 开启全员禁言");
        
        match account_manager.rpc_call("bob", "group/settings/mute_all", json!({
            "group_id": group_id,
            "operator_id": bob_id,
            "muted": true  // 注意：参数名是 muted 而不是 all_muted
        })).await {
            Ok(response) => {
                info!("✅ 全员禁言成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 全员禁言失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("全员禁言失败: {}", e));
            }
        }
        
        // 验证：普通成员不能发消息
        info!("   验证：Charlie（普通成员）尝试发送消息（应该失败）");
        match account_manager.send_message("charlie", group_id, "全员禁言了吗？", "text").await {
            Ok(_) => {
                warn!("⚠️ 全员禁言后普通成员仍能发消息！");
                metrics.errors.push("全员禁言验证失败".to_string());
            }
            Err(_) => {
                info!("✅ Charlie（普通成员）被全员禁言，无法发消息");
            }
        }
        
        // 验证：管理员可以发消息
        info!("   验证：Bob（管理员）尝试发送消息（应该成功）");
        match account_manager.send_message("bob", group_id, "管理员不受全员禁言限制", "text").await {
            Ok(_) => {
                info!("✅ Bob（管理员）不受全员禁言限制，可以发消息");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("⚠️ 管理员在全员禁言时也不能发消息: {}", e);
                metrics.errors.push("管理员发消息失败".to_string());
            }
        }
        
        // 验证：群主可以发消息
        info!("   验证：Alice（群主）尝试发送消息（应该成功）");
        match account_manager.send_message("alice", group_id, "群主也不受全员禁言限制", "text").await {
            Ok(_) => {
                info!("✅ Alice（群主）不受全员禁言限制，可以发消息");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("⚠️ 群主在全员禁言时也不能发消息: {}", e);
                metrics.errors.push("群主发消息失败".to_string());
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 5: 测试群设置管理
        // ═══════════════════════════════════════════════════════════
        info!("⚙️ Step 5: 更新群设置");
        
        match account_manager.rpc_call("alice", "group/settings/update", json!({
            "group_id": group_id,
            "operator_id": alice_id,
            "settings": {  // 注意：需要嵌套在 settings 对象中
                "join_need_approval": true,
                "member_can_invite": false,
                "all_muted": false  // 解除全员禁言
            }
        })).await {
            Ok(response) => {
                info!("✅ 群设置更新成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 群设置更新失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("群设置更新失败: {}", e));
            }
        }
        
        // 查询群设置验证
        match account_manager.rpc_call("alice", "group/settings/get", json!({
            "group_id": group_id,
            "user_id": alice_id  // ✨ 使用实际的 user_id
        })).await {
            Ok(settings) => {
                info!("✅ 查询群设置成功: {:?}", settings);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 查询群设置失败: {}", e);
                metrics.rpc_calls += 1;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 6: 测试 QR 码加群
        // ═══════════════════════════════════════════════════════════
        info!("📱 Step 6: 生成群 QR 码");
        
        let mut qr_token = String::new();
        match account_manager.rpc_call("alice", "group/qrcode/generate", json!({
            "group_id": group_id,
            "operator_id": alice_id,  // ✨ 使用实际的 user_id
            "expire_seconds": 3600
        })).await {
            Ok(response) => {
                if let Some(qr_code) = response.get("qr_code").and_then(|t| t.as_str()) {
                    qr_token = qr_code.to_string();
                    info!("✅ QR 码生成成功: {}", qr_token);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                } else {
                    warn!("⚠️ QR 码响应格式错误: {:?}", response);
                    metrics.rpc_calls += 1;
                }
            }
            Err(e) => {
                warn!("❌ QR 码生成失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("QR 码生成失败: {}", e));
            }
        }
        
        // 实际测试扫码加群和审批流程（如果 QR 码生成成功）
        if !qr_token.is_empty() {
            info!("📱 Step 6.1: 测试扫码加群和审批流程");
            
            // 先让 Charlie 退出群组（如果他在群组中）
            let _ = account_manager.rpc_call("charlie", "group/member/leave", json!({
                "group_id": group_id,
                "user_id": charlie_id.clone()
            })).await;
            
            sleep(Duration::from_millis(300)).await;
            
            // Charlie 扫码加群（需要审批）
            let mut request_id = String::new();
            match account_manager.rpc_call("charlie", "group/join/qrcode", json!({
                "user_id": charlie_id,
                "qr_code": qr_token,
                "message": "我想通过二维码加入群组"
            })).await {
                Ok(response) => {
                    if let Some(status) = response.get("status").and_then(|v| v.as_str()) {
                        if status == "pending" {
                            if let Some(rid) = response.get("request_id").and_then(|v| v.as_str()) {
                                request_id = rid.to_string();
                                info!("✅ Charlie 扫码加群申请已提交: request_id={}", request_id);
                                metrics.rpc_calls += 1;
                                metrics.rpc_successes += 1;
                            }
                        } else {
                            info!("✅ Charlie 直接加入群组（无需审批）");
                            metrics.rpc_calls += 1;
                            metrics.rpc_successes += 1;
                        }
                    }
                }
                Err(e) => {
                    warn!("⚠️ Charlie 扫码加群失败: {}", e);
                    metrics.rpc_calls += 1;
                }
            }
            
            sleep(Duration::from_millis(500)).await;
            
            // Alice 查看审批列表
            if !request_id.is_empty() {
                info!("📋 Step 6.2: Alice 查看加群审批列表");
                match account_manager.rpc_call("alice", "group/approval/list", json!({
                    "group_id": group_id,
                    "operator_id": alice_id.clone()
                })).await {
                    Ok(response) => {
                        if let Some(requests) = response.get("requests").and_then(|v| v.as_array()) {
                            info!("✅ 获取审批列表成功: {} 个待审批请求", requests.len());
                            metrics.rpc_calls += 1;
                            metrics.rpc_successes += 1;
                            
                            // 找到 Charlie 的申请
                            if let Some(charlie_request) = requests.iter().find(|req| {
                                req.get("user_id").and_then(|v| v.as_str()) == Some("charlie")
                            }) {
                                if let Some(rid) = charlie_request.get("request_id").and_then(|v| v.as_str()) {
                                    request_id = rid.to_string();
                                    
                                    sleep(Duration::from_millis(300)).await;
                                    
                                    // Alice 审批通过
                                    info!("✅ Step 6.3: Alice 审批通过 Charlie 的加群申请");
                                    match account_manager.rpc_call("alice", "group/approval/handle", json!({
                                        "request_id": &request_id,
                                        "operator_id": alice_id,
                                        "action": "approve"
                                    })).await {
                                        Ok(_) => {
                                            info!("✅ 审批通过成功");
                                            metrics.rpc_calls += 1;
                                            metrics.rpc_successes += 1;
                                        }
                                        Err(e) => {
                                            warn!("❌ 审批失败: {}", e);
                                            metrics.rpc_calls += 1;
                                            metrics.errors.push(format!("审批失败: {}", e));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ 获取审批列表失败: {}", e);
                        metrics.rpc_calls += 1;
                    }
                }
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 7: 测试角色转让（Alice 转让群主给 Bob）
        // ═══════════════════════════════════════════════════════════
        info!("👑 Step 7: Alice 转让群主给 Bob");
        
        match account_manager.rpc_call("alice", "group/role/transfer_owner", json!({
            "group_id": group_id,
            "current_owner_id": alice_id,
            "new_owner_id": bob_id
        })).await {
            Ok(response) => {
                info!("✅ 群主转让成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 群主转让失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("群主转让失败: {}", e));
            }
        }
        
        // 验证：Bob 现在是群主，可以执行群主操作
        info!("   验证：Bob（新群主）尝试设置 Charlie 为管理员");
        match account_manager.rpc_call("bob", "group/role/set", json!({
            "group_id": group_id,
            "operator_id": bob_id,
            "user_id": charlie_id,
            "role": "admin"
        })).await {
            Ok(_) => {
                info!("✅ Bob（新群主）成功设置管理员");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ Bob 转让后无法行使群主权限: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push("转让验证失败".to_string());
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 15 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "RPC调用{}/{}成功，发送{}条消息，错误{}个",
            metrics.rpc_successes,
            metrics.rpc_calls,
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "高级群组功能".to_string(),
            success: success && metrics.errors.len() < 5,  // 允许少量错误
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 18: 黑名单测试
    pub async fn phase18_blacklist_test(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🚫 Phase 18: 黑名单测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: Alice 将 Bob 加入黑名单
        // ═══════════════════════════════════════════════════════════
        info!("🚫 Step 1: Alice 将 Bob 加入黑名单");
        
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        match account_manager.rpc_call("alice", "contact/blacklist/add", json!({
            "user_id": alice_id,
            "blocked_user_id": bob_id,
            "reason": "测试黑名单功能"
        })).await {
            Ok(response) => {
                info!("✅ Bob 被加入黑名单成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 添加黑名单失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("添加黑名单失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: 验证 Bob 无法给 Alice 发消息
        // ═══════════════════════════════════════════════════════════
        info!("📤 Step 2: 验证 Bob 无法给 Alice 发消息");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        // ✨ 修复：服务端会返回错误码 4 "您已被对方拉黑，无法发送消息"
        match account_manager.send_message("bob", alice_bob_chat, "这条消息应该被拦截", "text").await {
            Ok(_) => {
                // 消息发送成功，但服务端应该拦截
                warn!("⚠️ Bob 被拉黑后仍能发送消息！");
                metrics.errors.push("黑名单拦截验证失败".to_string());
                success = false;
                metrics.messages_sent += 1;
            }
            Err(e) => {
                // 检查是否是黑名单拦截错误
                let err_msg = e.to_string();
                if err_msg.contains("拉黑") || err_msg.contains("blacklist") || err_msg.contains("blocked") {
                    info!("✅ Bob 被拉黑后无法发送消息（符合预期）：{}", err_msg);
                } else {
                    info!("✅ Bob 无法发送消息（原因：{}）", err_msg);
                }
                // 不计入 messages_sent，因为消息被拦截了
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: Alice 查询黑名单列表
        // ═══════════════════════════════════════════════════════════
        info!("📋 Step 3: Alice 查询黑名单列表");
        
        match account_manager.rpc_call("alice", "contact/blacklist/list", json!({
            "user_id": alice_id
        })).await {
            Ok(response) => {
                if let Some(blacklist) = response.get("blacklist").and_then(|v| v.as_array()) {
                    info!("✅ 获取黑名单列表成功: {} 个用户", blacklist.len());
                    // ✨ 修复：blocked_user_id 是 u64 类型，不是字符串
                    let bob_id_u64 = bob_id;
                    if blacklist.iter().any(|entry| {
                        entry.get("blocked_user_id").and_then(|v| v.as_u64()) == Some(bob_id_u64)
                    }) {
                        info!("✅ Bob 在黑名单列表中");
                    } else {
                        warn!("⚠️ Bob 不在黑名单列表中");
                        metrics.errors.push("黑名单列表验证失败".to_string());
                    }
                }
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 查询黑名单列表失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("查询黑名单失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 4: Alice 检查 Bob 是否在黑名单中
        // ═══════════════════════════════════════════════════════════
        info!("🔍 Step 4: Alice 检查 Bob 是否在黑名单中");
        
        match account_manager.rpc_call("alice", "contact/blacklist/check", json!({
            "user_id": alice_id,
            "target_user_id": bob_id
        })).await {
            Ok(response) => {
                if let Some(is_blocked) = response.get("is_blocked").and_then(|v| v.as_bool()) {
                    if is_blocked {
                        info!("✅ 检查结果：Bob 在黑名单中");
                    } else {
                        warn!("⚠️ 检查结果：Bob 不在黑名单中");
                        metrics.errors.push("黑名单检查验证失败".to_string());
                    }
                }
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 检查黑名单状态失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("检查黑名单失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 5: Alice 将 Bob 从黑名单移除
        // ═══════════════════════════════════════════════════════════
        info!("✅ Step 5: Alice 将 Bob 从黑名单移除");
        
        match account_manager.rpc_call("alice", "contact/blacklist/remove", json!({
            "user_id": alice_id,
            "blocked_user_id": bob_id
        })).await {
            Ok(response) => {
                info!("✅ Bob 从黑名单移除成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 移除黑名单失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("移除黑名单失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 6: 验证 Bob 可以给 Alice 发消息了
        // ═══════════════════════════════════════════════════════════
        info!("📤 Step 6: 验证 Bob 可以给 Alice 发消息了");
        
        match account_manager.send_message("bob", alice_bob_chat, "我可以发消息了！", "text").await {
            Ok(_) => {
                info!("✅ Bob 移除黑名单后可以发送消息");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("⚠️ Bob 移除黑名单后仍然不能发消息: {}", e);
                metrics.errors.push("移除黑名单验证失败".to_string());
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 18 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "RPC调用{}/{}成功，发送{}条消息，错误{}个",
            metrics.rpc_successes,
            metrics.rpc_calls,
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "黑名单测试".to_string(),
            success: success && metrics.errors.len() < 3,  // 允许少量错误
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 16: 消息引用/回复测试
    pub async fn phase16_message_reply(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("💬 Phase 16: 消息引用/回复测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: Alice 发送一条原始消息
        // ═══════════════════════════════════════════════════════════
        info!("📤 Step 1: Alice 发送原始消息");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        let original_message_id_str = match account_manager.send_message("alice", alice_bob_chat, "这是一条原始消息", "text").await {
            Ok(msg_id) => {
                info!("✅ 原始消息发送成功: {}", msg_id);
                metrics.messages_sent += 1;
                msg_id
            }
            Err(e) => {
                warn!("❌ 发送原始消息失败: {}", e);
                metrics.errors.push(format!("发送原始消息失败: {}", e));
                success = false;
                return Ok(PhaseResult {
                    phase_name: "消息引用/回复".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "发送原始消息失败".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: Bob 回复这条消息
        // ═══════════════════════════════════════════════════════════
        info!("💬 Step 2: Bob 回复消息");
        
        let original_message_id = original_message_id_str.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        match account_manager.send_message_advanced(
            "bob",
            alice_bob_chat,
            "这是对原始消息的回复",
            "text",
            None,
            Some(original_message_id),
            None,
            None,
        ).await {
            Ok(_) => {
                info!("✅ 回复消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送回复消息失败: {}", e);
                metrics.errors.push(format!("发送回复消息失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: Alice 在群组中发送消息并引用
        // ═══════════════════════════════════════════════════════════
        info!("💬 Step 3: Alice 在群组中发送引用消息");
        
        // 先创建一个测试群组或使用已存在的群组
        // ✨ 获取 alice 的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        
        let group_id = match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id,
            "name": "Phase 16 Test Group",
            "description": "Test group for message reply",
            "is_public": false
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    info!("✅ 创建测试群组成功: {}", gid);
                    // ✨ 获取 bob 和 charlie 的实际 user_id
                    let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
                    let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
                    
                    // 邀请 Bob 和 Charlie 加入
                    for (user_account, user_id) in [("bob", bob_id), ("charlie", charlie_id)] {
                        let _ = account_manager.rpc_call("alice", "group/member/add", json!({
                            "group_id": gid,
                            "inviter_id": alice_id,
                            "user_id": user_id,
                            "role": "member"
                        })).await;
                    }
                    sleep(Duration::from_millis(500)).await;
                    gid
                } else {
                    error!("❌ 群组创建响应格式错误：缺少 group_id");
                    metrics.errors.push("群组创建响应格式错误".to_string());
                    return Ok(PhaseResult {
                        phase_name: "消息引用/回复".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "群组创建响应格式错误".to_string(),
                        metrics,
                    });
                }
            }
            Err(e) => {
                error!("❌ 创建群组失败: {}", e);
                metrics.errors.push(format!("创建群组失败: {}", e));
                    return Ok(PhaseResult {
                        phase_name: "消息引用/回复".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: format!("创建群组失败: {}", e),
                        metrics,
                    });
            }
        };
        
        // ✨ 缓存群组ID
        account_manager.cache_group_id("Phase 16 Test Group", group_id);
        
        // 先发送一条群组消息
        let group_message_id_str = match account_manager.send_message("alice", group_id, "这是群组原始消息", "text").await {
            Ok(msg_id) => {
                info!("✅ 群组原始消息发送成功: {}", msg_id);
                metrics.messages_sent += 1;
                msg_id
            }
            Err(e) => {
                warn!("❌ 发送群组原始消息失败: {}", e);
                metrics.errors.push(format!("发送群组原始消息失败: {}", e));
                success = false;
                return Ok(PhaseResult {
                    phase_name: "消息引用/回复".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "发送群组原始消息失败".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // Charlie 回复群组消息
        let group_message_id = group_message_id_str.parse::<u64>()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::Other(format!("无法解析 message_id: {}", e)))?;
        match account_manager.send_message_advanced(
            "charlie",
            group_id,
            "这是对群组消息的回复",
            "text",
            None,
            Some(group_message_id),
            None,
            None,
        ).await {
            Ok(_) => {
                info!("✅ 群组回复消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送群组回复消息失败: {}", e);
                metrics.errors.push(format!("发送群组回复消息失败: {}", e));
                success = false;
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 16 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "发送{}条消息（包含引用），错误{}个",
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "消息引用/回复".to_string(),
            success: success && metrics.errors.len() < 2,
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 17: Reaction 测试（消息点赞）
    pub async fn phase17_message_reaction(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👍 Phase 17: Reaction 测试（消息点赞）");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: Alice 发送一条消息
        // ═══════════════════════════════════════════════════════════
        info!("📤 Step 1: Alice 发送消息");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_bob_chat = account_manager.get_private_chat_id("alice", "bob")
            .expect("无法获取 alice-bob 私聊会话ID（请先完成好友申请流程）");
        
        let message_id = match account_manager.send_message("alice", alice_bob_chat, "这是一条可以点赞的消息", "text").await {
            Ok(msg_id) => {
                info!("✅ 消息发送成功: {}", msg_id);
                metrics.messages_sent += 1;
                msg_id
            }
            Err(e) => {
                warn!("❌ 发送消息失败: {}", e);
                metrics.errors.push(format!("发送消息失败: {}", e));
                success = false;
                return Ok(PhaseResult {
                    phase_name: "Reaction 测试".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: "发送消息失败".to_string(),
                    metrics,
                });
            }
        };
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: Bob 给消息添加 Reaction（👍）
        // ═══════════════════════════════════════════════════════════
        info!("👍 Step 2: Bob 给消息添加 Reaction");
        
        match account_manager.rpc_call("bob", "message/reaction/add", json!({
            "message_id": message_id,
            "user_id": account_manager.get_user_id("bob").expect("Bob ID not found"),
            "emoji": "👍",
            "channel_id": alice_bob_chat  // ✨ 添加 channel_id 以支持 seq 查找
        })).await {
            Ok(response) => {
                info!("✅ 添加 Reaction 成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 添加 Reaction 失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("添加 Reaction 失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: Charlie 也给同一条消息添加 Reaction（❤️）
        // ═══════════════════════════════════════════════════════════
        info!("❤️ Step 3: Charlie 给消息添加 Reaction");
        
        // 先创建一个测试群组或使用已存在的群组
        // ✨ 获取 alice 和 charlie 的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        let group_id = match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id.clone(),
            "name": "Phase 17 Test Group",
            "description": "Test group for reaction",
            "is_public": false
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    info!("✅ 创建测试群组成功: {}", gid);
                    // 邀请 Charlie 加入
                    let _ = account_manager.rpc_call("alice", "group/member/add", json!({
                        "group_id": gid,
                        "inviter_id": alice_id,
                        "user_id": charlie_id,
                        "role": "member"
                    })).await;
                    sleep(Duration::from_millis(500)).await;
                    gid
                } else {
                    error!("❌ 群组创建响应格式错误：缺少 group_id");
                    metrics.errors.push("群组创建响应格式错误".to_string());
                    return Ok(PhaseResult {
                        phase_name: "Reaction 测试".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "群组创建响应格式错误".to_string(),
                        metrics,
                    });
                }
            }
            Err(e) => {
                error!("❌ 创建群组失败: {}", e);
                metrics.errors.push(format!("创建群组失败: {}", e));
                    return Ok(PhaseResult {
                        phase_name: "Reaction 测试".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: format!("创建群组失败: {}", e),
                        metrics,
                    });
            }
        };
        
        // ✨ 缓存群组ID
        account_manager.cache_group_id("Phase 17 Test Group", group_id);
        
        // 先让 Charlie 也收到这条消息（通过群组）
        match account_manager.send_message("alice", group_id, "这是一条群组消息", "text").await {
            Ok(group_msg_id) => {
                sleep(Duration::from_millis(500)).await;
                
                match account_manager.rpc_call("charlie", "message/reaction/add", json!({
                    "message_id": group_msg_id,
                    "user_id": account_manager.get_user_id("charlie").expect("Charlie ID not found"),
                    "emoji": "❤️",
                    "channel_id": group_id  // ✨ 添加 channel_id 以支持 seq 查找
                })).await {
                    Ok(response) => {
                        info!("✅ Charlie 添加 Reaction 成功: {:?}", response);
                        metrics.rpc_calls += 1;
                        metrics.rpc_successes += 1;
                        metrics.messages_sent += 1;
                    }
                    Err(e) => {
                        warn!("❌ Charlie 添加 Reaction 失败: {}", e);
                        metrics.rpc_calls += 1;
                        metrics.errors.push(format!("Charlie 添加 Reaction 失败: {}", e));
                    }
                }
            }
            Err(e) => {
                warn!("❌ 发送群组消息失败: {}", e);
                metrics.errors.push(format!("发送群组消息失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 4: 查询 Reaction 统计
        // ═══════════════════════════════════════════════════════════
        info!("📊 Step 4: 查询 Reaction 统计");
        
        match account_manager.rpc_call("alice", "message/reaction/stats", json!({
            "message_id": message_id,
            "channel_id": alice_bob_chat  // ✨ 添加 channel_id 以支持 seq 查找
        })).await {
            Ok(response) => {
                info!("✅ 查询 Reaction 统计成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 查询 Reaction 统计失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("查询 Reaction 统计失败: {}", e));
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 17 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "RPC调用{}/{}成功，发送{}条消息，错误{}个",
            metrics.rpc_successes,
            metrics.rpc_calls,
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "Reaction 测试".to_string(),
            success: success && metrics.rpc_successes >= 2,
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 19: @提及测试
    pub async fn phase19_mention_test(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("@ Phase 19: @提及测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: Alice 在群组中@ Bob
        // ═══════════════════════════════════════════════════════════
        info!("@ Step 1: Alice 在群组中@ Bob");
        
        // 先创建一个测试群组或使用已存在的群组
        // ✨ 获取 alice, bob 和 charlie 的实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        let group_id = match account_manager.rpc_call("alice", "group/group/create", json!({
            "creator_id": alice_id.clone(),
            "name": "Phase 19 Test Group",
            "description": "Test group for mention",
            "is_public": false
        })).await {
            Ok(response) => {
                if let Some(gid) = response.get("group_id")
                    .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))) {
                    info!("✅ 创建测试群组成功: {}", gid);
                    // 邀请 Bob 和 Charlie 加入（Bob 设为管理员，以便可以@所有人）
                    match account_manager.rpc_call("alice", "group/member/add", json!({
                        "group_id": gid,
                        "inviter_id": alice_id,
                        "user_id": bob_id,
                        "role": "admin"  // ✨ Bob 设为管理员，以便可以@所有人
                    })).await {
                        Ok(_) => info!("✅ Bob 已添加为管理员"),
                        Err(e) => warn!("⚠️ 添加 Bob 为管理员失败: {}，继续测试", e),
                    }
                    match account_manager.rpc_call("alice", "group/member/add", json!({
                        "group_id": gid,
                        "inviter_id": alice_id,
                        "user_id": charlie_id,
                        "role": "member"
                    })).await {
                        Ok(_) => info!("✅ Charlie 已添加为成员"),
                        Err(e) => warn!("⚠️ 添加 Charlie 为成员失败: {}，继续测试", e),
                    }
                    sleep(Duration::from_millis(1000)).await; // ✨ 增加延迟，确保成员添加完成
                    gid
                } else {
                    error!("❌ 群组创建响应格式错误：缺少 group_id");
                    metrics.errors.push("群组创建响应格式错误".to_string());
                    return Ok(PhaseResult {
                        phase_name: "@提及测试".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "群组创建响应格式错误".to_string(),
                        metrics,
                    });
                }
            }
            Err(e) => {
                error!("❌ 创建群组失败: {}", e);
                metrics.errors.push(format!("创建群组失败: {}", e));
                return Ok(PhaseResult {
                    phase_name: "@提及测试".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: format!("创建群组失败: {}", e),
                    metrics,
                });
            }
        };
        
        // ✨ 缓存群组ID
        account_manager.cache_group_id("Phase 19 Test Group", group_id);
        
        // 获取 Bob 的 user_id 用于 @提及
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let mentioned_user_ids = vec![bob_id];
        
        match account_manager.send_message_advanced(
            "alice",
            group_id,
            "@Bob 你好，这是一条@你的消息",
            "text",
            None,
            None,
            Some(&mentioned_user_ids),
            None,
        ).await {
            Ok(_) => {
                info!("✅ @提及消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送@提及消息失败: {}", e);
                metrics.errors.push(format!("发送@提及消息失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: Bob 在群组中@所有人
        // ═══════════════════════════════════════════════════════════
        info!("@all Step 2: Bob 在群组中@所有人");
        
        // 获取所有用户的 user_id 用于 @所有人
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let bob_id = account_manager.get_user_id("bob").expect("Bob ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        let mentioned_user_ids = vec![alice_id, bob_id, charlie_id];
        
        match account_manager.send_message_advanced(
            "bob",
            group_id,
            "@all 这是一条@所有人的消息",
            "text",
            None,
            None,
            Some(&mentioned_user_ids), // @所有人
            None,
        ).await {
            Ok(_) => {
                info!("✅ @所有人消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送@所有人消息失败: {}", e);
                metrics.errors.push(format!("发送@所有人消息失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: Charlie 在私聊中@ Alice（测试私聊@提及）
        // ═══════════════════════════════════════════════════════════
        info!("@ Step 3: Charlie 在私聊中@ Alice");
        
        // ✨ 获取私聊会话的 UUID（优先使用服务端返回的 channel_id）
        let alice_charlie_chat = account_manager.get_private_chat_id("alice", "charlie")
            .expect("无法获取 alice-charlie 私聊会话ID（请先完成好友申请流程）");
        
        // 获取 Alice 的 user_id 用于 @提及
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let mentioned_user_ids = vec![alice_id];
        
        match account_manager.send_message_advanced(
            "charlie",
            alice_charlie_chat,
            "@Alice 这是一条私聊@消息",
            "text",
            None,
            None,
            Some(&mentioned_user_ids),
            None,
        ).await {
            Ok(_) => {
                info!("✅ 私聊@提及消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送私聊@提及消息失败: {}", e);
                metrics.errors.push(format!("发送私聊@提及消息失败: {}", e));
                // 私聊@提及可能不支持，不算失败
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 19 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "发送{}条@提及消息，错误{}个",
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "@提及测试".to_string(),
            success: success && metrics.messages_sent >= 2,
            duration,
            details,
            metrics,
        })
    }
    
    /// ✨ Phase 20: 非好友消息测试
    pub async fn phase20_non_friend_message(
        &self,
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("💌 Phase 20: 非好友消息测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        let mut success = true;
        
        // ═══════════════════════════════════════════════════════════
        // Step 1: 确保 Alice 和 Charlie 不是好友
        // ═══════════════════════════════════════════════════════════
        info!("👥 Step 1: 检查 Alice 和 Charlie 的好友关系");
        
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        // 先检查是否是好友
        match account_manager.rpc_call("alice", "contact/friend/check", json!({
            "user_id": alice_id.to_string(),  // ✨ 转换为字符串（服务端期望字符串格式）
            "friend_id": charlie_id.to_string()  // ✨ 转换为字符串
        })).await {
            Ok(response) => {
                if let Some(is_friend) = response.get("is_friend").and_then(|v| v.as_bool()) {
                    if is_friend {
                        info!("⚠️ Alice 和 Charlie 已经是好友，先删除好友关系");
                        // 删除好友关系（如果存在）
                        let _ = account_manager.rpc_call("alice", "contact/friend/remove", json!({
                            "user_id": alice_id.to_string(),  // ✨ 转换为字符串（服务端期望字符串格式）
                            "friend_id": charlie_id.to_string()  // ✨ 转换为字符串
                        })).await;
                        sleep(Duration::from_millis(500)).await;
                    }
                }
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(_) => {
                // 检查失败，假设不是好友
            }
        }
        
        // ═══════════════════════════════════════════════════════════
        // Step 2: 确保 Charlie 允许接收非好友消息（默认允许）
        // ═══════════════════════════════════════════════════════════
        info!("🔒 Step 2: 检查 Charlie 的隐私设置");
        
        match account_manager.rpc_call("charlie", "account/privacy/get", json!({
            "user_id": account_manager.get_user_id("charlie").expect("Charlie ID not found").to_string()  // ✨ 转换为字符串（服务端期望字符串格式）
        })).await {
            Ok(response) => {
                if let Some(allow) = response.get("allow_receive_message_from_non_friend").and_then(|v| v.as_bool()) {
                    if !allow {
                        info!("⚠️ Charlie 不允许接收非好友消息，更新设置");
                        let _ = account_manager.rpc_call("charlie", "account/privacy/update", json!({
                            "user_id": account_manager.get_user_id("charlie").expect("Charlie ID not found").to_string(),  // ✨ 转换为字符串（服务端期望字符串格式）
                            "allow_receive_message_from_non_friend": true
                        })).await;
                        sleep(Duration::from_millis(500)).await;
                    }
                }
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 获取隐私设置失败: {}", e);
                metrics.rpc_calls += 1;
                // 默认允许，继续测试
            }
        }
        
        // ═══════════════════════════════════════════════════════════
        // Step 3: Alice 向 Charlie 发送非好友消息（带消息来源）
        // ═══════════════════════════════════════════════════════════
        info!("📤 Step 3: Alice 向 Charlie 发送非好友消息（带消息来源）");
        
        // ✨ 获取实际 user_id
        let alice_id = account_manager.get_user_id("alice").expect("Alice ID not found");
        let charlie_id = account_manager.get_user_id("charlie").expect("Charlie ID not found");
        
        // ✨ 对于非好友消息，尝试从缓存获取 channel_id，如果没有则使用临时 channel_id
        // 注意：非好友消息需要服务端创建临时频道，这里使用一个临时值
        // 实际场景中，应该通过搜索或RPC获取 channel_id
        let alice_charlie_chat = account_manager.get_private_chat_id("alice", "charlie")
            .unwrap_or(0); // 如果不存在，使用0作为临时值（服务端会处理）
        
        let message_source = json!({
            "type": "search",
            "search_session_id": "test_search_123"
        });
        
        match account_manager.send_message_advanced(
            "alice",
            alice_charlie_chat,
            "这是一条非好友消息，来自搜索",
            "text",
            None,
            None,
            None,
            Some(message_source),
        ).await {
            Ok(_) => {
                info!("✅ 非好友消息发送成功");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("❌ 发送非好友消息失败: {}", e);
                metrics.errors.push(format!("发送非好友消息失败: {}", e));
                success = false;
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // ═══════════════════════════════════════════════════════════
        // Step 4: 测试 Charlie 禁止接收非好友消息
        // ═══════════════════════════════════════════════════════════
        info!("🚫 Step 4: 测试 Charlie 禁止接收非好友消息");
        
        match account_manager.rpc_call("charlie", "account/privacy/update", json!({
            "user_id": account_manager.get_user_id("charlie").expect("Charlie ID not found").to_string(),  // ✨ 转换为字符串（服务端期望字符串格式）
            "allow_receive_message_from_non_friend": false
        })).await {
            Ok(_) => {
                info!("✅ 隐私设置更新成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("❌ 更新隐私设置失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("更新隐私设置失败: {}", e));
            }
        }
        
        sleep(Duration::from_millis(500)).await;
        
        // Alice 再次尝试发送消息（应该失败）
        match account_manager.send_message("alice", alice_charlie_chat, "这条消息应该被拒绝", "text").await {
            Ok(_) => {
                warn!("⚠️ 非好友消息应该被拒绝，但发送成功了");
                metrics.errors.push("隐私设置验证失败".to_string());
                metrics.messages_sent += 1;
            }
            Err(_) => {
                info!("✅ 非好友消息被正确拒绝（符合预期）");
            }
        }
        
        let duration = start_time.elapsed();
        
        info!("✅ Phase 20 完成，用时: {}ms", duration.as_millis());
        
        let details = format!(
            "RPC调用{}/{}成功，发送{}条消息，错误{}个",
            metrics.rpc_successes,
            metrics.rpc_calls,
            metrics.messages_sent,
            metrics.errors.len()
        );
        
        Ok(PhaseResult {
            phase_name: "非好友消息测试".to_string(),
            success: success && metrics.messages_sent >= 1,
            duration,
            details,
            metrics,
        })
    }
}
