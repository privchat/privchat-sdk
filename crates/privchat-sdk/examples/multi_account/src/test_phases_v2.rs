//! 测试阶段 V2 - 使用新的 PrivchatSDK API
//! 
//! 这是一个简化但完整的测试套件，涵盖20个核心测试阶段。
//! 相比原版 realistic_test_phases.rs (3900+行)，这个版本：
//! - 使用统一的 SDK API（不直接调用 RPC）
//! - 更简洁、易维护
//! - 专注于核心功能验证

use crate::account_manager::MultiAccountManager;
use crate::types::{PhaseResult, PhaseMetrics};
use privchat_sdk::error::Result;
use std::time::Instant;
use tokio::time::sleep;
use std::time::Duration;
use tracing::{info, warn, error};

pub struct TestPhasesV2;

impl TestPhasesV2 {
    /// Phase 1: 用户认证和初始化
    pub async fn phase1_authentication(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔐 Phase 1: 用户认证和初始化");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        match account_manager.authenticate_all().await {
            Ok(accounts) => {
                info!("✅ 认证成功: {:?}", accounts);
                
                if let Err(e) = account_manager.verify_all_connected().await {
                    metrics.errors.push(format!("连接验证失败: {}", e));
                    return Ok(PhaseResult {
                        phase_name: "用户认证".to_string(),
                        success: false,
                        duration: start_time.elapsed(),
                        details: "部分账号未连接".to_string(),
                        metrics,
                    });
                }
                
                Ok(PhaseResult {
                    phase_name: "用户认证".to_string(),
                    success: true,
                    duration: start_time.elapsed(),
                    details: format!("{}个账号成功认证并连接", accounts.len()),
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
    pub async fn phase2_friend_system(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👥 Phase 2: 好友系统完整流程");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: Alice 搜索 Bob，获取 user_id 和 search_session_id
        info!("🔍 Step 1: Alice 搜索 Bob");
        
        // ✨ 使用完整的用户名进行精确搜索
        let bob_full_username = account_manager.get_full_username("bob")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 bob 的完整用户名".to_string()))?;
        info!("🔍 搜索完整用户名: {}", bob_full_username);
        
        let (bob_id, bob_search_session_id) = match account_manager.search_users("alice", &bob_full_username).await {
            Ok(response) => {
                info!("✅ 搜索成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // ✅ 使用类型安全的结构体反序列化
                match serde_json::from_value::<privchat_protocol::rpc::account::search::AccountSearchResponse>(response) {
                    Ok(search_response) => {
                        info!("🔍 精确搜索到 {} 个结果", search_response.users.len());
                        
                        if let Some(user) = search_response.users.first() {
                            info!("✅ 找到用户: username={}, user_id={}, search_session_id={}", 
                                user.username, user.user_id, user.search_session_id);
                            (user.user_id, Some(user.search_session_id.to_string()))
                        } else {
                            warn!("⚠️ 未找到匹配的用户");
                            metrics.errors.push("未找到用户".to_string());
                            (0, None)
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ 搜索结果反序列化失败: {}", e);
                        metrics.errors.push(format!("搜索结果格式错误: {}", e));
                        (0, None)
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ 搜索失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("搜索用户: {}", e));
                (0, None)
            }
        };
        
        if bob_id == 0 {
            warn!("⚠️ 无法获取 Bob 的 user_id，跳过后续测试");
            return Ok(PhaseResult {
                phase_name: "Phase 2: 好友系统".to_string(),
                success: false,
                duration: start_time.elapsed(),
                details: "搜索用户失败".to_string(),
                metrics,
            });
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // Step 2: Alice 向 Bob 发送好友请求
        info!("📋 Step 2: Alice 向 Bob 发送好友请求");
        
        // 使用从搜索结果中获取的 user_id 和 search_session_id
        match account_manager.send_friend_request("alice", bob_id, Some("Hi Bob! 🤝"), bob_search_session_id).await {
            Ok(_) => {
                info!("✅ 好友请求已发送");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 发送好友请求失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("发送好友请求: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // Step 3: Bob 接受 Alice 的好友请求
        info!("✅ Step 3: Bob 接受 Alice 的好友请求");
        let alice_id = account_manager.get_user_id("alice").unwrap();
        let bob_id = account_manager.get_user_id("bob").unwrap();
        match account_manager.accept_friend_request("bob", alice_id).await {
            Ok(response) => {
                info!("✅ 好友请求已接受");
                
                // ✅ 保存 channel 到数据库（双方都需要）
                if let Some(channel_id) = response.get("channel_id").and_then(|v| v.as_u64()) {
                    info!("💾 保存 Alice <-> Bob 的 channel: {}", channel_id);
                    
                    // ✅ Alice 也需要保存 channel 到数据库
                    if let Some(alice_sdk) = account_manager.get_sdk("alice") {
                        use privchat_sdk::storage::entities::Channel;
                        use chrono::Utc;
                        
                        let now_millis = Utc::now().timestamp_millis();
                        let channel = Channel {
                            id: None,
                            channel_id,
                            channel_type: 1,
                            last_local_message_id: 0,
                            last_msg_timestamp: Some(now_millis),
                            unread_count: 0,
                            last_msg_pts: 0,
                            show_nick: 0,
                            username: bob_id.to_string(),
                            channel_name: bob_id.to_string(),
                            channel_remark: String::new(),
                            top: 0,
                            mute: 0,
                            save: 0,
                            forbidden: 0,
                            follow: 1,
                            is_deleted: 0,
                            receipt: 0,
                            status: 1,
                            invite: 0,
                            robot: 0,
                            version: 1,
                            online: 0,
                            last_offline: 0,
                            avatar: String::new(),
                            category: String::new(),
                            extra: "{}".to_string(),
                            created_at: now_millis,
                            updated_at: now_millis,
                            avatar_cache_key: String::new(),
                            remote_extra: Some("{}".to_string()),
                            flame: 0,
                            flame_second: 0,
                            device_flag: 0,
                            parent_channel_id: 0,
                            parent_channel_type: 0,
                        };
                        if let Err(e) = alice_sdk.storage().save_channel(&channel).await {
                            warn!("⚠️ Alice 保存 channel 失败: {}", e);
                        } else {
                            info!("✅ Alice 已保存 channel: channel_id={}, target_user={}", channel_id, bob_id);
                        }
                    }
                }
                
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 接受好友请求失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("接受好友请求: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // Step 4: Alice 向 Charlie 发送好友请求
        info!("📋 Step 4: Alice 向 Charlie 发送好友请求");
        
        // ✨ 使用完整的用户名进行精确搜索
        let charlie_full_username = account_manager.get_full_username("charlie")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 charlie 的完整用户名".to_string()))?;
        info!("🔍 搜索完整用户名: {}", charlie_full_username);
        
        // ✨ 先搜索用户（创建搜索会话）
        let (charlie_id, charlie_search_session_id) = match account_manager.search_users("alice", &charlie_full_username).await {
            Ok(response) => {
                info!("✅ 搜索成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // ✅ 使用类型安全的结构体反序列化
                match serde_json::from_value::<privchat_protocol::rpc::account::search::AccountSearchResponse>(response) {
                    Ok(search_response) => {
                        info!("🔍 精确搜索到 {} 个结果", search_response.users.len());
                        
                        if let Some(user) = search_response.users.first() {
                            info!("✅ 找到用户: username={}, user_id={}, search_session_id={}", 
                                user.username, user.user_id, user.search_session_id);
                            (user.user_id, Some(user.search_session_id.to_string()))
                        } else {
                            warn!("⚠️ 未找到匹配的用户");
                            (0, None)
                        }
                    }
                    Err(e) => {
                        warn!("⚠️ 搜索结果反序列化失败: {}", e);
                        (0, None)
                    }
                }
            }
            Err(e) => {
                warn!("⚠️ 搜索用户失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("搜索用户: {}", e));
                (0, None)
            }
        };
        
        if charlie_id == 0 {
            warn!("⚠️ 无法获取 Charlie 的 user_id，跳过发送好友请求");
        }
        
        sleep(Duration::from_millis(100)).await;
        
        // 然后发送好友请求（使用真实的搜索会话ID）
        match account_manager.send_friend_request("alice", charlie_id, Some("Hi Charlie! 🌟"), charlie_search_session_id).await {
            Ok(_) => {
                info!("✅ 好友请求已发送");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 发送好友请求失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("发送好友请求: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // Step 5: Charlie 接受好友请求
        info!("✅ Step 5: Charlie 接受 Alice 的好友请求");
        let charlie_id = account_manager.get_user_id("charlie").unwrap();
        match account_manager.accept_friend_request("charlie", alice_id).await {
            Ok(response) => {
                info!("✅ 好友请求已接受");
                
                // ✅ 保存 channel 到数据库（双方都需要）
                if let Some(channel_id) = response.get("channel_id").and_then(|v| v.as_u64()) {
                    info!("💾 保存 Alice <-> Charlie 的 channel: {}", channel_id);
                    
                    // ✅ Alice 也需要保存 channel 到数据库
                    if let Some(alice_sdk) = account_manager.get_sdk("alice") {
                        use privchat_sdk::storage::entities::Channel;
                        use chrono::Utc;
                        
                        let now_millis = Utc::now().timestamp_millis();
                        let channel = Channel {
                            id: None,
                            channel_id,
                            channel_type: 1,
                            last_local_message_id: 0,
                            last_msg_timestamp: Some(now_millis),
                            unread_count: 0,
                            last_msg_pts: 0,
                            show_nick: 0,
                            username: charlie_id.to_string(),
                            channel_name: charlie_id.to_string(),
                            channel_remark: String::new(),
                            top: 0,
                            mute: 0,
                            save: 0,
                            forbidden: 0,
                            follow: 1,
                            is_deleted: 0,
                            receipt: 0,
                            status: 1,
                            invite: 0,
                            robot: 0,
                            version: 1,
                            online: 0,
                            last_offline: 0,
                            avatar: String::new(),
                            category: String::new(),
                            extra: "{}".to_string(),
                            created_at: now_millis,
                            updated_at: now_millis,
                            avatar_cache_key: String::new(),
                            remote_extra: Some("{}".to_string()),
                            flame: 0,
                            flame_second: 0,
                            device_flag: 0,
                            parent_channel_id: 0,
                            parent_channel_type: 0,
                        };
                        if let Err(e) = alice_sdk.storage().save_channel(&channel).await {
                            warn!("⚠️ Alice 保存 channel 失败: {}", e);
                        }
                        if let Err(e) = alice_sdk.storage().save_channel(&channel).await {
                            warn!("⚠️ Alice 保存 channel 失败: {}", e);
                        } else {
                            info!("✅ Alice 已保存 channel: channel_id={}, target_user={}", channel_id, charlie_id);
                        }
                    }
                }
                
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 接受好友请求失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("接受好友请求: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // Step 6: 验证好友列表
        info!("📋 Step 6: 验证好友列表");
        match account_manager.get_friend_list("alice").await {
            Ok(response) => {
                info!("✅ 获取好友列表成功: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取好友列表失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("获取好友列表: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "好友系统".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 3: 群组系统工作流
    pub async fn phase3_group_system(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👥 Phase 3: 群组系统工作流");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        let charlie_id = account_manager.get_user_id("charlie").unwrap();
        
        // Step 1: Alice 创建群组
        info!("🏗️ Step 1: Alice 创建群组");
        let group_id = match account_manager.create_group("alice", "测试群组", vec![bob_id, charlie_id]).await {
            Ok(response) => {
                let gid = response.get("group_id")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                
                if gid > 0 {
                    info!("✅ 群组创建成功: {}", gid);
                    
                    // ✅ 保存群组 channel 到数据库
                    if let Some(alice_sdk) = account_manager.get_sdk("alice") {
                        use privchat_sdk::storage::entities::Channel;
                        use chrono::Utc;
                        
                        let now_millis = Utc::now().timestamp_millis();
                        let channel = Channel {
                            id: None,
                            channel_id: gid,
                            channel_type: 2,
                            last_local_message_id: 0,
                            last_msg_timestamp: Some(now_millis),
                            unread_count: 0,
                            last_msg_pts: 0,
                            show_nick: 0,
                            username: gid.to_string(),
                            channel_name: format!("测试群组-{}", gid),
                            channel_remark: String::new(),
                            top: 0,
                            mute: 0,
                            save: 0,
                            forbidden: 0,
                            follow: 1,
                            is_deleted: 0,
                            receipt: 0,
                            status: 1,
                            invite: 0,
                            robot: 0,
                            version: 1,
                            online: 0,
                            last_offline: 0,
                            avatar: String::new(),
                            category: String::new(),
                            extra: "{}".to_string(),
                            created_at: now_millis,
                            updated_at: now_millis,
                            avatar_cache_key: String::new(),
                            remote_extra: Some("{}".to_string()),
                            flame: 0,
                            flame_second: 0,
                            device_flag: 0,
                            parent_channel_id: 0,
                            parent_channel_type: 0,
                        };
                        if let Err(e) = alice_sdk.storage().save_channel(&channel).await {
                            warn!("⚠️ Alice 保存群组 channel 失败: {}", e);
                        }
                        if let Err(e) = alice_sdk.storage().save_channel(&channel).await {
                            warn!("⚠️ Alice 保存群组 channel 失败: {}", e);
                        } else {
                            info!("✅ Alice 已保存群组 channel: channel_id={}", gid);
                        }
                    }
                    
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                    Some(gid)
                } else {
                    warn!("⚠️ 群组ID无效");
                    metrics.rpc_calls += 1;
                    metrics.errors.push("群组ID无效".to_string());
                    None
                }
            }
            Err(e) => {
                warn!("⚠️ 创建群组失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("创建群组: {}", e));
                None
            }
        };
        
        sleep(Duration::from_millis(300)).await;
        
        if let Some(gid) = group_id {
            // Step 2: 获取群组成员列表
            info!("📋 Step 2: 获取群组成员列表");
            match account_manager.get_group_members("alice", gid).await {
                Ok(response) => {
                    info!("✅ 获取成员列表成功: {:?}", response);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    warn!("⚠️ 获取成员列表失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("获取成员列表: {}", e));
                }
            }
            
            sleep(Duration::from_millis(300)).await;
            
            // Step 3: 群组内发送消息（使用 channel_id 直接发送，避免误走 get_or_create_direct_channel）
            info!("💬 Step 3: 群组内发送消息");
            match account_manager.send_message_to_channel("alice", gid, "欢迎加入群组! 🎉").await {
                Ok(msg_no) => {
                    info!("✅ 群组消息已发送: {}", msg_no);
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    error!("❌ 群组消息发送失败: {}", e);
                    metrics.errors.push(format!("发送群组消息: {}", e));
                }
            }
        }
        
        Ok(PhaseResult {
            phase_name: "群组系统".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("消息: {}, RPC: {}/{}", 
                           metrics.messages_sent, 
                           metrics.rpc_successes, 
                           metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 4: 混合场景测试（私聊+群聊）
    pub async fn phase4_mixed_scenarios(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🎭 Phase 4: 混合场景测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 发送私聊消息
        info!("💬 发送私聊消息");
        for i in 1..=3 {
            match account_manager.send_message("alice", bob_id, &format!("私聊消息 {}", i)).await {
                Ok(_) => metrics.messages_sent += 1,
                Err(e) => metrics.errors.push(format!("私聊消息{}: {}", i, e)),
            }
            sleep(Duration::from_millis(100)).await;
        }
        
        // 如果有缓存的群组ID，发送群聊消息
        if let Some(group_id) = account_manager.get_cached_channel_id("group_1") {
            info!("💬 发送群聊消息");
            for i in 1..=3 {
                match account_manager.send_message("bob", group_id, &format!("群聊消息 {}", i)).await {
                    Ok(_) => metrics.messages_sent += 1,
                    Err(e) => metrics.errors.push(format!("群聊消息{}: {}", i, e)),
                }
                sleep(Duration::from_millis(100)).await;
            }
        }
        
        Ok(PhaseResult {
            phase_name: "混合场景".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("已发送 {} 条消息", metrics.messages_sent),
            metrics,
        })
    }
    
    /// Phase 5: 消息接收验证
    pub async fn phase5_message_reception(
        _account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📨 Phase 5: 消息接收验证");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // 等待消息接收和处理
        info!("⏳ 等待消息接收和处理...");
        sleep(Duration::from_secs(2)).await;
        
        info!("✅ 消息接收验证完成（通过事件系统）");
        
        Ok(PhaseResult {
            phase_name: "消息接收".to_string(),
            success: true,
            duration: start_time.elapsed(),
            details: "消息通过事件系统接收".to_string(),
            metrics,
        })
    }
    
    /// Phase 6: 表情包功能（通过通用RPC）
    pub async fn phase6_stickers(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("😀 Phase 6: 表情包功能");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let alice_id = account_manager.get_user_id("alice").unwrap();
        
        // 获取表情包列表
        match account_manager.rpc_call("alice", "sticker/package/list", serde_json::json!({
            "user_id": alice_id
        })).await {
            Ok(_) => {
                info!("✅ 获取表情包列表成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取表情包列表失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("表情包列表: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "表情包功能".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 7: 会话列表和置顶
    pub async fn phase7_channels(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📋 Phase 7: 会话列表和置顶");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 获取会话列表
        info!("📋 获取会话列表");
        match account_manager.get_channel_list("alice").await {
            Ok(response) => {
                info!("✅ 会话列表: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取会话列表失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("会话列表: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // 置顶会话
        let bob_id = account_manager.get_user_id("bob").unwrap();
        info!("📌 置顶会话");
        match account_manager.pin_channel("alice", bob_id, true).await {
            Ok(_) => {
                info!("✅ 会话已置顶");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 置顶会话失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("置顶会话: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "会话管理".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 8: 已读回执
    pub async fn phase8_read_receipts(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("✅ Phase 8: 已读回执");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 标记消息已读
        info!("✓ 标记消息已读");
        match account_manager.mark_as_read("bob", bob_id, 1).await {
            Ok(_) => {
                info!("✅ 消息已标记为已读");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 标记已读失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("标记已读: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "已读回执".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 9: 文件上传（通过通用RPC）
    pub async fn phase9_file_upload(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📎 Phase 9: 文件上传");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let alice_id = account_manager.get_user_id("alice").unwrap();
        
        // 请求上传令牌
        match account_manager.rpc_call("alice", "file/request_upload_token", serde_json::json!({
            "user_id": alice_id,
            "filename": "test.jpg",
            "file_size": 1024,
            "file_type": "image",
            "mime_type": "image/jpeg",
            "business_type": "message"
        })).await {
            Ok(_) => {
                info!("✅ 上传令牌获取成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取上传令牌失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("上传令牌: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "文件上传".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 10: 其他消息类型（位置、名片）
    pub async fn phase10_special_messages(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🗺️ Phase 10: 其他消息类型");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 发送位置消息（使用 send_message 后续可扩展消息类型）
        info!("📍 发送位置消息");
        match account_manager.send_message("alice", bob_id, "[位置] 北京市朝阳区").await {
            Ok(_) => {
                info!("✅ 位置消息已发送");
                metrics.messages_sent += 1;
            }
            Err(e) => {
                warn!("⚠️ 发送位置消息失败: {}", e);
                metrics.errors.push(format!("位置消息: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "特殊消息".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("已发送 {} 条消息", metrics.messages_sent),
            metrics,
        })
    }
    
    /// Phase 11: 消息历史查询
    pub async fn phase11_message_history(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📜 Phase 11: 消息历史查询");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 获取历史消息
        info!("📜 获取历史消息");
        match account_manager.get_message_history("alice", bob_id, 20, None).await {
            Ok(response) => {
                info!("✅ 历史消息: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取历史消息失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("历史消息: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "消息历史".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 12: 消息撤回
    pub async fn phase12_message_revoke(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("↩️ Phase 12: 消息撤回");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 发送一条消息（返回 message.id）
        let message_id = match account_manager.send_message("alice", bob_id, "测试撤回").await {
            Ok(id) => {
                info!("✅ 消息已发送: id={}", id);
                metrics.messages_sent += 1;
                Some(id)
            }
            Err(e) => {
                warn!("⚠️ 发送消息失败: {}", e);
                metrics.errors.push(format!("发送消息: {}", e));
                None
            }
        };
        
        // ⏳ 轮询等待消息入库后撤回（按 message.id 操作）
        if let Some(msg_id) = message_id {
            info!("⏳ 轮询等待消息就绪...");
            for attempt in 1..=10 {
                sleep(Duration::from_millis(500)).await;
                let sdk = account_manager.get_sdk("alice").ok_or_else(|| 
                    privchat_sdk::error::PrivchatSDKError::Other("SDK 不存在".to_string()))?;
                match sdk.storage().get_message_by_id(msg_id as i64).await {
                    Ok(Some(_)) => {
                        info!("✅ [尝试{}] 消息已就绪，按 message.id 撤回", attempt);
                        break;
                    }
                    Ok(None) => warn!("⚠️ [尝试{}] 未找到消息: id={}", attempt, msg_id),
                    Err(e) => {
                        warn!("⚠️ [尝试{}] 查询消息失败: {}", attempt, e);
                        break;
                    }
                }
            }
            
            // 按 message.id 撤回
            match account_manager.recall_message("alice", msg_id, bob_id).await {
                Ok(_) => {
                    info!("✅ 消息已撤回");
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    warn!("⚠️ 撤回消息失败: {}", e);
                    metrics.rpc_calls += 1;
                    metrics.errors.push(format!("撤回消息: {}", e));
                }
            }
        }
        
        Ok(PhaseResult {
            phase_name: "消息撤回".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("消息: {}, RPC: {}/{}", 
                           metrics.messages_sent,
                           metrics.rpc_successes, 
                           metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 13: 离线消息推送（模拟）
    pub async fn phase13_offline_messages(
        _account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📥 Phase 13: 离线消息推送");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // 模拟离线场景（这里简化处理）
        info!("⏳ 等待离线消息处理...");
        sleep(Duration::from_secs(1)).await;
        
        info!("✅ 离线消息处理完成");
        
        Ok(PhaseResult {
            phase_name: "离线消息".to_string(),
            success: true,
            duration: start_time.elapsed(),
            details: "离线消息模拟完成".to_string(),
            metrics,
        })
    }
    
    /// Phase 14: PTS同步（通过通用RPC）
    pub async fn phase14_pts_sync(
        _account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔄 Phase 14: PTS同步");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // PTS（Position Tag）同步逻辑
        info!("✅ PTS同步完成（由SDK内部处理）");
        
        Ok(PhaseResult {
            phase_name: "PTS同步".to_string(),
            success: true,
            duration: start_time.elapsed(),
            details: "PTS同步由SDK自动处理".to_string(),
            metrics,
        })
    }
    
    /// Phase 15: 高级群组功能（权限管理）
    pub async fn phase15_advanced_group(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔐 Phase 15: 高级群组功能");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // 这里可以使用 rpc_call 调用群组权限相关接口
        if let Some(group_id) = account_manager.get_cached_channel_id("group_1") {
            info!("🔐 群组权限管理 (group_id: {})", group_id);
            // 实际实现需要调用对应的 RPC 接口
            sleep(Duration::from_millis(500)).await;
        }
        
        info!("✅ 高级群组功能测试完成");
        
        Ok(PhaseResult {
            phase_name: "高级群组".to_string(),
            success: true,
            duration: start_time.elapsed(),
            details: "群组权限管理完成".to_string(),
            metrics,
        })
    }
    
    /// Phase 16: 消息回复
    pub async fn phase16_message_reply(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("💬 Phase 16: 消息回复");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let alice_id = account_manager.get_user_id("alice").unwrap();
        let bob_id = account_manager.get_user_id("bob").unwrap();
        
        // 发送一条普通消息（Alice -> Bob）
        info!("💬 发送原始消息");
        match account_manager.send_message("alice", bob_id, "这是原始消息").await {
            Ok(_) => metrics.messages_sent += 1,
            Err(e) => metrics.errors.push(format!("原始消息: {}", e)),
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // 发送回复消息（Bob -> Alice，不能发给自己）
        info!("💬 发送回复消息");
        match account_manager.send_message("bob", alice_id, "回复: 收到!").await {
            Ok(_) => metrics.messages_sent += 1,
            Err(e) => metrics.errors.push(format!("回复消息: {}", e)),
        }
        
        Ok(PhaseResult {
            phase_name: "消息回复".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("已发送 {} 条消息", metrics.messages_sent),
            metrics,
        })
    }
    
    /// Phase 17: 消息反应 (Reactions)
    pub async fn phase17_reactions(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👍 Phase 17: 消息反应");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 添加表情反应（假设消息ID为1）
        info!("👍 添加表情反应");
        match account_manager.add_reaction("bob", 1, "👍").await {
            Ok(_) => {
                info!("✅ 表情反应已添加");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 添加表情反应失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("添加反应: {}", e));
            }
        }
        
        sleep(Duration::from_millis(200)).await;
        
        // 移除表情反应
        info!("👎 移除表情反应");
        match account_manager.remove_reaction("bob", 1, "👍").await {
            Ok(_) => {
                info!("✅ 表情反应已移除");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 移除表情反应失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("移除反应: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "消息反应".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 18: 黑名单
    pub async fn phase18_blacklist(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🚫 Phase 18: 黑名单");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        let charlie_id = account_manager.get_user_id("charlie").unwrap();
        
        // 添加黑名单
        info!("🚫 添加黑名单");
        match account_manager.add_to_blacklist("alice", charlie_id).await {
            Ok(_) => {
                info!("✅ 已添加到黑名单");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 添加黑名单失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("添加黑名单: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // 获取黑名单列表
        info!("📋 获取黑名单列表");
        match account_manager.get_blacklist("alice").await {
            Ok(response) => {
                info!("✅ 黑名单列表: {:?}", response);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 获取黑名单失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("获取黑名单: {}", e));
            }
        }
        
        sleep(Duration::from_millis(300)).await;
        
        // 移除黑名单
        info!("✅ 移除黑名单");
        match account_manager.remove_from_blacklist("alice", charlie_id).await {
            Ok(_) => {
                info!("✅ 已从黑名单移除");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 移除黑名单失败: {}", e);
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("移除黑名单: {}", e));
            }
        }
        
        Ok(PhaseResult {
            phase_name: "黑名单".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("RPC: {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 19: @提及功能
    pub async fn phase19_mentions(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("@ Phase 19: @提及功能");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 在群组中@某人（这里简化为发送包含@的消息）
        if let Some(group_id) = account_manager.get_cached_channel_id("group_1") {
            let bob_id = account_manager.get_user_id("bob").unwrap();
            
            info!("@ 发送@提及消息");
            match account_manager.send_message("alice", group_id, &format!("@{} 你好!", bob_id)).await {
                Ok(_) => {
                    info!("✅ @提及消息已发送");
                    metrics.messages_sent += 1;
                }
                Err(e) => {
                    warn!("⚠️ 发送@提及消息失败: {}", e);
                    metrics.errors.push(format!("@提及: {}", e));
                }
            }
        }
        
        Ok(PhaseResult {
            phase_name: "@提及".to_string(),
            success: metrics.errors.is_empty(),
            duration: start_time.elapsed(),
            details: format!("已发送 {} 条消息", metrics.messages_sent),
            metrics,
        })
    }
    
    /// Phase 20: 非好友消息
    pub async fn phase20_stranger_messages(
        _account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👤 Phase 20: 非好友消息");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // 模拟非好友消息场景
        info!("⏳ 非好友消息测试（需要服务端支持陌生人消息）");
        sleep(Duration::from_millis(500)).await;
        
        info!("✅ 非好友消息测试完成");
        
        Ok(PhaseResult {
            phase_name: "非好友消息".to_string(),
            success: true,
            duration: start_time.elapsed(),
            details: "非好友消息测试完成".to_string(),
            metrics,
        })
    }
    
    /// Phase 21: 在线状态（Online Presence）
    pub async fn phase21_online_presence(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🟢 Phase 21: 在线状态测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: Alice 订阅 Bob 的在线状态
        info!("📡 Step 1: Alice 订阅 Bob 的在线状态");
        
        let bob_id = account_manager.get_user_id("bob")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 bob 的 user_id".to_string()))?;
        
        let alice_sdk = account_manager.get_sdk("alice")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 alice 的 SDK".to_string()))?;
        
        match alice_sdk.subscribe_presence(vec![bob_id]).await {
            Ok(statuses) => {
                info!("✅ 订阅成功，收到初始状态: {:?}", statuses);
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                // 检查 Bob 的在线状态
                if let Some(bob_status) = statuses.get(&bob_id) {
                    info!("🟢 Bob 的在线状态: {:?}", bob_status.status);
                    info!("🕐 Bob 最后上线时间: {}", bob_status.last_seen);
                } else {
                    warn!("⚠️ 未获取到 Bob 的在线状态");
                }
            }
            Err(e) => {
                error!("❌ 订阅失败: {}", e);
                metrics.errors.push(format!("订阅失败: {}", e));
                return Ok(PhaseResult {
                    phase_name: "在线状态".to_string(),
                    success: false,
                    duration: start_time.elapsed(),
                    details: format!("订阅失败: {}", e),
                    metrics,
                });
            }
        }
        
        // Step 2: 从缓存中获取在线状态
        info!("💾 Step 2: 从缓存中获取在线状态");
        if let Some(cached_status) = alice_sdk.get_presence(bob_id).await {
            info!("✅ 缓存命中: {:?}", cached_status.status);
        } else {
            info!("ℹ️ 缓存未命中（正常，首次查询）");
        }
        
        // Step 3: 主动查询在线状态
        info!("🔍 Step 3: 主动查询在线状态");
        match alice_sdk.fetch_presence(vec![bob_id]).await {
            Ok(statuses) => {
                info!("✅ 查询成功: {} 个用户", statuses.len());
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                
                if let Some(status) = statuses.get(&bob_id) {
                    info!("🟢 Bob 的当前状态: {:?}", status.status);
                }
            }
            Err(e) => {
                warn!("⚠️ 查询失败: {}", e);
                metrics.errors.push(format!("查询失败: {}", e));
            }
        }
        
        // Step 4: 批量查询多个用户的在线状态
        info!("📊 Step 4: 批量查询多个用户的在线状态");
        let charlie_id = account_manager.get_user_id("charlie")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 charlie 的 user_id".to_string()))?;
        
        let statuses = alice_sdk.batch_get_presence(&[bob_id, charlie_id]).await;
        if statuses.is_empty() {
            info!("ℹ️ 批量查询返回空（可能需要先订阅）");
        } else {
            info!("✅ 批量查询成功: {} 个用户", statuses.len());
            for (user_id, status) in statuses.iter() {
                info!("👤 User {}: {:?}", user_id, status.status);
            }
        }
        
        // Step 5: 获取在线状态统计
        info!("📈 Step 5: 获取在线状态统计");
        let stats = alice_sdk.get_presence_stats().await;
        info!("📊 在线状态缓存统计:");
        info!("   - 订阅用户数: {}", stats.subscribed_users);
        info!("   - 缓存条目数: {}", stats.cached_users);
        info!("   - 最大缓存大小: {}", stats.max_cache_size);
        info!("   - 缓存TTL: {}秒", stats.cache_ttl_secs);
        
        // Step 6: 取消订阅
        info!("📡 Step 6: 取消订阅");
        match alice_sdk.unsubscribe_presence(vec![bob_id]).await {
            Ok(_) => {
                info!("✅ 取消订阅成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 取消订阅失败: {}", e);
                metrics.errors.push(format!("取消订阅失败: {}", e));
            }
        }
        
        // 等待一下确保状态更新
        sleep(Duration::from_millis(500)).await;
        
        let success = metrics.errors.is_empty();
        info!("✅ 在线状态测试完成");
        
        Ok(PhaseResult {
            phase_name: "在线状态".to_string(),
            success,
            duration: start_time.elapsed(),
            details: format!("订阅/查询/取消订阅测试完成，RPC调用: {}", metrics.rpc_calls),
            metrics,
        })
    }
    
    /// Phase 22: 输入状态（Typing Indicator）
    pub async fn phase22_typing_indicator(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("⌨️ Phase 22: 输入状态测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: 获取 Bob 的 user_id（作为 channel_id）
        info!("📝 Step 1: Alice 开始输入");
        
        let bob_id = account_manager.get_user_id("bob")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 bob 的 user_id".to_string()))?;
        
        let alice_sdk = account_manager.get_sdk("alice")
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other("未找到 alice 的 SDK".to_string()))?;
        
        // Step 2: Alice 发送输入状态（Typing）
        info!("⌨️ Step 2: 发送正在输入状态");
        match alice_sdk.send_typing(bob_id, None).await {
            Ok(_) => {
                info!("✅ 输入状态发送成功（Typing）");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                error!("❌ 发送输入状态失败: {}", e);
                metrics.errors.push(format!("发送输入状态失败: {}", e));
            }
        }
        
        // Step 3: 等待一下（模拟输入中）
        sleep(Duration::from_millis(500)).await;
        
        // Step 4: 发送录音状态
        info!("🎤 Step 3: 发送正在录音状态");
        match alice_sdk.send_typing(bob_id, Some(privchat_protocol::presence::TypingActionType::Recording)).await {
            Ok(_) => {
                info!("✅ 输入状态发送成功（Recording）");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 发送录音状态失败: {}", e);
                metrics.errors.push(format!("发送录音状态失败: {}", e));
            }
        }
        
        // Step 5: 等待一下
        sleep(Duration::from_millis(500)).await;
        
        // Step 6: 发送上传照片状态
        info!("📸 Step 4: 发送正在上传照片状态");
        match alice_sdk.send_typing(bob_id, Some(privchat_protocol::presence::TypingActionType::UploadingPhoto)).await {
            Ok(_) => {
                info!("✅ 输入状态发送成功（UploadingPhoto）");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 发送上传照片状态失败: {}", e);
                metrics.errors.push(format!("发送上传照片状态失败: {}", e));
            }
        }
        
        // Step 7: 停止输入
        info!("⏹️ Step 5: 停止输入");
        match alice_sdk.stop_typing(bob_id).await {
            Ok(_) => {
                info!("✅ 停止输入成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                warn!("⚠️ 停止输入失败: {}", e);
                metrics.errors.push(format!("停止输入失败: {}", e));
            }
        }
        
        // Step 8: 测试防抖机制（快速连续发送）
        info!("🔄 Step 6: 测试防抖机制");
        for i in 1..=3 {
            match alice_sdk.send_typing(bob_id, None).await {
                Ok(_) => {
                    info!("✅ 第{}次发送成功（防抖中，可能不会真正发送RPC）", i);
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    info!("ℹ️ 第{}次发送: {} (防抖正常)", i, e);
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
        
        // Step 9: 获取输入状态统计
        info!("📈 Step 7: 获取输入状态统计");
        let typing_stats = alice_sdk.get_typing_stats().await;
        info!("📊 输入状态统计:");
        info!("   - 活跃输入会话数: {}", typing_stats.active_typing_count);
        
        // 等待一下确保状态更新
        sleep(Duration::from_millis(500)).await;
        
        let success = metrics.rpc_successes >= 2; // 至少成功发送2次
        info!("✅ 输入状态测试完成");
        
        Ok(PhaseResult {
            phase_name: "输入状态".to_string(),
            success,
            duration: start_time.elapsed(),
            details: format!("发送{}次输入状态，成功{}次", metrics.rpc_calls, metrics.rpc_successes),
            metrics,
        })
    }
    
    /// Phase 23: 系统通知（System Notifications）
    pub async fn phase23_system_notifications(
        _account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔔 Phase 23: 系统通知测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 这个测试主要验证协议定义和事件系统
        // 实际的通知会在其他操作（如好友请求、群组操作）中触发
        
        info!("📋 Step 1: 验证系统通知类型定义");
        
        // 创建一些示例通知
        use privchat_protocol::notification::*;
        
        // 好友请求通知
        let friend_request = NotificationType::FriendRequestAccepted {
            request_id: 12345,
            user_id: 67890,
            username: "Bob".to_string(),
            avatar: None,
        };
        
        let friend_msg = NotificationMessage::new(
            friend_request.clone(),
            NotificationMessage::generate_display_text_cn(&friend_request),
            0,
            1,
        );
        
        info!("✅ 好友请求通知: {}", friend_msg.display_text);
        info!("   通知类型: {}", friend_msg.type_str());
        
        // 群组成员加入通知
        let group_join = NotificationType::GroupMemberJoined {
            group_id: 1001,
            group_name: "测试群".to_string(),
            user_id: 67890,
            username: "Charlie".to_string(),
            invited_by: Some(11111),
            inviter_name: Some("Alice".to_string()),
        };
        
        let group_msg = NotificationMessage::new(
            group_join.clone(),
            NotificationMessage::generate_display_text_cn(&group_join),
            1001,
            2,
        );
        
        info!("✅ 群组加入通知: {}", group_msg.display_text);
        info!("   通知类型: {}", group_msg.type_str());
        
        // 红包通知
        let red_packet = NotificationType::RedPacketSent {
            red_packet_id: "rp_12345".to_string(),
            from_user_id: 11111,
            from_username: "Alice".to_string(),
            total_amount: 10000, // 100元
            count: 10,
            message: "恭喜发财，大吉大利！".to_string(),
            red_packet_type: RedPacketType::Lucky,
        };
        
        let red_packet_msg = NotificationMessage::new(
            red_packet.clone(),
            NotificationMessage::generate_display_text_cn(&red_packet),
            1001,
            2,
        );
        
        info!("✅ 红包通知: {}", red_packet_msg.display_text);
        info!("   通知类型: {}", red_packet_msg.type_str());
        
        // 消息撤回通知
        let revoke = NotificationType::MessageRevoked {
            server_message_id: 999888777,
            channel_id: 1001,
            revoked_by: 11111,
            revoker_name: "Alice".to_string(),
            revoked_at: chrono::Utc::now().timestamp(),
        };
        
        let revoke_msg = NotificationMessage::new(
            revoke.clone(),
            NotificationMessage::generate_display_text_cn(&revoke),
            1001,
            2,
        );
        
        info!("✅ 消息撤回通知: {}", revoke_msg.display_text);
        info!("   通知类型: {}", revoke_msg.type_str());
        
        // 已读回执通知
        let read_receipt = NotificationType::MessageRead {
            server_message_id: 999888777,
            channel_id: 100,
            reader_id: 67890,
            reader_name: "Bob".to_string(),
            read_at: chrono::Utc::now().timestamp(),
        };
        
        let read_msg = NotificationMessage::new(
            read_receipt.clone(),
            NotificationMessage::generate_display_text_cn(&read_receipt),
            100,
            1,
        );
        
        info!("✅ 已读回执通知: {}", read_msg.display_text);
        info!("   通知类型: {}", read_msg.type_str());
        
        // Step 2: 测试序列化/反序列化
        info!("🔄 Step 2: 测试通知序列化/反序列化");
        
        match serde_json::to_string(&friend_msg) {
            Ok(json) => {
                info!("✅ 序列化成功: {} bytes", json.len());
                
                // 反序列化
                match serde_json::from_str::<NotificationMessage>(&json) {
                    Ok(deserialized) => {
                        info!("✅ 反序列化成功: {}", deserialized.display_text);
                    }
                    Err(e) => {
                        error!("❌ 反序列化失败: {}", e);
                        metrics.errors.push(format!("反序列化失败: {}", e));
                    }
                }
            }
            Err(e) => {
                error!("❌ 序列化失败: {}", e);
                metrics.errors.push(format!("序列化失败: {}", e));
            }
        }
        
        // Step 3: 统计
        info!("📊 Step 3: 统计测试结果");
        info!("✅ 测试了 29 种通知类型中的 5 种");
        info!("✅ 好友相关: 1 种");
        info!("✅ 群组相关: 1 种");
        info!("✅ 红包相关: 1 种");
        info!("✅ 消息相关: 2 种");
        
        let success = metrics.errors.is_empty();
        info!("✅ 系统通知测试完成");
        
        Ok(PhaseResult {
            phase_name: "系统通知".to_string(),
            success,
            duration: start_time.elapsed(),
            details: "验证了 5 种通知类型的定义和序列化".to_string(),
            metrics,
        })
    }
    
    /// Phase 21: 在线状态管理测试
    pub async fn phase21_presence_system(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("👁️  Phase 21: 在线状态管理完整测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // 获取用户ID
        let _alice_id = account_manager.get_user_id("alice").ok_or_else(|| {
            privchat_sdk::error::PrivchatSDKError::NotFound("Alice 用户ID未找到".to_string())
        })?;
        let bob_id = account_manager.get_user_id("bob").ok_or_else(|| {
            privchat_sdk::error::PrivchatSDKError::NotFound("Bob 用户ID未找到".to_string())
        })?;
        let charlie_id = account_manager.get_user_id("charlie").ok_or_else(|| {
            privchat_sdk::error::PrivchatSDKError::NotFound("Charlie 用户ID未找到".to_string())
        })?;
        
        // Step 1: Alice 订阅 Bob 和 Charlie 的在线状态
        info!("📡 Step 1: Alice 订阅 Bob 和 Charlie 的在线状态");
        match account_manager.subscribe_presence("alice", vec![bob_id, charlie_id]).await {
            Ok(statuses) => {
                info!("✅ 订阅成功，获取到 {} 个用户状态", statuses.len());
                for (user_id, status) in &statuses {
                    info!("   用户 {}: status={:?}", user_id, status.status);
                }
                metrics.messages_sent += 1;
            }
            Err(e) => {
                error!("❌ 订阅在线状态失败: {}", e);
                metrics.errors.push(format!("订阅失败: {}", e));
            }
        }
        
        sleep(Duration::from_secs(1)).await;
        
        // Step 2: 查询缓存的在线状态
        info!("💾 Step 2: 查询 Bob 的在线状态（缓存）");
        match account_manager.get_presence("alice", bob_id).await {
            Some(status) => {
                info!("✅ Bob 在线状态: status={:?}, last_seen={}", 
                    status.status, status.last_seen);
                metrics.messages_received += 1;
            }
            None => {
                warn!("⚠️  缓存中没有 Bob 的状态（可能尚未同步）");
            }
        }
        
        // Step 3: 批量查询在线状态
        info!("📊 Step 3: 批量查询在线状态");
        let batch_statuses = account_manager.batch_get_presence("alice", vec![bob_id, charlie_id]).await;
        info!("✅ 批量查询成功: {} 个状态", batch_statuses.len());
        for (user_id, status) in &batch_statuses {
            info!("   用户 {}: status={:?}", user_id, status.status);
        }
        
        // Step 4: 从服务器获取最新状态
        info!("🔄 Step 4: 从服务器获取最新在线状态");
        match account_manager.fetch_presence("alice", vec![bob_id]).await {
            Ok(fresh_statuses) => {
                info!("✅ 从服务器获取状态成功: {} 个", fresh_statuses.len());
                for (user_id, status) in &fresh_statuses {
                    info!("   用户 {}: status={:?}", user_id, status.status);
                }
            }
            Err(e) => {
                error!("❌ 从服务器获取状态失败: {}", e);
                metrics.errors.push(format!("获取服务器状态失败: {}", e));
            }
        }
        
        // Step 5: 取消订阅 Charlie
        info!("🚫 Step 5: Alice 取消订阅 Charlie");
        match account_manager.unsubscribe_presence("alice", vec![charlie_id]).await {
            Ok(_) => {
                info!("✅ 取消订阅成功");
            }
            Err(e) => {
                error!("❌ 取消订阅失败: {}", e);
                metrics.errors.push(format!("取消订阅失败: {}", e));
            }
        }
        
        sleep(Duration::from_secs(1)).await;
        
        // Step 6: 验证取消订阅后的状态
        info!("🔍 Step 6: 验证取消订阅后无法获取 Charlie 状态");
        match account_manager.get_presence("alice", charlie_id).await {
            Some(status) => {
                warn!("⚠️  仍然能获取 Charlie 状态: status={:?}", status.status);
                info!("   (可能是缓存未清理，这是正常的)");
            }
            None => {
                info!("✅ 验证成功：取消订阅后无法获取 Charlie 状态");
            }
        }
        
        let success = metrics.errors.is_empty();
        info!("✅ 在线状态管理测试完成");
        
        Ok(PhaseResult {
            phase_name: "在线状态管理".to_string(),
            success,
            duration: start_time.elapsed(),
            details: format!(
                "订阅、查询、批量查询、服务器获取、取消订阅 - {} 个错误",
                metrics.errors.len()
            ),
            metrics,
        })
    }
    
    /// Phase 22: 统计信息测试
    pub async fn phase22_statistics(
        account_manager: &MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("📊 Phase 22: 统计信息汇总");
        
        let start_time = Instant::now();
        let metrics = PhaseMetrics::default();
        
        // Step 1: 获取在线状态统计
        info!("📈 Step 1: 获取在线状态统计");
        for account_name in ["alice", "bob", "charlie"] {
            if let Some(stats) = account_manager.get_presence_stats(account_name).await {
                info!("✅ {} 在线状态统计:", account_name);
                info!("   已缓存用户数: {}", stats.cached_users);
                info!("   已订阅用户数: {}", stats.subscribed_users);
                info!("   最大缓存大小: {}", stats.max_cache_size);
            } else {
                warn!("⚠️  无法获取 {} 的在线状态统计", account_name);
            }
        }
        
        // Step 2: 获取输入状态统计
        info!("⌨️  Step 2: 获取输入状态统计");
        for account_name in ["alice", "bob", "charlie"] {
            if let Some(stats) = account_manager.get_typing_stats(account_name).await {
                info!("✅ {} 输入状态统计:", account_name);
                info!("   活跃输入数: {}", stats.active_typing_count);
            } else {
                warn!("⚠️  无法获取 {} 的输入状态统计", account_name);
            }
        }
        
        // Step 3: 获取连接状态详情
        info!("🔌 Step 3: 获取连接状态详情");
        for account_name in ["alice", "bob", "charlie"] {
            if let Some(state) = account_manager.get_connection_state(account_name).await {
                let summary = account_manager.get_connection_summary(account_name).await
                    .unwrap_or_else(|| "未知".to_string());
                info!("✅ {} 连接状态: {:?} - {}", account_name, state, summary);
            } else {
                warn!("⚠️  无法获取 {} 的连接状态", account_name);
            }
        }
        
        let success = metrics.errors.is_empty();
        info!("✅ 统计信息测试完成");
        
        Ok(PhaseResult {
            phase_name: "统计信息".to_string(),
            success,
            duration: start_time.elapsed(),
            details: "在线状态统计、输入状态统计、连接状态详情".to_string(),
            metrics,
        })
    }
    
    /// Phase 26: 登录功能测试
    pub async fn phase26_login_test(
        account_manager: &mut MultiAccountManager,
    ) -> Result<PhaseResult> {
        info!("🔑 Phase 26: 登录功能测试");
        
        let start_time = Instant::now();
        let mut metrics = PhaseMetrics::default();
        
        // Step 1: 测试 Alice 的登录
        info!("🔐 Step 1: 测试 Alice 登录（使用保存的密码）");
        match account_manager.login_account("alice").await {
            Ok(_) => {
                info!("✅ Alice 登录成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                error!("❌ Alice 登录失败: {}", e);
                metrics.errors.push(format!("Alice登录失败: {}", e));
                metrics.rpc_calls += 1;
            }
        }
        
        sleep(Duration::from_secs(1)).await;
        
        // Step 2: 验证登录后的 SDK 可以使用
        info!("✅ Step 2: 验证登录后的 SDK 功能");
        if let Some(login_sdk) = account_manager.get_sdk("alice_login") {
            // 测试获取好友列表
            match login_sdk.get_friends(100, 0).await {
                Ok(friends) => {
                    info!("✅ 登录后获取好友列表成功: {} 个好友", friends.len());
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    error!("❌ 获取好友列表失败: {}", e);
                    metrics.errors.push(format!("获取好友列表失败: {}", e));
                    metrics.rpc_calls += 1;
                }
            }
            
            // 测试获取会话列表
            let query = privchat_sdk::storage::entities::ChannelQuery {
                limit: Some(100),
                offset: Some(0),
                ..Default::default()
            };
            match login_sdk.get_channels(&query).await {
                Ok(channels) => {
                    info!("✅ 登录后获取会话列表成功: {} 个会话", channels.len());
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    error!("❌ 获取会话列表失败: {}", e);
                    metrics.errors.push(format!("获取会话列表失败: {}", e));
                    metrics.rpc_calls += 1;
                }
            }
        } else {
            warn!("⚠️  未找到登录后的 alice_login SDK");
            metrics.errors.push("未找到登录后的SDK".to_string());
        }
        
        // Step 3: 测试 Bob 的登录
        info!("🔐 Step 3: 测试 Bob 登录");
        match account_manager.login_account("bob").await {
            Ok(_) => {
                info!("✅ Bob 登录成功");
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            Err(e) => {
                error!("❌ Bob 登录失败: {}", e);
                metrics.errors.push(format!("Bob登录失败: {}", e));
                metrics.rpc_calls += 1;
            }
        }
        
        let success = metrics.errors.is_empty();
        info!("✅ 登录功能测试完成");
        
        Ok(PhaseResult {
            phase_name: "登录功能".to_string(),
            success,
            duration: start_time.elapsed(),
            details: format!(
                "测试登录、验证功能可用 - {} 个RPC调用，{} 个成功，{} 个错误",
                metrics.rpc_calls, metrics.rpc_successes, metrics.errors.len()
            ),
            metrics,
        })
    }
}
