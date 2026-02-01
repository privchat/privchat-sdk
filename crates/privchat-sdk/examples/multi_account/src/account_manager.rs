//! 多账号管理器 - 管理多个 PrivchatSDK 实例

use crate::types::AccountConfig;
use crate::event_system::{EventBus, AccountEvent};
use privchat_sdk::{PrivchatSDK, PrivchatConfig, ServerEndpoint, TransportProtocol, ServerConfig};
use privchat_sdk::error::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::path::PathBuf;
use tempfile::TempDir;
use tracing::{info, error, warn};
use uuid::Uuid;

/// 多账号管理器
pub struct MultiAccountManager {
    /// 账号 SDK 映射
    sdks: HashMap<String, Arc<PrivchatSDK>>,
    /// 账号配置映射（账号名 -> AccountConfig）
    account_configs: HashMap<String, AccountConfig>,
    /// ✨ 会话 ID 缓存（存储服务端返回的 channel_id 和 group_id）
    /// Key: (user1_account, user2_account) 或 group_name, Value: channel_id/group_id
    channel_cache: HashMap<String, u64>,
    /// 事件总线
    event_bus: EventBus,
    /// 服务器端点配置
    server_endpoints: Vec<ServerEndpoint>,
    /// 临时目录（测试用）
    _temp_dir: TempDir,
    /// 临时目录路径
    temp_dir_path: PathBuf,
}

impl MultiAccountManager {
    /// 创建新的多账号管理器
    pub async fn new() -> Result<Self> {
        info!("🔧 初始化多账号管理器 (使用 PrivchatSDK)");
        
        // 创建临时目录用于测试（测试结束后自动清理）
        let temp_dir = TempDir::new()
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::IO(format!("创建临时目录失败: {}", e)))?;
        let temp_dir_path = temp_dir.path().to_path_buf();
        info!("📁 使用临时目录: {}", temp_dir_path.display());
        
        // 配置服务器端点（按优先级排序）
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
        
        let mut manager = Self {
            sdks: HashMap::new(),
            account_configs: HashMap::new(),
            channel_cache: HashMap::new(),
            event_bus: EventBus::new(),
            server_endpoints,
            _temp_dir: temp_dir,
            temp_dir_path,
        };
        
        // 初始化三个测试账号
        manager.initialize_accounts().await?;
        
        info!("✅ 多账号管理器初始化完成");
        Ok(manager)
    }
    
    /// 初始化所有测试账号
    async fn initialize_accounts(&mut self) -> Result<()> {
        // 生成随机用户名后缀
        let random_suffix = uuid::Uuid::new_v4().to_string().split('-').next().unwrap().to_string();
        
        // 注册三个测试账号（使用随机用户名）
        let alice_name = format!("alice_{}", random_suffix);
        let bob_name = format!("bob_{}", random_suffix);
        let charlie_name = format!("charlie_{}", random_suffix);
        
        info!("📝 将注册用户: {}, {}, {}", alice_name, bob_name, charlie_name);
        
        self.register_and_create_account(&alice_name, "password123").await?;
        self.register_and_create_account(&bob_name, "password123").await?;
        self.register_and_create_account(&charlie_name, "password123").await?;
        
        info!("✅ 所有测试账号注册完成");
        
        Ok(())
    }
    
    /// 注册并创建单个账号
    async fn register_and_create_account(&mut self, name: &str, password: &str) -> Result<()> {
        info!("🔧 注册账号: {}", name);
        
        let data_dir = self.temp_dir_path.join(name);
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::IO(format!("创建目录失败: {}", e)))?;
        
        // ⭐ 使用标准UUID格式作为设备ID（服务器端要求）
        let device_id = uuid::Uuid::new_v4().to_string();
        
        // 构造 DeviceInfo
        let device_info = privchat_protocol::message::DeviceInfo {
            device_id: device_id.clone(),
            device_name: format!("{}'s Device", name),
            device_type: privchat_protocol::message::DeviceType::MacOS,
            app_id: "macos".to_string(),
            device_model: Some("MacBook Pro".to_string()),
            push_token: None,
            push_channel: None,
            manufacturer: None,
            device_fingerprint: Some(format!("fingerprint_{}", uuid::Uuid::new_v4())),
            os_version: Some("macOS 14.0".to_string()),
            app_version: Some("1.0.0".to_string()),
        };
        
        // 创建 SDK 配置
        let config = PrivchatConfig {
            data_dir,
            assets_dir: None,  // SDK 使用 refinery 内置 migrations
            server_config: ServerConfig {
                endpoints: self.server_endpoints.clone(),
            },
            connection_timeout: 30,
            heartbeat_interval: 30,
            retry_config: Default::default(),
            queue_config: Default::default(),
            event_config: Default::default(),
            timezone_offset_seconds: Some(8 * 3600),
            debug_mode: false,
            file_api_base_url: None,
            http_client_config: privchat_sdk::HttpClientConfig::default(),
            image_send_max_edge: Some(1080),
        };
        
        let sdk = PrivchatSDK::initialize(config).await?;
        
        // 1. 建立网络连接
        info!("🔌 正在建立网络连接...");
        sdk.connect().await?;
        
        // 2. 调用 SDK 的 register 方法
        info!("📝 正在注册用户: {}", name);
        let (user_id, token) = sdk.register(
            name.to_string(),
            password.to_string(),
            device_id.clone(),
            Some(device_info.clone()),
        ).await?;
        
        info!("✅ 账号 {} 注册成功: user_id={}", name, user_id);
        
        // 3. 使用 token 进行认证
        info!("🔐 正在认证用户: {}", name);
        sdk.authenticate(user_id, &token, device_info).await?;
        info!("✅ 账号 {} 认证成功", name);
        
        // 存储账号配置
        // 提取简短名字（去掉后缀）用作 key，例如 "alice_ddc59247" -> "alice"
        let short_name = name.split('_').next().unwrap_or(name);
        
        self.account_configs.insert(short_name.to_string(), AccountConfig {
            name: name.to_string(),
            user_id,
            token,
            data_dir: self.temp_dir_path.join(name).to_string_lossy().to_string(),
            password: Some(password.to_string()), // 保存密码
            full_username: Some(name.to_string()), // 保存完整用户名
            device_id: Some(device_id.clone()), // 保存设备ID，确保登录时使用相同设备ID
        });
        self.sdks.insert(short_name.to_string(), sdk);
        
        info!("✅ 账号 {} SDK 创建并认证完成", name);
        Ok(())
    }
    
    /// 连接所有账号（已在注册时自动连接）
    pub async fn connect_all(&mut self) -> Result<()> {
        info!("🔌 所有账号已在注册时自动连接");
        
        // 验证连接状态
        for (account_name, sdk) in &self.sdks {
            let is_connected = sdk.is_connected().await;
            info!("📊 账号 {} 连接状态: {}", account_name, if is_connected { "✅ 已连接" } else { "❌ 未连接" });
        }
        
        Ok(())
    }
    
    /// 发送消息（使用 SDK）
    pub async fn send_message(
        &mut self,
        from_account: &str,
        to_user_id: u64,
        content: &str,
    ) -> Result<u64> {
        let sdk = self.sdks.get(from_account)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", from_account)
            ))?;
        
        // ✅ 从数据库查询对应的 channel_id（username 存储对方的 user_id）
        let channel_id = match sdk.storage().find_channel_id_by_user(to_user_id).await? {
            Some(ch_id) => {
                info!("📤 [{}] 从数据库查询到 channel_id={} (user={}): {}", from_account, ch_id, to_user_id, content);
                ch_id
            }
            None => {
                warn!("⚠️ [{}] 未找到与 user_id={} 对应的 channel，使用 user_id 作为 channel_id", from_account, to_user_id);
                to_user_id
            }
        };
        
        // 使用 SDK 发送消息（会自动入队）
        let local_message_id = sdk.send_message(channel_id, content).await?;
        
        info!("✅ [{}] 消息已加入队列: {}", from_account, local_message_id);
        Ok(local_message_id)
    }
    
    /// 获取账号的 user_id
    pub fn get_user_id(&self, account_name: &str) -> Option<u64> {
        self.account_configs.get(account_name).map(|config| config.user_id)
    }
    
    /// 获取账号的完整用户名（包含后缀，如 "alice_xxx"）
    pub fn get_full_username(&self, account_name: &str) -> Option<String> {
        self.account_configs.get(account_name).map(|config| config.name.clone())
    }
    
    /// 获取 SDK 实例
    pub fn get_sdk(&self, account_name: &str) -> Option<Arc<PrivchatSDK>> {
        self.sdks.get(account_name).cloned()
    }
    
    /// 缓存会话 ID
    pub fn cache_channel_id(&mut self, key: String, channel_id: u64) {
        info!("💾 缓存会话 ID: {} = {}", key, channel_id);
        self.channel_cache.insert(key, channel_id);
    }
    
    /// 获取缓存的会话 ID
    pub fn get_cached_channel_id(&self, key: &str) -> Option<u64> {
        self.channel_cache.get(key).copied()
    }
    
    /// 获取事件总线
    pub fn event_bus(&self) -> &EventBus {
        &self.event_bus
    }
    
    // ========== SDK 方法代理 ==========
    
    /// 认证所有账号（Phase 1 用）
    pub async fn authenticate_all(&mut self) -> Result<Vec<String>> {
        self.connect_all().await?;
        Ok(vec!["alice".to_string(), "bob".to_string(), "charlie".to_string()])
    }
    
    /// 验证所有账号已连接
    pub async fn verify_all_connected(&self) -> Result<()> {
        for (account_name, sdk) in &self.sdks {
            if !sdk.is_connected().await {
                return Err(privchat_sdk::error::PrivchatSDKError::NotConnected);
            }
        }
        Ok(())
    }
    
    /// 搜索用户
    /// 搜索用户，返回搜索会话ID
    pub async fn search_users(&self, account_name: &str, query: &str) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.search_users(query).await
    }
    
    /// 发送好友请求
    pub async fn send_friend_request(
        &self,
        account_name: &str,
        to_user_id: u64,
        remark: Option<&str>,
        search_session_id: Option<String>,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.send_friend_request(to_user_id, remark, search_session_id).await
    }
    
    /// 接受好友请求
    pub async fn accept_friend_request(
        &self,
        account_name: &str,
        from_user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.accept_friend_request(from_user_id).await
    }
    
    /// 拒绝好友请求
    pub async fn reject_friend_request(
        &self,
        account_name: &str,
        from_user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.reject_friend_request(from_user_id).await
    }
    
    /// 获取好友列表（含 User 展示信息）
    pub async fn get_friend_list(
        &self,
        account_name: &str,
    ) -> Result<Vec<(privchat_sdk::storage::entities::Friend, privchat_sdk::storage::entities::User)>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        sdk.get_friends(100, 0).await
    }
    
    /// 删除好友
    pub async fn delete_friend(
        &self,
        account_name: &str,
        friend_user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.delete_friend(friend_user_id).await
    }
    
    /// 创建群组
    pub async fn create_group(
        &self,
        account_name: &str,
        name: &str,
        member_ids: Vec<u64>,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.create_group(name, member_ids)
            .await
            .map(|r| serde_json::to_value(&r).unwrap_or(serde_json::Value::Null))
    }
    
    /// 邀请成员加入群组
    pub async fn invite_to_group(
        &self,
        account_name: &str,
        group_id: u64,
        user_ids: Vec<u64>,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.invite_to_group(group_id, user_ids).await
    }
    
    /// 退出群组
    pub async fn leave_group(
        &self,
        account_name: &str,
        group_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.leave_group(group_id)
            .await
            .map(|b| serde_json::json!(b))
    }
    
    /// 获取群组成员（按 group_id，从本地数据库）
    pub async fn get_group_members(
        &self,
        account_name: &str,
        group_id: u64,
    ) -> Result<Vec<privchat_sdk::storage::entities::ChannelMember>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        sdk.get_group_members(group_id, None, None).await
    }
    
    /// 移除群组成员
    pub async fn remove_group_member(
        &self,
        account_name: &str,
        group_id: u64,
        user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.remove_group_member(group_id, user_id)
            .await
            .map(|b| serde_json::json!(b))
    }
    
    /// 获取历史消息
    pub async fn get_message_history(
        &self,
        account_name: &str,
        channel_id: u64,
        limit: u32,
        before_message_id: Option<u64>,
    ) -> Result<Vec<privchat_sdk::storage::entities::Message>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.get_messages(channel_id, limit, before_message_id).await
    }
    
    /// 搜索消息
    pub async fn search_messages(
        &self,
        account_name: &str,
        query: &str,
        channel_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.search_messages(query, channel_id).await
    }
    
    /// 标记消息已读
    pub async fn mark_as_read(
        &self,
        account_name: &str,
        channel_id: u64,
        message_id: u64,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.mark_as_read(channel_id, message_id).await
    }
    
    /// 根据 message.id 获取消息（返回 Option<Message>）
    pub async fn get_message_by_id(
        &self,
        account_name: &str,
        id: u64,
    ) -> Result<Option<privchat_sdk::storage::entities::Message>> {
        let sdk = self.sdks.get(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::InvalidInput(format!("账号未找到: {}", account_name)))?;
        
        sdk.get_message_by_id(id).await
    }
    
    /// 撤回消息
    pub async fn recall_message(
        &self,
        account_name: &str,
        message_id: u64,
        _channel_id: u64,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.recall_message(message_id).await
    }
    
    /// 撤回消息（别名，按 server_message_id + channel_id，供测试使用）
    pub async fn revoke_message(
        &self,
        account_name: &str,
        message_id: u64,
        channel_id: u64,
    ) -> Result<()> {
        self.recall_message(account_name, message_id, channel_id).await
    }

    /// 编辑消息
    pub async fn edit_message(
        &self,
        account_name: &str,
        message_id: u64,
        new_content: &str,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.edit_message(message_id, new_content).await
    }
    
    /// 添加表情反应
    pub async fn add_reaction(
        &self,
        account_name: &str,
        message_id: u64,
        emoji: &str,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.add_reaction(message_id, emoji).await
    }
    
    /// 移除表情反应
    pub async fn remove_reaction(
        &self,
        account_name: &str,
        message_id: u64,
        emoji: &str,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.remove_reaction(message_id, emoji).await
    }
    
    /// 开始输入
    pub async fn start_typing(
        &self,
        account_name: &str,
        channel_id: u64,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.start_typing(channel_id).await
    }
    
    /// 停止输入
    pub async fn stop_typing(
        &self,
        account_name: &str,
        channel_id: u64,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.stop_typing(channel_id).await
    }
    
    /// 添加黑名单
    pub async fn add_to_blacklist(
        &self,
        account_name: &str,
        blocked_user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.add_to_blacklist(blocked_user_id).await
    }
    
    /// 移除黑名单
    pub async fn remove_from_blacklist(
        &self,
        account_name: &str,
        blocked_user_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.remove_from_blacklist(blocked_user_id).await
    }
    
    /// 获取黑名单列表
    pub async fn get_blacklist(&self, account_name: &str) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.get_blacklist().await
    }
    
    /// 获取会话列表
    pub async fn get_channel_list(&self, account_name: &str) -> Result<Vec<privchat_sdk::storage::entities::Channel>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        let query = privchat_sdk::storage::entities::ChannelQuery {
            limit: Some(100),
            offset: Some(0),
            ..Default::default()
        };
        sdk.get_channels(&query).await
    }
    
    /// 置顶会话
    pub async fn pin_channel(
        &self,
        account_name: &str,
        channel_id: u64,
        pin: bool,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.pin_channel(channel_id, pin)
            .await
            .map(|b| serde_json::json!(b))
    }
    
    /// 删除会话
    pub async fn delete_channel(
        &self,
        account_name: &str,
        channel_id: u64,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.hide_channel(channel_id)
            .await
            .map(|b| serde_json::json!(b))
    }
    
    /// 通用 RPC 调用（用于尚未封装的接口）
    pub async fn rpc_call(
        &self,
        account_name: &str,
        route: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.rpc_call(route, params).await
    }
    
    /// 更新账号的 JWT token
    pub fn update_token(&mut self, account_key: &str, token: String) -> Result<()> {
        if let Some(config) = self.account_configs.get_mut(account_key) {
            config.token = token;
            info!("✅ 更新 {} 的 token", account_key);
            Ok(())
        } else {
            Err(privchat_sdk::error::PrivchatSDKError::NotConnected)
        }
    }
    
    /// 更新账号的 user_id
    pub fn update_user_id(&mut self, account_key: &str, user_id: u64) -> Result<()> {
        if let Some(config) = self.account_configs.get_mut(account_key) {
            config.user_id = user_id;
            info!("✅ 更新 {} 的 user_id: {}", account_key, user_id);
            Ok(())
        } else {
            Err(privchat_sdk::error::PrivchatSDKError::NotFound(
                format!("账号 {} 不存在", account_key)
            ))
        }
    }
    
    // ========== 在线状态管理 ==========
    
    /// 订阅在线状态
    pub async fn subscribe_presence(
        &self,
        account_name: &str,
        user_ids: Vec<u64>,
    ) -> Result<std::collections::HashMap<u64, privchat_protocol::presence::OnlineStatusInfo>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.subscribe_presence(user_ids).await
    }
    
    /// 取消订阅在线状态
    pub async fn unsubscribe_presence(
        &self,
        account_name: &str,
        user_ids: Vec<u64>,
    ) -> Result<()> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.unsubscribe_presence(user_ids).await
    }
    
    /// 获取在线状态（缓存）
    pub async fn get_presence(
        &self,
        account_name: &str,
        user_id: u64,
    ) -> Option<privchat_protocol::presence::OnlineStatusInfo> {
        let sdk = self.get_sdk(account_name)?;
        sdk.get_presence(user_id).await
    }
    
    /// 批量获取在线状态
    pub async fn batch_get_presence(
        &self,
        account_name: &str,
        user_ids: Vec<u64>,
    ) -> std::collections::HashMap<u64, privchat_protocol::presence::OnlineStatusInfo> {
        if let Some(sdk) = self.get_sdk(account_name) {
            sdk.batch_get_presence(&user_ids).await
        } else {
            std::collections::HashMap::new()
        }
    }
    
    /// 从服务器获取在线状态
    pub async fn fetch_presence(
        &self,
        account_name: &str,
        user_ids: Vec<u64>,
    ) -> Result<std::collections::HashMap<u64, privchat_protocol::presence::OnlineStatusInfo>> {
        let sdk = self.get_sdk(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 不存在", account_name)
            ))?;
        
        sdk.fetch_presence(user_ids).await
    }
    
    // ========== 统计信息 ==========
    
    /// 获取在线状态统计
    pub async fn get_presence_stats(
        &self,
        account_name: &str,
    ) -> Option<privchat_sdk::presence::PresenceCacheStats> {
        let sdk = self.get_sdk(account_name)?;
        Some(sdk.get_presence_stats().await)
    }
    
    /// 获取输入状态统计
    pub async fn get_typing_stats(
        &self,
        account_name: &str,
    ) -> Option<privchat_sdk::typing::TypingStats> {
        let sdk = self.get_sdk(account_name)?;
        Some(sdk.get_typing_stats().await)
    }
    
    // ========== 连接状态详情 ==========
    
    /// 获取连接状态
    pub async fn get_connection_state(
        &self,
        account_name: &str,
    ) -> Option<privchat_sdk::connection_state::ConnectionState> {
        let sdk = self.get_sdk(account_name)?;
        Some(sdk.get_connection_state().await)
    }
    
    /// 获取连接摘要
    pub async fn get_connection_summary(
        &self,
        account_name: &str,
    ) -> Option<String> {
        let sdk = self.get_sdk(account_name)?;
        Some(sdk.get_connection_summary().await)
    }
    
    // ========== 账号登录 ==========
    
    /// 登录已有账号（使用保存的密码）
    pub async fn login_account(
        &mut self,
        account_name: &str,
    ) -> Result<()> {
        // 获取账号配置
        let config = self.account_configs.get(account_name)
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 的配置不存在", account_name)
            ))?;
        
        // 检查是否有保存的密码和用户名
        let password = config.password.clone()
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 没有保存密码", account_name)
            ))?;
        
        let full_username = config.full_username.clone()
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 没有保存完整用户名", account_name)
            ))?;
        
        // ⭐ 使用保存的设备ID，确保与注册时相同
        let device_id = config.device_id.clone()
            .ok_or_else(|| privchat_sdk::error::PrivchatSDKError::Other(
                format!("账号 {} 没有保存设备ID", account_name)
            ))?;
        
        info!("🔐 正在登录账号: {} (device_id: {})", account_name, device_id);
        
        // 创建新的数据目录（login test）
        let login_data_dir = self.temp_dir_path.join(format!("{}_login", account_name));
        std::fs::create_dir_all(&login_data_dir)
            .map_err(|e| privchat_sdk::error::PrivchatSDKError::IO(format!("创建目录失败: {}", e)))?;
        
        // 构造 DeviceInfo
        let device_info = privchat_protocol::message::DeviceInfo {
            device_id: device_id.clone(),
            device_name: format!("{}'s Login Device", account_name),
            device_type: privchat_protocol::message::DeviceType::MacOS,
            app_id: "macos".to_string(),
            device_model: Some("MacBook Pro".to_string()),
            push_token: None,
            push_channel: None,
            manufacturer: None,
            device_fingerprint: Some(format!("fingerprint_{}", uuid::Uuid::new_v4())),
            os_version: Some("macOS 14.0".to_string()),
            app_version: Some("1.0.0".to_string()),
        };
        
        // 创建新的 SDK 配置
        let sdk_config = PrivchatConfig {
            data_dir: login_data_dir.clone(),
            assets_dir: None,  // SDK 使用 refinery 内置 migrations
            server_config: ServerConfig {
                endpoints: self.server_endpoints.clone(),
            },
            connection_timeout: 30,
            heartbeat_interval: 30,
            retry_config: Default::default(),
            queue_config: Default::default(),
            event_config: Default::default(),
            timezone_offset_seconds: Some(8 * 3600),
            debug_mode: false,
            file_api_base_url: None,
            http_client_config: privchat_sdk::HttpClientConfig::default(),
            image_send_max_edge: Some(1080),
        };
        
        let sdk = PrivchatSDK::initialize(sdk_config).await?;
        
        // 1. 建立网络连接
        info!("🔌 正在建立网络连接...");
        sdk.connect().await?;
        
        // 2. 调用 SDK 的 login 方法
        info!("📝 正在登录用户: {} (username: {})", account_name, full_username);
        let (user_id, token) = sdk.login(
            full_username.clone(),
            password.clone(),
            device_id.clone(),
            Some(device_info.clone()),
        ).await?;
        
        info!("✅ 账号 {} 登录成功: user_id={}", account_name, user_id);
        
        // 3. 使用 token 进行认证
        info!("🔐 正在认证用户: {}", account_name);
        sdk.authenticate(user_id, &token, device_info).await?;
        info!("✅ 账号 {} 认证成功", account_name);
        
        // 存储新的 SDK 实例（使用 login_ 前缀区分）
        let login_key = format!("{}_login", account_name);
        self.sdks.insert(login_key, sdk);
        
        Ok(())
    }
    
    /// 清理资源
    pub async fn cleanup(&mut self) -> Result<()> {
        info!("🧹 开始清理资源...");
        
        for (account_name, sdk) in &self.sdks {
            info!("🔌 断开账号: {}", account_name);
            if let Err(e) = sdk.disconnect().await {
                warn!("断开账号 {} 失败: {}", account_name, e);
            }
        }
        
        self.sdks.clear();
        
        info!("✅ 资源清理完成");
        Ok(())
    }
}
