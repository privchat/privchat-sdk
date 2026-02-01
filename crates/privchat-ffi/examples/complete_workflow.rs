//! 完整工作流程示例 - 展示所有48个API
//! 
//! 本示例注册单个账号，展示所有可用的 API：
//! 
//! ✅ 已实现的完整功能（48个API）:
//!    [账号管理] register, login, authenticate, current_user_id
//!    [连接管理] connect, disconnect, is_connected, connection_state, shutdown
//!    [消息管理] send, mark_read, history, search, edit, revoke, add_reaction, 
//!              remove_reaction, forward
//!    [实体同步] run_bootstrap_sync（auth 成功后必须调用一次，内部决定全量/增量）
//!    [会话管理] get_channels, get_channel_list, mark_channel_read,
//!              pin_channel, hide_channel, mute_channel
//!    [好友管理] get_friends, search_users, send_friend_request, accept_friend_request,
//!              reject_friend_request, delete_friend
//!    [群组管理] create_group, invite_to_group, get_group_members, 
//!              remove_group_member, leave_group
//!    [黑名单] add_to_blacklist, remove_from_blacklist, get_blacklist
//!    [在线状态] subscribe_presence, unsubscribe_presence, get_presence,
//!              batch_get_presence, fetch_presence
//!    [输入状态] send_typing, stop_typing
//!    [事件系统] poll_events, pending_events_count, clear_events, 
//!              set_delegate, remove_delegate
//! 
//! 注意：部分功能需要多账号测试，请参考 multi_account 示例

use std::sync::Arc;
use std::time::Duration;
use privchat_ffi::{
    PrivchatConfigBuilder, PrivchatSDK, ServerEndpoint, TransportProtocol,
    AuthResult, ChannelListEntry, FriendEntry, UserEntry,
};
use tokio;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Privchat FFI - 完整工作流程示例");
    println!("====================================\n");
    
    // ===========================================================
    // 步骤 0: 配置SDK（参考 multi_account）
    // ===========================================================
    println!("📝 步骤 0: 配置 SDK");
    
    // SDK 使用 refinery 内置 migrations，无需 assets 目录
    let config = Arc::new(PrivchatConfigBuilder::new())
        .data_dir("/tmp/privchat_complete_test".to_string())
        // 添加多个端点（按优先级）
        .server_endpoint(ServerEndpoint {
            protocol: TransportProtocol::Quic,
            host: "127.0.0.1".to_string(),
            port: 8082,
            path: None,
            use_tls: false,
        })
        .server_endpoint(ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 8080,
            path: None,
            use_tls: false,
        })
        .server_endpoint(ServerEndpoint {
            protocol: TransportProtocol::WebSocket,
            host: "127.0.0.1".to_string(),
            port: 8081,
            path: Some("/".to_string()),
            use_tls: false,
        })
        .connection_timeout(30)
        .heartbeat_interval(30)
        .debug_mode(true)
        .build()?;
    
    let sdk = Arc::new(PrivchatSDK::new(config)?);
    println!("✅ SDK 初始化完成");
    println!("   服务器端点: QUIC:8082, TCP:8080, WebSocket:8081\n");
    
    // ===========================================================
    // 步骤 1: 注册账号（首次使用）
    // ===========================================================
    println!("📝 步骤 1: 注册新账号");
    
    // 生成唯一用户名和 UUID 格式的 device_id
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let username = format!("test_user_{}", timestamp);
    let password = "test_password_123";
    // 生成 UUID v4 格式的 device_id
    let device_id = uuid::Uuid::new_v4().to_string();
    
    println!("   用户名: {}", username);
    println!("   密码: {}", password);
    println!("   设备ID: {}", device_id);
    println!();
    
    // ===========================================================
    // 步骤 1: 连接到服务器（必须在注册前建立网络连接）
    // ===========================================================
    println!("📝 步骤 1: 建立网络连接");
    sdk.clone().connect()?;
    println!("   ✅ 网络连接已建立\n");
    
    // ===========================================================
    // 步骤 2: 注册新账号
    // ===========================================================
    println!("📝 步骤 2: 注册新账号");
    match sdk.register(
        username.clone(),
        password.to_string(),
        device_id.clone(),
    ) {
        Ok(auth_result) => {
            println!("   ✅ 注册成功!");
            println!("   响应: {:?}\n", auth_result);
            
            // 从 AuthResult 获取 user_id 和 token
            let user_id = auth_result.user_id;
            let token = auth_result.token;
            
            println!("   📊 账号信息:");
            println!("      User ID: {}", user_id);
            println!("      Token: {}...", &token[..20.min(token.len())]);
            println!();
            
            // ===========================================================
            // 步骤 3: 认证用户（使用相同的device_id）
            // 注意：注册成功后，SDK 内部已经完成了认证，可能不需要再次调用 authenticate
            // ===========================================================
            println!("📝 步骤 3: 验证认证状态");
            match sdk.authenticate(user_id, token.to_string(), device_id.clone()) {
                Ok(_) => println!("   ✅ 认证成功\n"),
                Err(e) => {
                    println!("   ⚠️  显式认证失败: {:?}", e);
                    println!("   💡 注意: 注册可能已自动完成认证，继续测试其他功能...\n");
                    // 不返回错误，继续执行
                }
            }
            
            // ===========================================================
            // 步骤 3.5: 启动同步（首次登录设备必须强制初始化，类似微信）
            // 若未完成过 Bootstrap 则必须 run_bootstrap_sync() 直到成功；内部全量/增量由 CursorStore 决定
            // ===========================================================
            println!("📝 步骤 3.5: 启动同步 (run_bootstrap_sync)");
            let needs_bootstrap = match sdk.is_bootstrap_completed() {
                Ok(false) => {
                    println!("   ℹ️  首次初始化：未完成过 Bootstrap，必须执行全量同步");
                    true
                }
                Ok(true) => {
                    println!("   ℹ️  已初始化过：本设备已跑过 Bootstrap，跳过阻塞同步（可选后台补齐）");
                    false
                }
                Err(e) => {
                    println!("   ⚠️  检查 Bootstrap 状态失败: {:?}，尝试执行同步", e);
                    true
                }
            };
            if needs_bootstrap {
                match sdk.run_bootstrap_sync() {
                    Ok(()) => println!("   ✅ Bootstrap 同步完成 (Friend → Group → Channel → UserSettings)\n"),
                    Err(e) => {
                        println!("   ⚠️  Bootstrap 同步失败: {:?}", e);
                        println!("   💡 若服务端未实现 entity/sync_entities，可忽略；继续测试...\n");
                    }
                }
            } else {
                sdk.run_bootstrap_sync_in_background();
                println!("   ✅ 已发起后台增量同步 (run_bootstrap_sync_in_background)\n");
            }
            
            // ===========================================================
            // 步骤 4: 获取会话列表
            // ===========================================================
            println!("📝 步骤 4: 获取会话列表");
            match sdk.get_channels(None, None) {
                Ok(channels) => {
                    println!("   📊 会话数量: {}", channels.len());
                    if channels.is_empty() {
                        println!("   ℹ️  暂无会话");
                    } else {
                        for (i, conv) in channels.iter().take(3).enumerate() {
                            println!("   会话 {}: channel_id={}, notifications={}", 
                                i + 1, conv.channel_id, conv.notifications);
                        }
                    }
                }
                Err(e) => {
                    println!("   ⚠️  获取会话列表失败: {:?}", e);
                }
            }
            println!();
            
            // ===========================================================
            // 步骤 5: 获取好友列表
            // ===========================================================
            println!("📝 步骤 5: 获取好友列表");
            match sdk.get_friends(None, None) {
                Ok(friends) => {
                    println!("   📊 好友数量: {}", friends.len());
                    if friends.is_empty() {
                        println!("   ℹ️  暂无好友");
                    } else {
                        for (i, friend) in friends.iter().take(3).enumerate() {
                            println!("   好友 {}: user_id={}, username={}", 
                                i + 1, friend.user_id, friend.username);
                        }
                    }
                }
                Err(e) => {
                    println!("   ⚠️  获取好友列表失败: {:?}", e);
                }
            }
            println!();
            
            // ===========================================================
            // 步骤 6: 搜索用户（演示）
            // ===========================================================
            println!("📝 步骤 6: 搜索用户（演示）");
            println!("   搜索关键词: \"test\"");
            match sdk.search_users("test".to_string()) {
                Ok(results) => {
                    println!("   📊 搜索结果: {} 个用户", results.len());
                    for (i, user) in results.iter().take(3).enumerate() {
                        println!("   用户 {}: user_id={}, username={}", 
                            i + 1, user.user_id, user.username);
                    }
                }
                Err(e) => {
                    println!("   ⚠️  搜索失败: {:?}", e);
                }
            }
            println!();
            
            // ===========================================================
            // 步骤 7: 发送测试消息（如果有会话）
            // ===========================================================
            println!("📝 步骤 7: 发送测试消息");
            let test_channel_id = 12345u64;
            let test_message = format!("Hello from user {} at {}", user_id, timestamp);
            
            println!("   目标频道: {}", test_channel_id);
            println!("   消息内容: {}", test_message);
            
            match sdk.clone().send_message(test_message, test_channel_id, 1) {
                Ok(message_id) => {
                    println!("   ✅ 消息已发送，ID: {}", message_id);
                    std::thread::sleep(Duration::from_millis(500));
                }
                Err(e) => {
                    println!("   ⚠️  发送失败: {:?}", e);
                }
            }
            println!();
            
            // ===========================================================
            // 步骤 8: 查询消息历史
            // ===========================================================
            println!("📝 步骤 8: 查询消息历史");
            match sdk.get_message_history(test_channel_id, 10, None) {
                Ok(messages) => {
                    println!("   📊 历史消息: {} 条", messages.len());
                    for (i, msg) in messages.iter().take(3).enumerate() {
                        println!("   消息 {}: server_message_id={:?}, content={}", 
                            i + 1, msg.server_message_id, msg.content);
                    }
                }
                Err(e) => {
                    println!("   ⚠️  查询失败: {:?}", e);
                }
            }
            println!();
            
            // ===========================================================
            // 步骤 9: 高级功能演示
            // ===========================================================
            println!("📝 步骤 9: 高级功能演示");
            println!();
            
            // 检查连接状态
            println!("   🔹 连接状态检查:");
            let is_connected = sdk.is_connected();
            println!("      连接状态: {}", if is_connected { "✅ 已连接" } else { "❌ 未连接" });
            println!();
            
            // 群组管理功能（需要真实群组ID）
            println!("   🔹 群组管理功能（演示代码）:");
            println!("      // 创建群组");
            println!("      let group = sdk.create_group(\"My Group\".to_string(), vec![friend1, friend2])?;");
            println!("      ");
            println!("      // 获取群成员");
            println!("      let members = sdk.get_group_members(group_id, None, None)?;");
            println!("      ");
            println!("      // 邀请新成员");
            println!("      sdk.invite_to_group(group_id, vec![friend3])?;");
            println!("      ");
            println!("      // 移除成员");
            println!("      sdk.remove_group_member(group_id, user_id)?;");
            println!("      ");
            println!("      // 退出群组");
            println!("      sdk.leave_group(group_id)?;");
            println!();
            
            // 好友管理功能
            println!("   🔹 好友管理功能（演示代码）:");
            println!("      // 发送好友请求");
            println!("      sdk.send_friend_request(target_user_id, Some(\"Hello!\".to_string()))?;");
            println!("      ");
            println!("      // 接受好友请求");
            println!("      sdk.accept_friend_request(from_user_id)?;");
            println!("      ");
            println!("      // 拒绝好友请求");
            println!("      sdk.reject_friend_request(from_user_id)?;");
            println!("      ");
            println!("      // 删除好友");
            println!("      sdk.delete_friend(friend_user_id)?;");
            println!();
            
            // 黑名单功能
            println!("   🔹 黑名单管理功能（演示代码）:");
            println!("      // 添加黑名单");
            println!("      sdk.add_to_blacklist(user_id)?;");
            println!("      ");
            println!("      // 获取黑名单列表");
            println!("      let blacklist = sdk.get_blacklist()?;");
            println!("      ");
            println!("      // 移除黑名单");
            println!("      sdk.remove_from_blacklist(user_id)?;");
            println!();
            
            // 会话高级管理
            println!("   🔹 会话高级管理（演示代码）:");
            println!("      // 置顶会话");
            println!("      sdk.pin_channel(channel_id, true)?;");
            println!("      ");
            println!("      // 取消置顶");
            println!("      sdk.pin_channel(channel_id, false)?;");
            println!("      ");
            println!("      // 隐藏会话");
            println!("      sdk.hide_channel(channel_id)?;");
            println!("      ");
            println!("      // 设置静音");
            println!("      sdk.mute_channel(channel_id, true)?;");
            println!();
            
            // 消息高级操作
            println!("   🔹 消息高级操作（演示代码）:");
            println!("      // 转发/编辑 未实现，已移除");
            println!("      ");
            println!("      // 撤回消息");
            println!("      sdk.revoke_message(message_id)?;");
            println!("      ");
            println!("      // 添加表情");
            println!("      sdk.add_reaction(message_id, \"👍\".to_string())?;");
            println!("      ");
            println!("      // 移除表情");
            println!("      sdk.remove_reaction(message_id, \"👍\".to_string())?;");
            println!();
            
            // 在线状态功能
            println!("   🔹 在线状态功能（演示代码）:");
            println!("      // 订阅在线状态");
            println!("      let statuses = sdk.subscribe_presence(vec![user_id1, user_id2])?;");
            println!("      ");
            println!("      // 查询在线状态");
            println!("      let status = sdk.get_presence(user_id)?;");
            println!("      ");
            println!("      // 批量查询");
            println!("      let batch = sdk.batch_get_presence(vec![id1, id2, id3]);");
            println!();
            
            // 输入状态功能
            println!("   🔹 输入状态功能（演示代码）:");
            println!("      // 开始输入");
            println!("      sdk.send_typing(channel_id)?;");
            println!("      ");
            println!("      // 停止输入");
            println!("      sdk.stop_typing(channel_id)?;");
            println!();
            
        }
        Err(e) => {
            println!("   ❌ 注册失败: {:?}", e);
            println!("   💡 提示: 请确保服务器已启动并可访问\n");
            return Err(e.into());
        }
    }
    
    // ===========================================================
    // 清理和关闭
    // ===========================================================
    println!("📝 步骤 10: 清理和关闭");
    sdk.clone().disconnect()?;
    std::thread::sleep(Duration::from_secs(1));
    sdk.shutdown()?;
    println!("   ✅ SDK 已关闭\n");
    
    // ===========================================================
    // 总结
    // ===========================================================
    println!("🎊 完整工作流程示例完成！\n");
    println!("💡 已演示的功能:");
    println!("   ✅ 账号注册 - register()");
    println!("   ✅ 服务器连接 - connect()");
    println!("   ✅ 用户认证 - authenticate()");
    println!("   ✅ 启动同步 - run_bootstrap_sync()（auth 后必调，内部全量/增量）");
    println!("   ✅ 获取会话列表 - get_channels()");
    println!("   ✅ 获取好友列表 - get_friends()");
    println!("   ✅ 搜索用户 - search_users()");
    println!("   ✅ 发送消息 - send_message()");
    println!("   ✅ 查询历史 - get_message_history()");
    println!();
    println!("📋 需要多账号的功能:");
    println!("   • 添加好友 - send_friend_request()");
    println!("   • 接受好友 - accept_friend_request()");
    println!("   • 创建群组 - create_group()");
    println!("   • 群组邀请 - invite_to_group()");
    println!("   • 输入状态 - send_typing() / stop_typing()");
    println!("   • 在线状态 - subscribe_presence() / get_presence()");
    println!("   • 消息操作 - edit_message() / revoke_message() / add_reaction()");
    println!();
    println!("🚀 可以参考 examples/ 目录下的其他示例获取更多用法！");
    
    Ok(())
}
