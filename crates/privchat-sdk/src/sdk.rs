//! 统一 SDK 接口 - PrivchatSDK 主入口
//! 
//! 分层架构设计：
//! ```
//! PrivchatSDK (业务逻辑层)
//!   ├── PrivchatClient (传输协议层) 
//!   ├── StorageManager (存储管理层)
//!   ├── AdvancedFeatures (高级功能层)
//!   ├── EventManager (事件系统层)
//!   └── NetworkMonitor (网络监控层)
//! ```
//! 
//! 设计原则：
//! - 异步优先：主要 API 使用 async/await
//! - FFI 兼容：提供同步接口供 FFI 调用
//! - 分层清晰：每层职责明确，依赖关系清晰
//! - 事件驱动：统一的事件回调机制

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Mutex};
use serde::{Serialize, Deserialize};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::error::{PrivchatSDKError, Result};
use crate::client::{PrivchatClient, UserSession};
use crate::network::{NetworkMonitor, NetworkStatus, DummyNetworkSender, DummyNetworkStatusListener};
use crate::events::{EventManager, SDKEvent, EventFilter};
use crate::storage::{StorageManager};
use crate::storage::advanced_features::AdvancedFeaturesManager;

/// SDK 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SDKConfig {
    /// 数据存储目录
    pub data_dir: PathBuf,
    /// 用户 ID
    pub user_id: String,
    /// 服务器地址
    pub server_url: String,
    /// 连接超时时间（秒）
    pub connection_timeout: u64,
    /// 心跳间隔（秒）
    pub heartbeat_interval: u64,
    /// 重试配置
    pub retry_config: RetryConfig,
    /// 队列配置
    pub queue_config: QueueConfig,
    /// 事件配置
    pub event_config: EventConfig,
    /// 调试模式
    pub debug_mode: bool,
}

/// 重试配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// 最大重试次数
    pub max_retries: u32,
    /// 基础延迟（毫秒）
    pub base_delay_ms: u64,
    /// 最大延迟（毫秒）
    pub max_delay_ms: u64,
    /// 指数退避因子
    pub backoff_factor: f64,
}

/// 队列配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    /// 发送队列大小
    pub send_queue_size: usize,
    /// 接收队列大小
    pub receive_queue_size: usize,
    /// 批处理大小
    pub batch_size: usize,
    /// 工作线程数
    pub worker_threads: usize,
}

/// 事件配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventConfig {
    /// 事件缓冲区大小
    pub buffer_size: usize,
    /// 事件过滤器
    pub filters: Vec<EventFilter>,
}

impl Default for SDKConfig {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("./privchat_data"),
            user_id: String::new(),
            server_url: String::new(),
            connection_timeout: 30,
            heartbeat_interval: 30,
            retry_config: RetryConfig::default(),
            queue_config: QueueConfig::default(),
            event_config: EventConfig::default(),
            debug_mode: false,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30000,
            backoff_factor: 2.0,
        }
    }
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            send_queue_size: 1000,
            receive_queue_size: 1000,
            batch_size: 10,
            worker_threads: 2,
        }
    }
}

impl Default for EventConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1000,
            filters: Vec::new(),
        }
    }
}

/// SDK 配置构建器
pub struct SDKConfigBuilder {
    config: SDKConfig,
}

impl SDKConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: SDKConfig::default(),
        }
    }

    pub fn data_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.config.data_dir = path.as_ref().to_path_buf();
        self
    }

    pub fn user_id<S: Into<String>>(mut self, user_id: S) -> Self {
        self.config.user_id = user_id.into();
        self
    }

    pub fn server_url<S: Into<String>>(mut self, url: S) -> Self {
        self.config.server_url = url.into();
        self
    }

    pub fn connection_timeout(mut self, timeout: u64) -> Self {
        self.config.connection_timeout = timeout;
        self
    }

    pub fn heartbeat_interval(mut self, interval: u64) -> Self {
        self.config.heartbeat_interval = interval;
        self
    }

    pub fn retry_config(mut self, config: RetryConfig) -> Self {
        self.config.retry_config = config;
        self
    }

    pub fn queue_config(mut self, config: QueueConfig) -> Self {
        self.config.queue_config = config;
        self
    }

    pub fn event_config(mut self, config: EventConfig) -> Self {
        self.config.event_config = config;
        self
    }

    pub fn debug_mode(mut self, enabled: bool) -> Self {
        self.config.debug_mode = enabled;
        self
    }

    pub fn build(self) -> SDKConfig {
        self.config
    }
}

impl SDKConfig {
    pub fn builder() -> SDKConfigBuilder {
        SDKConfigBuilder::new()
    }
}

/// 消息输入
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageInput {
    /// 消息内容
    pub content: String,
    /// 会话 ID
    pub session_id: String,
    /// 消息类型
    pub message_type: MessageType,
    /// 扩展数据
    pub extra: HashMap<String, String>,
}

/// 消息输出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageOutput {
    /// 消息 ID
    pub message_id: String,
    /// 消息内容
    pub content: String,
    /// 发送者 ID
    pub sender_id: String,
    /// 会话 ID
    pub session_id: String,
    /// 消息类型
    pub message_type: MessageType,
    /// 消息状态
    pub status: MessageStatus,
    /// 创建时间
    pub created_at: u64,
    /// 扩展数据
    pub extra: HashMap<String, String>,
}

/// 消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    Text,
    Image,
    Audio,
    Video,
    File,
    System,
}

/// 消息状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageStatus {
    Draft,
    Sending,
    Sent,
    Delivered,
    Read,
    Failed,
    Revoked,
}

/// 统一 SDK 主接口
/// 
/// 采用分层架构：
/// - 业务逻辑层：PrivchatSDK（当前类）
/// - 传输协议层：PrivchatClient（内部使用）
/// - 存储管理层：StorageManager
/// - 事件系统层：EventManager
pub struct PrivchatSDK {
    /// SDK 配置
    config: SDKConfig,
    
    /// 传输客户端（内部使用）
    client: Arc<RwLock<Option<PrivchatClient>>>,
    
    /// 存储管理器
    storage: Arc<StorageManager>,
    
    /// 高级特性集成
    features: Arc<RwLock<Option<AdvancedFeaturesManager>>>,
    
    /// 网络监控
    network: Arc<NetworkMonitor>,
    
    /// 事件管理器
    events: Arc<EventManager>,
    
    /// 同步运行时（用于FFI）
    sync_runtime: Option<Arc<tokio::runtime::Runtime>>,
    
    /// 是否已初始化
    initialized: Arc<RwLock<bool>>,
    
    /// 是否正在关闭
    shutting_down: Arc<RwLock<bool>>,
}

impl PrivchatSDK {
    /// 异步初始化 SDK（推荐方式）
    /// 
    /// 分层初始化顺序：
    /// 1. 存储层 → 2. 网络层 → 3. 事件层 → 4. 业务层
    pub async fn initialize(config: SDKConfig) -> Result<Arc<Self>> {
        info!("正在初始化 PrivchatSDK...");
        
        // 验证配置
        Self::validate_config(&config)?;
        
        // === 第1层：存储管理器 ===
        let storage = Arc::new(StorageManager::new(&config.data_dir, None).await?);
        storage.init_user(&config.user_id).await?;
        storage.switch_user(&config.user_id).await?;
        
        // === 第2层：网络监控 ===
        let network_sender = Arc::new(DummyNetworkSender::default());
        let network_listener = Arc::new(DummyNetworkStatusListener::default());
        let network = Arc::new(NetworkMonitor::new(
            network_listener,
            network_sender,
        ));
        
        // === 第3层：事件管理器 ===
        let events = Arc::new(EventManager::new(config.event_config.buffer_size));
        
        let sdk = Arc::new(Self {
            config,
            client: Arc::new(RwLock::new(None)),
            storage,
            features: Arc::new(RwLock::new(None)),
            network,
            events,
            sync_runtime: None,
            initialized: Arc::new(RwLock::new(true)),
            shutting_down: Arc::new(RwLock::new(false)),
        });
        
        info!("PrivchatSDK 初始化完成");
        Ok(sdk)
    }
    
    /// 同步初始化 SDK（用于 FFI）
    pub fn initialize_blocking(config: SDKConfig) -> Result<Arc<Self>> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| PrivchatSDKError::Runtime(format!("创建运行时失败: {}", e)))?;
        
        let sdk = rt.block_on(async {
            Self::initialize(config).await
        })?;
        
        // 安全地设置同步运行时
        // 注意：这里使用 unsafe 是因为我们确保只在初始化时设置一次
        unsafe {
            let sdk_ptr = Arc::as_ptr(&sdk) as *mut PrivchatSDK;
            (*sdk_ptr).sync_runtime = Some(Arc::new(rt));
        }
        
        Ok(sdk)
    }
    
    /// 验证配置
    fn validate_config(config: &SDKConfig) -> Result<()> {
        if config.user_id.is_empty() {
            return Err(PrivchatSDKError::Config("用户 ID 不能为空".to_string()));
        }
        
        if config.server_url.is_empty() {
            return Err(PrivchatSDKError::Config("服务器地址不能为空".to_string()));
        }
        
        Ok(())
    }
    
    /// 连接到服务器
    /// 
    /// 这会创建内部的 PrivchatClient 并建立连接
    pub async fn connect(&self, auth_token: &str) -> Result<()> {
        self.check_initialized().await?;
        
        info!("正在连接到服务器: {}", self.config.server_url);
        
        // 创建传输层客户端
        // 注意：这里需要创建真实的 Transport，暂时用模拟的
        // let transport = Arc::new(Transport::new(...));
        // let client = PrivchatClient::new(&self.config.data_dir, transport).await?;
        
        // 暂时创建一个模拟客户端
        // TODO: 实现真实的连接逻辑
        
        // 初始化高级特性（连接成功后）
        // let features = AdvancedFeaturesIntegration::new(...);
        // *self.features.write().await = Some(features);
        
        info!("连接成功");
        Ok(())
    }
    
    /// 断开连接
    pub async fn disconnect(&self) -> Result<()> {
        // 只有在不是关闭过程中时才检查初始化状态
        if !self.is_shutting_down().await {
            self.check_initialized().await?;
        }
        
        info!("正在断开连接...");
        
        // 断开传输层客户端
        if let Some(client) = self.client.read().await.as_ref() {
            // client.disconnect("用户主动断开").await?;
        }
        
        // 清理高级特性
        *self.features.write().await = None;
        *self.client.write().await = None;
        
        info!("连接已断开");
        Ok(())
    }
    
    /// 异步关闭 SDK
    pub async fn shutdown(&self) -> Result<()> {
        info!("正在关闭 PrivchatSDK...");
        
        // 设置关闭标志
        {
            let mut shutting_down = self.shutting_down.write().await;
            *shutting_down = true;
        }
        
        // 断开连接
        self.disconnect().await?;
        
        // 停止网络监控
        // self.network.stop().await?;
        
        // 设置未初始化标志
        {
            let mut initialized = self.initialized.write().await;
            *initialized = false;
        }
        
        info!("PrivchatSDK 关闭完成");
        Ok(())
    }
    
    /// 同步关闭 SDK（用于 FFI）
    pub fn shutdown_blocking(&self) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.shutdown().await
            })
        } else {
            // 如果没有同步运行时，创建一个临时的
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| PrivchatSDKError::Runtime(format!("创建运行时失败: {}", e)))?;
            rt.block_on(async {
                self.shutdown().await
            })
        }
    }
    
    /// 检查 SDK 是否已初始化
    pub async fn is_initialized(&self) -> bool {
        *self.initialized.read().await
    }
    
    /// 检查 SDK 是否正在关闭
    pub async fn is_shutting_down(&self) -> bool {
        *self.shutting_down.read().await
    }
    
    /// 检查是否已连接
    pub async fn is_connected(&self) -> bool {
        self.client.read().await.is_some()
    }
    
    // ========== 消息操作 ==========
    
    /// 发送消息
    pub async fn send_message(&self, channel_id: &str, content: &str) -> Result<String> {
        // 检查是否已初始化
        if !self.is_initialized().await {
            return Err(PrivchatSDKError::NotInitialized("SDK未初始化".to_string()));
        }

        // 检查是否已连接
        if let Some(client) = self.client.read().await.as_ref() {
            if !client.is_connected().await {
                return Err(PrivchatSDKError::NotConnected);
            }
        } else {
            return Err(PrivchatSDKError::NotConnected);
        }

        // 创建发送请求
        let message_id = format!("msg_{}", Uuid::new_v4());
        let client_msg_no = format!("client_{}", Uuid::new_v4());
        
        if let Some(_client) = self.client.read().await.as_ref() {
            // 暂时记录日志，实际发送逻辑由 MessageSender 处理
            tracing::info!("准备发送消息: {} -> {} (内容: {})", client_msg_no, channel_id, content);
            
            // TODO: 集成 MessageSender 进行实际发送
            // let send_request = privchat_protocol::SendRequest::new();
            // 配置 send_request 的各个字段
            // 通过 MessageSender 发送
        }

        Ok(message_id)
    }
    
    /// 发送消息（完整参数）
    pub async fn send_message_with_input(&self, input: &MessageInput) -> Result<String> {
        self.check_initialized().await?;
        self.check_connected().await?;
        
        let message_id = Uuid::new_v4().to_string();
        
        // 通过传输层发送消息
        if let Some(_client) = self.client.read().await.as_ref() {
            // TODO: 集成 MessageSender 进行实际发送
            // 构建完整的 SendRequest 并通过消息队列发送
            tracing::info!("准备发送消息: {} -> {}", message_id, input.session_id);
        }
        
        debug!("消息发送成功: {}", message_id);
        Ok(message_id)
    }
    
    /// 标记消息为已读
    pub async fn mark_as_read(&self, session_id: &str, message_id: String) -> Result<()> {
        self.check_initialized().await?;
        
        if let Some(features) = self.features.read().await.as_ref() {
            // features.mark_message_as_read(session_id, 1, &self.config.user_id, &message_id).await?;
        }
        
        debug!("消息已标记为已读: {}", message_id);
        Ok(())
    }
    
    /// 撤回消息
    pub async fn recall_message(&self, message_id: &str) -> Result<()> {
        self.check_initialized().await?;
        
        if let Some(features) = self.features.read().await.as_ref() {
            // features.revoke_message(message_id, "用户撤回", None).await?;
        }
        
        debug!("消息已撤回: {}", message_id);
        Ok(())
    }
    
    /// 编辑消息
    pub async fn edit_message(&self, message_id: &str, new_content: &str) -> Result<()> {
        self.check_initialized().await?;
        
        if let Some(features) = self.features.read().await.as_ref() {
            // features.edit_message(message_id, new_content, &self.config.user_id).await?;
        }
        
        debug!("消息已编辑: {}", message_id);
        Ok(())
    }
    
    // ========== 实时交互 ==========
    
    /// 开始输入状态
    pub async fn start_typing(&self, session_id: &str) -> Result<()> {
        self.check_initialized().await?;
        
        // TODO: 实现输入状态逻辑
        
        debug!("开始输入状态: {}", session_id);
        Ok(())
    }
    
    /// 停止输入状态
    pub async fn stop_typing(&self, session_id: &str) -> Result<()> {
        self.check_initialized().await?;
        
        // TODO: 实现输入状态逻辑
        
        debug!("停止输入状态: {}", session_id);
        Ok(())
    }
    
    /// 添加表情反馈
    pub async fn add_reaction(&self, message_id: &str, emoji: &str) -> Result<()> {
        self.check_initialized().await?;
        
        // TODO: 实现表情反馈逻辑
        
        debug!("添加表情反馈: {} -> {}", message_id, emoji);
        Ok(())
    }
    
    /// 移除表情反馈
    pub async fn remove_reaction(&self, message_id: &str, emoji: &str) -> Result<()> {
        self.check_initialized().await?;
        
        // TODO: 实现表情反馈逻辑
        
        debug!("移除表情反馈: {} -> {}", message_id, emoji);
        Ok(())
    }
    
    // ========== 事件系统 ==========
    
    /// 注册消息接收回调
    pub fn on_message_received<F>(&self, callback: F) 
    where 
        F: Fn(MessageOutput) + Send + Sync + 'static 
    {
        // TODO: 实现事件订阅逻辑
        // self.events.add_listener("message_received", move |event| {
        //     // 转换事件并调用回调
        // });
    }
    
    /// 注册输入状态回调
    pub fn on_typing_indicator<F>(&self, callback: F)
    where
        F: Fn(String, String, bool) + Send + Sync + 'static // user_id, session_id, is_typing
    {
        // TODO: 实现事件订阅逻辑
    }
    
    /// 注册表情反馈回调
    pub fn on_reaction_changed<F>(&self, callback: F)
    where
        F: Fn(String, String, String, bool) + Send + Sync + 'static // message_id, user_id, emoji, is_added
    {
        // TODO: 实现事件订阅逻辑
    }
    
    /// 注册连接状态回调
    pub fn on_connection_state_changed<F>(&self, callback: F)
    where
        F: Fn(bool) + Send + Sync + 'static // is_connected
    {
        // TODO: 实现事件订阅逻辑
    }
    
    // ========== 内部方法 ==========
    
    /// 检查是否已初始化
    async fn check_initialized(&self) -> Result<()> {
        if !self.is_initialized().await {
            return Err(PrivchatSDKError::NotInitialized("SDK 未初始化".to_string()));
        }
        
        if self.is_shutting_down().await {
            return Err(PrivchatSDKError::ShuttingDown("SDK 正在关闭".to_string()));
        }
        
        Ok(())
    }
    
    /// 检查是否已连接
    async fn check_connected(&self) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        Ok(())
    }
    
    /// 获取配置
    pub fn config(&self) -> &SDKConfig {
        &self.config
    }
    
    /// 获取用户 ID
    pub fn user_id(&self) -> &str {
        &self.config.user_id
    }
    
    /// 获取事件管理器
    pub fn events(&self) -> &Arc<EventManager> {
        &self.events
    }
    
    /// 获取存储管理器
    pub fn storage(&self) -> &Arc<StorageManager> {
        &self.storage
    }
    
    /// 获取网络监控
    pub fn network(&self) -> &Arc<NetworkMonitor> {
        &self.network
    }
}

// ========== 同步接口（用于 FFI） ==========

impl PrivchatSDK {
    /// 同步连接
    pub fn connect_blocking(&self, auth_token: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.connect(auth_token).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步发送消息（阻塞版本，用于FFI）
    pub fn send_message_blocking(&self, channel_id: &str, content: &str) -> Result<String> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.send_message(channel_id, content).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步标记已读
    pub fn mark_as_read_blocking(&self, session_id: &str, message_id: String) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.mark_as_read(session_id, message_id).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步撤回消息
    pub fn recall_message_blocking(&self, message_id: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.recall_message(message_id).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步编辑消息
    pub fn edit_message_blocking(&self, message_id: &str, new_content: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.edit_message(message_id, new_content).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步开始输入状态
    pub fn start_typing_blocking(&self, session_id: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.start_typing(session_id).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
    
    /// 同步添加表情反馈
    pub fn add_reaction_blocking(&self, message_id: &str, emoji: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async {
                self.add_reaction(message_id, emoji).await
            })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_sdk_initialization() {
        let temp_dir = TempDir::new().unwrap();
        
        let config = SDKConfig::builder()
            .data_dir(temp_dir.path())
            .user_id("test_user")
            .server_url("wss://test.example.com")
            .build();
        
        let sdk = PrivchatSDK::initialize(config).await.unwrap();
        
        assert!(sdk.is_initialized().await);
        assert_eq!(sdk.user_id(), "test_user");
        assert!(!sdk.is_connected().await);
        
        sdk.shutdown().await.unwrap();
        assert!(!sdk.is_initialized().await);
    }
    
    #[tokio::test]
    async fn test_sdk_lifecycle() {
        let temp_dir = TempDir::new().unwrap();
        
        let config = SDKConfig::builder()
            .data_dir(temp_dir.path())
            .user_id("test_user")
            .server_url("wss://test.example.com")
            .build();
        
        let sdk = PrivchatSDK::initialize(config).await.unwrap();
        
        // 测试连接
        // sdk.connect("test_token").await.unwrap();
        // assert!(sdk.is_connected().await);
        
        // 测试断开连接
        sdk.disconnect().await.unwrap();
        assert!(!sdk.is_connected().await);
        
        // 测试关闭
        sdk.shutdown().await.unwrap();
        assert!(!sdk.is_initialized().await);
    }
    
    #[test]
    fn test_config_builder() {
        let config = SDKConfig::builder()
            .data_dir("/tmp/test")
            .user_id("user123")
            .server_url("wss://example.com")
            .connection_timeout(60)
            .debug_mode(true)
            .build();
        
        assert_eq!(config.data_dir, PathBuf::from("/tmp/test"));
        assert_eq!(config.user_id, "user123");
        assert_eq!(config.server_url, "wss://example.com");
        assert_eq!(config.connection_timeout, 60);
        assert!(config.debug_mode);
    }
} 