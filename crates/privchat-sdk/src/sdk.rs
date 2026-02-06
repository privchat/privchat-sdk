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

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::client::PrivchatClient;
use crate::connection_state::{ConnectionProtocol, ConnectionStateManager};
use crate::error::{PrivchatSDKError, Result};
use crate::events::{ConnectionState as EventConnectionState, EventFilter, EventManager, SDKEvent};
use crate::network::{NetworkMonitor, NetworkStatus, NetworkStatusEvent, NetworkStatusListener};
use crate::rate_limiter::{
    MessageRateLimiter, MessageRateLimiterConfig, ReconnectRateLimiter, ReconnectRateLimiterConfig,
    RpcRateLimiter, RpcRateLimiterConfig,
};
use crate::rpc_client::RpcClientExt;
use crate::storage::advanced_features::AdvancedFeaturesManager;
use crate::storage::queue::{MessageData, QueuePriority, SendQueueManager, SendTask};
use crate::storage::StorageManager;
use async_trait::async_trait;
use privchat_protocol::presence::{TypingActionType, TypingIndicatorRequest};
use privchat_protocol::rpc::routes;
use privchat_protocol::PushMessageRequest;
use tokio::sync::broadcast;

/// 默认网络状态监听器（内部使用，假设网络始终在线）
/// 实际应用应该由平台层（Android/iOS）提供真实的网络状态监听
#[derive(Debug)]
struct DefaultNetworkStatusListener {
    status: Arc<RwLock<NetworkStatus>>,
    sender: Arc<RwLock<Option<broadcast::Sender<NetworkStatusEvent>>>>,
}

impl Default for DefaultNetworkStatusListener {
    fn default() -> Self {
        Self {
            status: Arc::new(RwLock::new(NetworkStatus::Online)),
            sender: Arc::new(RwLock::new(None)),
        }
    }
}

#[async_trait]
impl NetworkStatusListener for DefaultNetworkStatusListener {
    async fn get_current_status(&self) -> NetworkStatus {
        self.status.read().await.clone()
    }

    async fn start_monitoring(
        &self,
    ) -> crate::error::Result<broadcast::Receiver<NetworkStatusEvent>> {
        let (sender, receiver) = broadcast::channel(100);
        {
            let mut sender_guard = self.sender.write().await;
            *sender_guard = Some(sender);
        }
        Ok(receiver)
    }

    async fn stop_monitoring(&self) {
        let mut sender_guard = self.sender.write().await;
        *sender_guard = None;
    }
}

/// 传输协议类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransportProtocol {
    /// QUIC 协议（高性能）
    Quic,
    /// TCP 协议（稳定）
    Tcp,
    /// WebSocket 协议（兼容性强）
    WebSocket,
}

/// 服务器配置项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEndpoint {
    /// 协议类型
    pub protocol: TransportProtocol,
    /// 服务器地址（可以是域名或IP）
    pub host: String,
    /// 端口号
    pub port: u16,
    /// 路径（用于WebSocket）
    pub path: Option<String>,
    /// 是否使用TLS（仅对WebSocket有效，QUIC强制TLS，TCP通常不使用TLS）
    pub use_tls: bool,
}

impl ServerEndpoint {
    /// 转换为 client::ServerEndpoint
    pub fn to_client_endpoint(&self) -> crate::client::ServerEndpoint {
        crate::client::ServerEndpoint {
            protocol: match self.protocol {
                TransportProtocol::Quic => crate::client::TransportProtocol::Quic,
                TransportProtocol::Tcp => crate::client::TransportProtocol::Tcp,
                TransportProtocol::WebSocket => crate::client::TransportProtocol::WebSocket,
            },
            host: self.host.clone(),
            port: self.port,
            path: self.path.clone(),
            use_tls: self.use_tls,
        }
    }
}

/// 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 服务器端点列表（按优先级顺序）
    pub endpoints: Vec<ServerEndpoint>,
}

/// HTTP 客户端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpClientConfig {
    /// 连接超时（秒）
    pub connect_timeout_secs: Option<u64>,
    /// 请求超时（秒）
    pub request_timeout_secs: Option<u64>,
    /// 是否启用重试
    pub enable_retry: bool,
    /// 最大重试次数
    pub max_retries: u32,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout_secs: Some(30),
            request_timeout_secs: Some(300), // 文件上传可能需要较长时间
            enable_retry: true,
            max_retries: 3,
        }
    }
}

/// Privchat SDK 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivchatConfig {
    /// 数据存储目录
    pub data_dir: PathBuf,
    /// Assets目录（SQL脚本等）
    pub assets_dir: Option<PathBuf>,
    /// 服务器配置
    pub server_config: ServerConfig,
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
    /// 时区配置（时区偏移秒数，例如：+8小时 = 28800，-5小时 = -18000）
    /// None 表示使用系统本地时区
    pub timezone_offset_seconds: Option<i32>,
    /// 调试模式
    pub debug_mode: bool,
    /// 文件服务 API 基础 URL
    ///
    /// 例如：https://files.example.com/api/app
    /// 如果不提供，将从 RPC 响应的 upload_url 中提取（但建议显式配置）
    pub file_api_base_url: Option<String>,
    /// HTTP 客户端配置
    pub http_client_config: HttpClientConfig,
    /// 发送图片时最长边上限（如 720、1080）；None 表示发原图
    pub image_send_max_edge: Option<u32>,
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

impl Default for ServerEndpoint {
    fn default() -> Self {
        Self {
            protocol: TransportProtocol::Tcp,
            host: "localhost".to_string(),
            port: 9001,
            path: None,
            use_tls: false,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![
                ServerEndpoint {
                    protocol: TransportProtocol::Quic,
                    host: "localhost".to_string(),
                    port: 9001,
                    path: None,
                    use_tls: true, // QUIC强制TLS
                },
                ServerEndpoint {
                    protocol: TransportProtocol::Tcp,
                    host: "localhost".to_string(),
                    port: 9001,
                    path: None,
                    use_tls: false, // TCP通常不使用TLS
                },
                ServerEndpoint {
                    protocol: TransportProtocol::WebSocket,
                    host: "localhost".to_string(),
                    port: 9080,
                    path: Some("/".to_string()),
                    use_tls: true, // 默认使用wss
                },
            ],
        }
    }
}

impl Default for PrivchatConfig {
    fn default() -> Self {
        Self {
            data_dir: get_default_data_dir(),
            assets_dir: None,
            server_config: ServerConfig::default(),
            connection_timeout: 15, // 单次尝试 15s 超时，无网络/服务不可用时快速失败，便于多轮重试（4×15s 比 1×60s 更易连上）
            heartbeat_interval: 30,
            retry_config: RetryConfig::default(),
            queue_config: QueueConfig::default(),
            event_config: EventConfig::default(),
            timezone_offset_seconds: None, // 默认使用系统本地时区
            debug_mode: false,
            file_api_base_url: None,
            http_client_config: HttpClientConfig::default(),
            image_send_max_edge: Some(1080),
        }
    }
}

/// 获取默认数据目录 ~/.privchat/
fn get_default_data_dir() -> PathBuf {
    // 尝试获取用户主目录
    if let Some(home_dir) = std::env::var("HOME").ok().map(PathBuf::from) {
        home_dir.join(".privchat")
    } else if let Some(home_dir) = std::env::var("USERPROFILE").ok().map(PathBuf::from) {
        // Windows 支持
        home_dir.join(".privchat")
    } else {
        // 如果无法获取用户主目录，则回退到当前目录
        PathBuf::from("./privchat_data")
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

/// Privchat SDK 配置构建器
pub struct PrivchatConfigBuilder {
    config: PrivchatConfig,
}

impl PrivchatConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: PrivchatConfig::default(),
        }
    }

    pub fn data_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.config.data_dir = path.as_ref().to_path_buf();
        self
    }

    pub fn assets_dir<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.config.assets_dir = Some(path.as_ref().to_path_buf());
        self
    }

    /// 添加服务器端点
    pub fn add_server<S: Into<String>>(mut self, url: S) -> Self {
        if let Some(endpoint) = self.parse_server_url(&url.into()) {
            self.config.server_config.endpoints.push(endpoint);
        }
        self
    }

    /// 设置服务器端点列表（按优先级顺序）
    pub fn servers<I, S>(mut self, urls: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut endpoints = Vec::new();
        for url in urls {
            if let Some(endpoint) = self.parse_server_url(&url.into()) {
                endpoints.push(endpoint);
            }
        }
        self.config.server_config.endpoints = endpoints;
        self
    }

    pub fn server_config(mut self, config: ServerConfig) -> Self {
        self.config.server_config = config;
        self
    }

    /// 解析服务器URL
    fn parse_server_url(&self, url: &str) -> Option<ServerEndpoint> {
        if url.starts_with("quic://") {
            self.parse_url_parts(url, "quic://", TransportProtocol::Quic, true) // QUIC强制TLS
        } else if url.starts_with("tcp://") {
            self.parse_url_parts(url, "tcp://", TransportProtocol::Tcp, false) // TCP通常不使用TLS
        } else if url.starts_with("ws://") {
            self.parse_url_parts(url, "ws://", TransportProtocol::WebSocket, false)
        // 明确的非安全WebSocket
        } else if url.starts_with("wss://") {
            self.parse_url_parts(url, "wss://", TransportProtocol::WebSocket, true)
        // 安全WebSocket
        } else {
            None
        }
    }

    fn parse_url_parts(
        &self,
        url: &str,
        prefix: &str,
        protocol: TransportProtocol,
        use_tls: bool,
    ) -> Option<ServerEndpoint> {
        let remainder = url.strip_prefix(prefix)?;

        // 分离主机:端口和路径
        let (host_port, path) = if let Some(slash_pos) = remainder.find('/') {
            let host_port = &remainder[..slash_pos];
            let path = &remainder[slash_pos..];
            (host_port, Some(path.to_string()))
        } else {
            (remainder, None)
        };

        if let Some((host, port)) = self.parse_host_port(host_port) {
            Some(ServerEndpoint {
                protocol,
                host,
                port,
                path,
                use_tls,
            })
        } else {
            None
        }
    }

    fn parse_host_port(&self, host_port: &str) -> Option<(String, u16)> {
        if let Some(colon_pos) = host_port.rfind(':') {
            let host = &host_port[..colon_pos];
            let port_str = &host_port[colon_pos + 1..];
            if let Ok(port) = port_str.parse::<u16>() {
                return Some((host.to_string(), port));
            }
        }
        // 如果没有端口，使用默认端口（PrivChat Gateway 9001）
        Some((host_port.to_string(), 9001))
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

    /// 设置时区偏移（从小时）
    ///
    /// # 参数
    ///
    /// * `hours` - 时区小时偏移，例如：+8, -5
    pub fn timezone_hours(mut self, hours: i32) -> Self {
        self.config.timezone_offset_seconds = Some(hours * 3600);
        self
    }

    /// 设置时区偏移（从分钟）
    ///
    /// # 参数
    ///
    /// * `minutes` - 时区分钟偏移，例如：480 (+8小时), -300 (-5小时)
    pub fn timezone_minutes(mut self, minutes: i32) -> Self {
        self.config.timezone_offset_seconds = Some(minutes * 60);
        self
    }

    /// 设置时区偏移（从秒）
    ///
    /// # 参数
    ///
    /// * `seconds` - 时区秒偏移
    pub fn timezone_seconds(mut self, seconds: i32) -> Self {
        self.config.timezone_offset_seconds = Some(seconds);
        self
    }

    /// 使用系统本地时区
    pub fn timezone_local(mut self) -> Self {
        self.config.timezone_offset_seconds = None;
        self
    }

    /// 设置文件服务 API 基础 URL
    pub fn file_api_base_url<S: Into<String>>(mut self, url: S) -> Self {
        self.config.file_api_base_url = Some(url.into());
        self
    }

    /// 设置 HTTP 客户端配置
    pub fn http_client_config(mut self, config: HttpClientConfig) -> Self {
        self.config.http_client_config = config;
        self
    }

    /// 发送图片时最长边上限（如 90、720、1080）；None 表示发原图；横图以宽为基准、竖图以高为基准，另一边等比缩放
    pub fn image_send_max_edge(mut self, max_edge: Option<u32>) -> Self {
        self.config.image_send_max_edge = max_edge;
        self
    }

    pub fn build(self) -> PrivchatConfig {
        self.config
    }
}

impl PrivchatConfig {
    pub fn builder() -> PrivchatConfigBuilder {
        PrivchatConfigBuilder::new()
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
    /// 服务端消息 ID
    pub server_message_id: u64,
    /// 消息内容
    pub content: String,
    /// 发送者 ID
    pub sender_id: u64,
    /// 会话 ID
    pub session_id: u64,
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

/// 连接状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// 未连接
    Disconnected,
    /// 连接中
    Connecting,
    /// 已连接
    Connected,
    /// 重连中
    Reconnecting,
}

/// SDK 运行状态
#[derive(Debug, Clone)]
pub struct SDKState {
    /// 连接状态
    pub connection_state: ConnectionState,
    /// 当前使用的协议
    pub current_protocol: Option<TransportProtocol>,
    /// 当前用户ID
    pub current_user_id: Option<u64>,
    /// 最后连接时间
    pub last_connected: Option<Instant>,
    /// 最后断开时间
    pub last_disconnected: Option<Instant>,
}

impl Default for SDKState {
    fn default() -> Self {
        Self {
            connection_state: ConnectionState::Disconnected,
            current_protocol: None,
            current_user_id: None,
            last_connected: None,
            last_disconnected: None,
        }
    }
}

/// 统一 SDK 主接口
///
/// 采用分层架构：
/// - 业务逻辑层：PrivchatSDK（当前类）
/// - 传输协议层：PrivchatClient（内部使用）
/// - 存储管理层：StorageManager
/// - 事件系统层：EventManager
/// - 队列系统层：SendQueueManager
pub struct PrivchatSDK {
    /// SDK 配置
    config: PrivchatConfig,

    /// 传输客户端（内部使用）
    client: Arc<RwLock<Option<PrivchatClient>>>,

    /// 存储管理器
    storage: Arc<StorageManager>,

    /// 高级特性集成
    features: Arc<RwLock<Option<AdvancedFeaturesManager>>>,

    /// 网络监控
    network: Arc<NetworkMonitor>,

    /// 事件管理器
    event_manager: Arc<EventManager>,

    /// 每用户发送队列管理器（路径为 users/{uid}/queue.db），不共享
    user_send_queue_managers: Arc<RwLock<HashMap<String, Arc<SendQueueManager>>>>,

    /// 发送消息消费者
    send_consumer:
        Arc<RwLock<Option<Arc<crate::storage::queue::send_consumer::SendConsumerRunner>>>>,

    /// 连接状态管理器
    connection_state: Arc<ConnectionStateManager>,

    /// 在线状态管理器
    presence_manager: Arc<crate::presence::PresenceManager>,

    /// 生命周期管理器
    lifecycle_manager: Arc<tokio::sync::RwLock<crate::lifecycle::LifecycleManager>>,

    /// 输入状态管理器
    typing_manager: Arc<crate::typing::TypingManager>,

    /// pts 管理器（Phase 8）
    pts_manager: Arc<crate::sync::PtsManager>,

    /// 同步引擎（Phase 8，在 connect() 时初始化）
    sync_engine: Arc<RwLock<Option<Arc<crate::sync::SyncEngine>>>>,

    /// 同步观察者（用于监听同步状态变化）
    sync_observer: Arc<RwLock<Option<Arc<dyn Fn(crate::events::SyncStatus) + Send + Sync>>>>,

    /// 是否正在受监督的同步
    supervised_sync_running: Arc<RwLock<bool>>,

    /// Snowflake ID 生成器（用于生成 local_message_id 等唯一ID）
    snowflake: Arc<snowflake_me::Snowflake>,

    /// SDK 状态
    state: Arc<RwLock<SDKState>>,

    /// 同步运行时（用于FFI）
    sync_runtime: Option<Arc<tokio::runtime::Runtime>>,

    /// 是否已初始化
    initialized: Arc<RwLock<bool>>,

    /// 是否正在关闭
    shutting_down: Arc<RwLock<bool>>,

    /// 消息发送限流器
    message_rate_limiter: Arc<MessageRateLimiter>,

    /// RPC 限流器
    rpc_rate_limiter: Arc<RpcRateLimiter>,

    /// 重连限流器
    reconnect_rate_limiter: Arc<ReconnectRateLimiter>,

    /// HTTP 客户端（用于文件上传/下载）
    http_client: Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
    /// transport 断开时 SDK 侧接收信号（桥：Client send 后本侧 recv，执行被动 disconnect）
    transport_disconnect_rx: Arc<RwLock<Option<mpsc::UnboundedReceiver<()>>>>,
    /// 视频处理钩子（缩略图/压缩由上层实现，未设置时视频缩略图用 1x1 透明 PNG 占位）
    video_process_hook: Arc<RwLock<Option<crate::storage::media_preprocess::VideoProcessHook>>>,
    /// 文件发送队列（附件消息入此队列，2～3 消费者处理，不阻塞消息队列）
    file_send_queue: Arc<crate::storage::queue::FileSendQueue>,
    /// 文件发送消费者
    file_send_consumer: Arc<RwLock<Option<Arc<crate::storage::queue::FileConsumerRunner>>>>,
}

impl PrivchatSDK {
    /// 异步初始化 SDK（推荐方式）
    ///
    /// 分层初始化顺序：
    /// 1. 存储层 → 2. 网络层 → 3. 事件层 → 4. 业务层
    pub async fn initialize(config: PrivchatConfig) -> Result<Arc<Self>> {
        info!("正在初始化 PrivchatSDK...");

        // 验证配置
        Self::validate_config(&config)?;

        // 应用时区配置
        use crate::utils::{TimeFormatter, TimezoneConfig};
        if let Some(offset_seconds) = config.timezone_offset_seconds {
            let tz_config = TimezoneConfig { offset_seconds };
            TimeFormatter::set_timezone(tz_config);
            info!(
                "已设置时区偏移: {} 秒 ({} 小时)",
                offset_seconds,
                offset_seconds / 3600
            );
        } else {
            let tz_config = TimezoneConfig::local();
            TimeFormatter::set_timezone(tz_config);
            info!("使用系统本地时区: {} 秒偏移", tz_config.offset_seconds);
        }

        // === 第1层：存储管理器 ===
        let assets_dir = config.assets_dir.as_ref().map(|p| p.as_path());
        let storage = Arc::new(StorageManager::new(&config.data_dir, assets_dir).await?);

        // === 第2层：网络监控 ===
        // 使用默认网络监听器（假设始终在线）
        // 实际应用应该由平台层提供真实的网络状态监听实现
        let network_listener = Arc::new(DefaultNetworkStatusListener::default());
        let network = Arc::new(NetworkMonitor::new(network_listener));

        // === 第3层：事件管理器 ===
        let event_manager = Arc::new(EventManager::new(config.event_config.buffer_size));

        // === 第4层：队列管理器 ===
        // 发送队列按用户创建，在 get_queue_manager() 时懒加载 users/{uid}/queue.db，不在此处创建共享实例
        info!("队列管理器将按用户懒加载");

        // === 第5层：连接状态管理器 ===
        let platform_info = Self::get_platform_info();
        let connection_state = Arc::new(ConnectionStateManager::new(platform_info));
        info!("连接状态管理器初始化完成");

        // === 第6层：在线状态管理器 ===
        let presence_manager =
            Arc::new(crate::presence::PresenceManager::new(event_manager.clone()));
        info!("在线状态管理器初始化完成");

        // === 第7层：输入状态管理器 ===
        let typing_manager = Arc::new(crate::typing::TypingManager::new(event_manager.clone()));
        info!("输入状态管理器初始化完成");

        // === 第8层：pts 同步管理器（Phase 8）===
        let pts_manager = Arc::new(crate::sync::PtsManager::new(storage.clone()));
        info!("pts 管理器初始化完成");

        // === 第9层：生命周期管理器 ===
        let lifecycle_manager = Arc::new(tokio::sync::RwLock::new(
            crate::lifecycle::LifecycleManager::new(),
        ));
        info!("生命周期管理器初始化完成");

        // 注意：sync_engine 需要 client，将在 connect() 时初始化
        let sync_engine = Arc::new(RwLock::new(None));

        // === 第9层：客户端限流器 ===
        let message_rate_limiter =
            Arc::new(MessageRateLimiter::new(MessageRateLimiterConfig::default()));

        let rpc_rate_limiter = RpcRateLimiter::new(RpcRateLimiterConfig::default());

        let reconnect_rate_limiter = Arc::new(ReconnectRateLimiter::new(
            ReconnectRateLimiterConfig::default(),
        ));

        info!("✅ 客户端限流器初始化完成");

        // === 第10层：Snowflake ID 生成器 ===
        // 使用 Builder 手动指定 machine_id 和 data_center_id，避免 IP 地址检测失败
        // 使用随机数作为 machine_id 和 data_center_id (0-31, 5 bits each)
        // 注意：使用 StdRng 而不是 thread_rng()，因为 thread_rng() 不是 Send 的，不能在 async 函数中使用
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};
        let mut rng = StdRng::from_entropy();
        let machine_id: u16 = rng.gen_range(0..32);
        let data_center_id: u16 = rng.gen_range(0..32);

        let snowflake = snowflake_me::Snowflake::builder()
            .machine_id(&|| Ok(machine_id))
            .data_center_id(&|| Ok(data_center_id))
            .finalize()
            .map_err(|e| PrivchatSDKError::Other(format!("初始化 Snowflake 失败: {:?}", e)))?;

        info!(
            "✅ Snowflake ID 生成器初始化完成 (machine_id={}, data_center_id={})",
            machine_id, data_center_id
        );

        // === 第11层：HTTP 客户端（文件上传/下载）===
        let http_client = if let Some(ref file_api_url) = config.file_api_base_url {
            match crate::http_client::FileHttpClient::new(
                &config.http_client_config,
                Some(file_api_url.clone()),
            ) {
                Ok(client) => {
                    info!("✅ HTTP 客户端初始化完成 (base_url: {})", file_api_url);
                    Arc::new(RwLock::new(Some(Arc::new(client))))
                }
                Err(e) => {
                    warn!("⚠️ HTTP 客户端初始化失败: {}，文件上传/下载功能将不可用", e);
                    Arc::new(RwLock::new(None))
                }
            }
        } else {
            info!("ℹ️ 未配置 file_api_base_url，HTTP 客户端未初始化（将从 RPC 响应中获取 upload_url）");
            Arc::new(RwLock::new(None))
        };

        let sdk = Arc::new(Self {
            config,
            client: Arc::new(RwLock::new(None)),
            storage,
            features: Arc::new(RwLock::new(None)),
            network,
            event_manager,
            user_send_queue_managers: Arc::new(RwLock::new(HashMap::new())),
            send_consumer: Arc::new(RwLock::new(None)),
            connection_state,
            presence_manager,
            typing_manager,
            pts_manager,
            sync_engine,
            sync_observer: Arc::new(RwLock::new(None)),
            supervised_sync_running: Arc::new(RwLock::new(false)),
            snowflake: Arc::new(snowflake),
            state: Arc::new(RwLock::new(SDKState::default())),
            sync_runtime: None,
            initialized: Arc::new(RwLock::new(true)),
            shutting_down: Arc::new(RwLock::new(false)),
            message_rate_limiter,
            rpc_rate_limiter,
            reconnect_rate_limiter,
            http_client,
            lifecycle_manager,
            transport_disconnect_rx: Arc::new(RwLock::new(None)),
            video_process_hook: Arc::new(RwLock::new(None)),
            file_send_queue: Arc::new(crate::storage::queue::FileSendQueue::new()),
            file_send_consumer: Arc::new(RwLock::new(None)),
        });

        // === 每次打开清理 tmp：删除非当日目录 ===
        if let Err(e) = sdk.cleanup_tmp_files().await {
            warn!("清理 tmp 目录失败（可忽略）: {}", e);
        }

        // === 自动注册 Push 生命周期 Hook ===
        // 在 SDK 初始化时自动注册，无需用户手动注册
        // Hook 内部会检查 device_id，如果没有则跳过操作
        {
            use crate::lifecycle::PushLifecycleHook;
            let push_hook = Arc::new(PushLifecycleHook::new(sdk.clone()));
            let mut manager = sdk.lifecycle_manager.write().await;
            manager.register_hook(push_hook);
            drop(manager);
            info!("✅ Push 生命周期 Hook 已自动注册");
        }

        // === 第5层：启动队列消费者 ===

        // 注意：SendConsumer 在 connect() 成功后启动
        // 因为它需要 PrivchatClient 来发送消息

        info!("✅ PrivchatSDK 初始化完成");
        Ok(sdk)
    }

    /// 同步初始化 SDK（用于 FFI）
    pub fn initialize_blocking(config: PrivchatConfig) -> Result<Arc<Self>> {
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| PrivchatSDKError::Runtime(format!("创建运行时失败: {}", e)))?;

        let sdk = rt.block_on(async { Self::initialize(config).await })?;

        // 安全地设置同步运行时
        // 注意：这里使用 unsafe 是因为我们确保只在初始化时设置一次
        unsafe {
            let sdk_ptr = Arc::as_ptr(&sdk) as *mut PrivchatSDK;
            (*sdk_ptr).sync_runtime = Some(Arc::new(rt));
        }

        Ok(sdk)
    }

    /// 验证配置
    fn validate_config(config: &PrivchatConfig) -> Result<()> {
        if config.server_config.endpoints.is_empty() {
            return Err(PrivchatSDKError::Config(
                "至少需要配置一个服务器端点".to_string(),
            ));
        }

        // 验证每个端点配置
        for endpoint in &config.server_config.endpoints {
            if endpoint.host.is_empty() {
                return Err(PrivchatSDKError::Config("服务器主机名不能为空".to_string()));
            }

            if endpoint.port == 0 {
                return Err(PrivchatSDKError::Config("服务器端口不能为0".to_string()));
            }
        }

        if config.data_dir.as_os_str().is_empty() {
            return Err(PrivchatSDKError::Config("数据目录不能为空".to_string()));
        }

        Ok(())
    }

    /// 用户注册
    ///
    /// 注册新用户并自动连接。返回 (user_id, token)
    pub async fn register(
        &self,
        username: String,
        password: String,
        device_id: String,
        device_info: Option<privchat_protocol::protocol::DeviceInfo>,
    ) -> Result<(u64, String)> {
        use privchat_protocol::rpc::auth::UserRegisterRequest;

        info!("正在注册用户: {}", username);

        // 1. 构造注册请求
        let request = UserRegisterRequest {
            username: username.clone(),
            password,
            nickname: None,
            phone: None,
            email: None,
            device_id: device_id.clone(),
            device_info,
        };

        // 2. 直接调用 RPC（网络连接已在 SDK 初始化时建立）
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response: serde_json::Value = client
            .call_rpc_typed(routes::account_user::REGISTER, request)
            .await?;

        drop(client_guard); // 释放锁

        // 3. 解析响应
        let success = response["success"].as_bool().unwrap_or(false);
        if !success {
            let message = response["message"].as_str().unwrap_or("注册失败");
            return Err(PrivchatSDKError::Auth(message.to_string()));
        }

        let user_id = response["user_id"]
            .as_u64()
            .ok_or_else(|| PrivchatSDKError::Auth("缺少 user_id".to_string()))?;
        let token = response["token"]
            .as_str()
            .ok_or_else(|| PrivchatSDKError::Auth("缺少 token".to_string()))?
            .to_string();

        info!(
            "✅ 用户注册成功: username={}, user_id={}",
            username, user_id
        );

        // 4. 注册成功后不自动认证，由调用方决定是否调用 authenticate()

        Ok((user_id, token))
    }

    /// 用户登录
    ///
    /// 登录已有用户并自动连接。返回 (user_id, token)
    pub async fn login(
        &self,
        username: String,
        password: String,
        device_id: String,
        device_info: Option<privchat_protocol::protocol::DeviceInfo>,
    ) -> Result<(u64, String)> {
        use privchat_protocol::rpc::auth::AuthLoginRequest;

        info!("正在登录用户: {}", username);

        // 1. 构造登录请求
        let request = AuthLoginRequest {
            username: username.clone(),
            password,
            device_id: device_id.clone(),
            device_info,
        };

        // 2. 直接调用 RPC（网络连接已在 SDK 初始化时建立）
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response: serde_json::Value =
            client.call_rpc_typed(routes::auth::LOGIN, request).await?;

        drop(client_guard); // 释放锁

        // 3. 解析响应
        let success = response["success"].as_bool().unwrap_or(false);
        if !success {
            let message = response["message"].as_str().unwrap_or("登录失败");
            return Err(PrivchatSDKError::Auth(message.to_string()));
        }

        let user_id = response["user_id"]
            .as_u64()
            .ok_or_else(|| PrivchatSDKError::Auth("缺少 user_id".to_string()))?;
        let token = response["token"]
            .as_str()
            .ok_or_else(|| PrivchatSDKError::Auth("缺少 token".to_string()))?
            .to_string();

        info!(
            "✅ 用户登录成功: username={}, user_id={}",
            username, user_id
        );

        // 4. 登录成功后不自动认证，由调用方决定是否调用 authenticate()

        Ok((user_id, token))
    }

    /// 连接到服务器
    ///
    /// 使用 JWT token 进行认证
    ///
    /// 在调用此方法前必须先调用 connect() 建立网络连接
    pub async fn authenticate(
        &self,
        user_id: u64,
        token: &str,
        device_info: privchat_protocol::protocol::DeviceInfo,
    ) -> Result<()> {
        self.check_initialized().await?;

        info!("正在认证用户: user_id={}", user_id);

        // 先设置当前用户指针并完成 db/kv/queue 初始化，再发认证请求，避免服务端立即下发的 Push 落库时「用户数据库不存在」
        let user_id_str = user_id.to_string();
        self.storage.switch_user(&user_id_str).await?;
        {
            let mut state = self.state.write().await;
            state.current_user_id = Some(user_id);
        }
        info!("✅ 当前用户已设置: user_id={}", user_id);
        self.ensure_user_storage_initialized().await?;

        // 获取 client
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        // 调用 client 的 authenticate 方法（服务端可能在此后立即下发欢迎消息，此时 db/kv/queue 已就绪）
        let session = client.authenticate(user_id, token, device_info).await?;

        drop(client_guard); // 释放锁

        // 更新连接状态
        self.connection_state.mark_connected().await;
        self.connection_state
            .set_user_info(
                session.user_id.to_string(),
                session.device_id.clone(),
                session.session_id.clone(),
            )
            .await;

        // 更新服务器元数据
        if let Some(server_info) = &session.server_info {
            self.connection_state
                .set_server_metadata(
                    Some(server_info.version.clone()),
                    Some(server_info.name.clone()),
                    None,
                )
                .await;

            info!(
                "📡 服务器信息 - 版本: {}, 名称: {}, 功能: {:?}",
                server_info.version, server_info.name, server_info.features
            );
        }

        // 更新网络状态为 Online
        self.network
            .set_status(crate::network::NetworkStatus::Online)
            .await;
        info!("✅ 网络状态已更新为 Online");

        // 注意：Push 生命周期 Hook 已在 SDK 初始化时自动注册
        // 无需在此处再次注册

        info!("✅ 认证成功: user_id={}", user_id);
        Ok(())
    }

    /// 支持多协议自动降级：QUIC → TCP → WebSocket
    /// 建立到服务器的网络连接（不进行认证）
    ///
    /// 注意：此方法自动包含重连限流保护（指数退避），防止重连风暴。
    /// 频道消息同步：在 run_bootstrap_sync() 中执行（sync RPC 需已认证，bootstrap 应在 authenticate 后调用）。
    /// 好友列表同步：通过 FFI 使用时，FFI 层在 connect 成功后会自动调用
    /// 连接成功后可由上层调用 `sync_entities_in_background(EntityType::Friend, None)`，保证 local-first 下创建群组等可立即用本地好友。
    pub async fn connect(&self) -> Result<()> {
        self.check_initialized().await?;

        info!("正在建立网络连接...");

        // 🔥 检查重连限流（指数退避保护）
        if let Err(wait_duration) = self.reconnect_rate_limiter.check_reconnect() {
            info!(
                "连接受限，等待 {}s（指数退避：防止重连风暴）",
                wait_duration.as_secs()
            );
            tokio::time::sleep(wait_duration).await;
        }

        // 设置连接状态
        {
            let mut state = self.state.write().await;
            state.connection_state = ConnectionState::Connecting;
        }

        // 尝试按优先级顺序连接不同协议
        let mut last_error = None;

        for endpoint in &self.config.server_config.endpoints {
            match self.try_connect_with_endpoint(endpoint).await {
                Ok(()) => {
                    info!("成功使用 {:?} 协议连接到服务器", endpoint.protocol);

                    // 🔥 连接成功，重置重连限流器
                    self.reconnect_rate_limiter.mark_success();
                    info!("✅ 连接成功，重置重连计数器");

                    // 更新连接状态
                    {
                        let mut state = self.state.write().await;
                        state.connection_state = ConnectionState::Connected;
                        state.last_connected = Some(Instant::now());
                        state.current_protocol = Some(endpoint.protocol.clone());
                    }

                    // 触发连接状态变化事件
                    let connection_event = crate::events::event_builders::connection_state_changed(
                        EventConnectionState::Connecting,
                        EventConnectionState::Connected,
                    );
                    self.event_manager.emit(connection_event).await;

                    return Ok(());
                }
                Err(e) => {
                    warn!("使用 {:?} 协议连接失败: {}", endpoint.protocol, e);
                    last_error = Some(e);
                }
            }
        }

        // 所有协议都连接失败
        {
            let mut state = self.state.write().await;
            state.connection_state = ConnectionState::Disconnected;
            state.last_disconnected = Some(Instant::now());
        }

        let error = last_error
            .unwrap_or_else(|| PrivchatSDKError::Transport("没有可用的传输协议".to_string()));

        // 触发连接失败事件
        let connection_event = crate::events::event_builders::connection_state_changed(
            EventConnectionState::Connecting,
            EventConnectionState::Disconnected,
        );
        self.event_manager.emit(connection_event).await;

        Err(error)
    }

    /// 尝试使用指定端点连接
    async fn try_connect_with_endpoint(&self, endpoint: &ServerEndpoint) -> Result<()> {
        let server_url = self.build_server_url_from_endpoint(endpoint);

        info!("尝试连接到: {} (协议: {:?})", server_url, endpoint.protocol);

        // ========== 0. 建立 transport 断开桥（Client → SDK 单向信号） ==========
        let (disconnect_tx, disconnect_rx) = mpsc::unbounded_channel::<()>();
        *self.transport_disconnect_rx.write().await = Some(disconnect_rx);

        // ========== 1. 创建 PrivchatClient ==========

        let client_endpoint = endpoint.to_client_endpoint();

        let mut client = PrivchatClient::new(
            &self.config.data_dir,
            vec![client_endpoint],
            Duration::from_secs(self.config.connection_timeout),
        )
        .await?;

        // ========== 1.1 设置 RPC 限流器 ==========
        client.set_rpc_rate_limiter(self.rpc_rate_limiter.clone());

        // ========== 2. 设置消息接收器（连接前） ==========

        let (message_tx, mut message_rx) = mpsc::unbounded_channel::<PushMessageRequest>();
        client.set_message_receiver(message_tx);

        // ========== 3. 启动消息分发任务 ==========

        let event_manager = self.event_manager.clone();
        let storage = self.storage.clone();
        let connection_state = self.connection_state.clone();
        let pts_manager = self.pts_manager.clone();
        let sync_engine_ref = self.sync_engine.clone();
        let data_dir = self.config.data_dir.clone();
        let file_api_base_url = self.config.file_api_base_url.clone();
        let http_client = self.http_client.clone();

        tokio::spawn(async move {
            debug!("消息分发任务已启动");
            // 离线推送批处理窗口：收到第一条后等待短时，收齐同批消息再按 pts 发一次 Append，避免 3、2、1 倒序（不宜过大，否则欢迎消息等单条会延迟落库，get_messages 先于保存被调用）
            const PUSH_BATCH_DELAY_MS: u64 = 5;
            while let Some(first_push) = message_rx.recv().await {
                let mut batch: Vec<PushMessageRequest> = vec![first_push];
                tokio::time::sleep(Duration::from_millis(PUSH_BATCH_DELAY_MS)).await;
                while let Ok(next) = message_rx.try_recv() {
                    batch.push(next);
                }
                batch.sort_by_key(|m| (m.channel_id, m.channel_type, m.message_seq));

                // 按 channel 分组，用于最后按 channel 批量发一次 TimelineDiff Append
                let mut channel_timeline: HashMap<(u64, i32), Vec<crate::events::TimelineMessage>> =
                    HashMap::new();

                for push_msg in batch {
                    debug!(
                        "收到推送消息: message_id={}, from={}",
                        push_msg.server_message_id, push_msg.from_uid
                    );

                    // 更新接收统计
                    let payload_len = push_msg.payload.len() as u64;
                    connection_state.increment_received(payload_len).await;

                    // ========== 保存到本地数据库 ==========

                    let content = if let Ok(payload_json) =
                        serde_json::from_slice::<serde_json::Value>(&push_msg.payload)
                    {
                        payload_json
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else {
                        String::from_utf8_lossy(&push_msg.payload).to_string()
                    };

                    // 消息类型来自协议层 PushMessageRequest.message_type（u32），不再从 payload 解析
                    use crate::storage::entities::Message;
                    use chrono::Utc;

                    let timestamp_ms: i64 = {
                        let ts = push_msg.timestamp as u64;
                        if ts < 1_000_000_000_000u64 {
                            (ts * 1000) as i64
                        } else {
                            ts as i64
                        }
                    };

                    debug!("[Rust SDK] 💾 保存接收消息: message_id={}, content={}, timestamp={} (原始={})",
                           push_msg.server_message_id,
                           content.chars().take(50).collect::<String>(),
                           timestamp_ms,
                           push_msg.timestamp);

                    let message = Message {
                        id: None,
                        server_message_id: Some(push_msg.server_message_id),
                        pts: push_msg.message_seq as i64,
                        channel_id: push_msg.channel_id,
                        channel_type: push_msg.channel_type as i32,
                        timestamp: Some(timestamp_ms),
                        from_uid: push_msg.from_uid,
                        message_type: push_msg.message_type as i32,
                        content: content.clone(),
                        status: 2,
                        voice_status: 0,
                        created_at: Utc::now().timestamp_millis(),
                        updated_at: Utc::now().timestamp_millis(),
                        searchable_word: content.clone(),
                        local_message_id: 0,
                        is_deleted: 0,
                        setting: push_msg.setting.need_receipt as i32,
                        order_seq: push_msg.message_seq as i64,
                        extra: "{}".to_string(),
                        flame: 0,
                        flame_second: 0,
                        viewed: 0,
                        viewed_at: 0,
                        topic_id: push_msg.topic.clone(),
                        expire_time: if push_msg.expire > 0 {
                            Some(push_msg.expire as i64)
                        } else {
                            None
                        },
                        expire_timestamp: None,
                        revoked: 0,
                        revoked_at: 0,
                        revoked_by: None,
                    };

                    let row_id = match storage.save_received_message(&message, false).await {
                        Ok(id) => id,
                        Err(e) => {
                            warn!("保存接收消息到数据库失败: {}", e);
                            0
                        }
                    };
                    if row_id > 0 {
                        debug!(
                            "✅ 消息已保存到数据库: message_id={}, row_id={}",
                            push_msg.server_message_id, row_id
                        );
                        // 有附件的消息：后台下载缩略图到 {data_dir}/users/{uid}/files/{yyyymm}/{message.id}/
                        if let Some(uid) = storage.get_current_user_id().await {
                            let content_for_thumb = content.clone();
                            let created_at_ms = message.created_at;
                            let msg_row_id = row_id;
                            let data_dir_thumb = data_dir.clone();
                            let base_url_thumb = file_api_base_url.clone();
                            let http_thumb = http_client.clone();
                            tokio::spawn(async move {
                                if let Err(e) = Self::download_thumbnail_after_receive(
                                    data_dir_thumb,
                                    base_url_thumb,
                                    http_thumb,
                                    uid,
                                    msg_row_id,
                                    content_for_thumb,
                                    created_at_ms,
                                )
                                .await
                                {
                                    warn!(
                                        "下载消息缩略图失败: row_id={}, error={:?}",
                                        msg_row_id, e
                                    );
                                }
                            });
                        }
                    }

                    let server_pts = push_msg.message_seq;
                    let channel_id = push_msg.channel_id;
                    let channel_type = push_msg.channel_type as u8;

                    match pts_manager
                        .has_gap(channel_id, channel_type, server_pts as u64)
                        .await
                    {
                        Ok(true) => {
                            warn!(
                                "检测到 pts 间隙: channel_id={}, channel_type={}",
                                channel_id, channel_type
                            );
                            if let Some(sync_engine) = sync_engine_ref.read().await.as_ref() {
                                let sync_engine_clone = sync_engine.clone();
                                tokio::spawn(async move {
                                    info!("开始补齐同步: channel_id={}", channel_id);
                                    match sync_engine_clone
                                        .sync_channel(channel_id, channel_type)
                                        .await
                                    {
                                        Ok(state) => info!(
                                            "补齐同步完成: channel_id={}, state={:?}",
                                            channel_id, state.state
                                        ),
                                        Err(e) => error!(
                                            "补齐同步失败: channel_id={}, error={:?}",
                                            channel_id, e
                                        ),
                                    }
                                });
                            }
                        }
                        Ok(false) => {
                            if let Err(e) = pts_manager
                                .update_local_pts(channel_id, channel_type, server_pts as u64)
                                .await
                            {
                                warn!("更新本地 pts 失败: {:?}", e);
                            }
                        }
                        Err(e) => warn!("间隙检测失败: {:?}", e),
                    }

                    let timestamp_ms_u64: u64 = {
                        let ts = push_msg.timestamp as u64;
                        if ts < 1_000_000_000_000u64 {
                            ts * 1000
                        } else {
                            ts
                        }
                    };

                    let event = SDKEvent::MessageReceived {
                        server_message_id: push_msg.server_message_id,
                        channel_id: push_msg.channel_id,
                        channel_type: push_msg.channel_type as i32,
                        from_uid: push_msg.from_uid,
                        timestamp: timestamp_ms_u64,
                        content: content.clone(),
                    };
                    event_manager.emit(event.clone()).await;

                    // 加入本 channel 的 Timeline 批次，最后统一发一次 Append（按 pts 已排序）
                    use crate::events::TimelineMessage;
                    let timeline_message = TimelineMessage {
                        id: push_msg.server_message_id,
                        server_message_id: Some(push_msg.server_message_id),
                        channel_id: push_msg.channel_id,
                        channel_type: push_msg.channel_type as i32,
                        from_uid: push_msg.from_uid,
                        content: content.clone(),
                        message_type: push_msg.message_type as i32,
                        timestamp: timestamp_ms_u64,
                        pts: push_msg.message_seq as u64,
                    };
                    channel_timeline
                        .entry((push_msg.channel_id, push_msg.channel_type as i32))
                        .or_default()
                        .push(timeline_message);

                    // 触发会话列表更新事件（ChannelListUpdate）
                    let storage_clone = storage.clone();
                    let event_manager_clone = event_manager.clone();
                    let channel_id = push_msg.channel_id;
                    let channel_type = push_msg.channel_type as i32;
                    let content_clone = content.clone();
                    let timestamp = push_msg.timestamp as u64;
                    tokio::spawn(async move {
                        // 1. 获取会话信息
                        let query = crate::storage::entities::ChannelQuery {
                            limit: None,
                            offset: None,
                            channel_id: Some(channel_id),
                            channel_type: Some(channel_type),
                            ..Default::default()
                        };

                        let conv = match storage_clone.get_channels(&query).await {
                            Ok(channels) => channels.first().cloned(),
                            Err(e) => {
                                warn!("获取会话信息失败: {:?}", e);
                                None
                            }
                        };

                        // 2. 获取频道信息（名称、头像等）
                        let channel =
                            match storage_clone.get_channel(channel_id, channel_type).await {
                                Ok(ch) => ch,
                                Err(e) => {
                                    warn!("获取频道信息失败: {:?}", e);
                                    None
                                }
                            };

                        // 3. 获取群组成员数量（如果是群聊）
                        let member_count = if channel_type == 2 {
                            match storage_clone
                                .get_group_members(channel_id, None, None)
                                .await
                            {
                                Ok(members) => members.len() as u32,
                                Err(_) => 0,
                            }
                        } else {
                            0
                        };

                        // 4. 构建 ChannelListEntry
                        if let Some(conv) = conv {
                            use crate::events::{ChannelListUpdateKind, LatestChannelEvent};

                            let latest_event = Some(LatestChannelEvent {
                                event_type: "message".to_string(),
                                content: content_clone,
                                timestamp,
                            });

                            let entry = Self::build_channel_list_entry(
                                &conv,
                                channel.as_ref(),
                                member_count,
                                latest_event,
                            );

                            let conv_event = SDKEvent::ChannelListUpdate {
                                update_kind: ChannelListUpdateKind::Update { channel: entry },
                                timestamp,
                            };
                            event_manager_clone.emit(conv_event).await;
                        }
                    });
                }

                // 按 channel 批量发一次 TimelineDiff Append（消息已按 pts 升序），避免客户端「插头部」导致 3、2、1 倒序
                use crate::events::TimelineDiffKind;
                for ((channel_id, _channel_type), messages) in channel_timeline {
                    if messages.is_empty() {
                        continue;
                    }
                    let timestamp = messages.iter().map(|m| m.timestamp).max().unwrap_or(0);
                    let timeline_event = SDKEvent::TimelineDiff {
                        channel_id,
                        diff_kind: TimelineDiffKind::Append { messages },
                        timestamp,
                    };
                    event_manager.emit(timeline_event).await;
                }
            }
            debug!("消息分发任务已结束");
        });

        // ========== 4. 建立网络连接（不进行认证） ==========

        client.connect().await?;
        info!("✅ 网络连接建立成功");

        // 更新连接状态
        let protocol = match endpoint.protocol {
            TransportProtocol::Quic => ConnectionProtocol::Quic,
            TransportProtocol::Tcp => ConnectionProtocol::Tcp,
            TransportProtocol::WebSocket => ConnectionProtocol::WebSocket,
        };
        self.connection_state.set_protocol(protocol).await;
        self.connection_state
            .set_server_info(
                format!("{}:{}", endpoint.host, endpoint.port),
                endpoint.use_tls,
            )
            .await;

        // ========== 5. 注入断开桥 sender，再保存客户端实例 ==========
        client.set_transport_disconnect_sender(disconnect_tx);
        *self.client.write().await = Some(client);

        // ========== 6. 初始化同步引擎（Phase 8）==========

        let commit_applier = Arc::new(crate::sync::CommitApplier::new(
            self.storage.clone(),
            Some(self.event_manager.clone()),
        ));

        let sync_engine = Arc::new(crate::sync::SyncEngine::new(
            self.client.clone(),
            self.pts_manager.clone(),
            commit_applier,
        ));

        *self.sync_engine.write().await = Some(sync_engine.clone());
        info!("✅ 同步引擎已初始化");

        // 注意：初始同步（batch_sync_channels）已移至 authenticate() 成功后执行，
        // 因为 sync/batch_get_channel_pts 等 RPC 需要认证，在 connect() 时 session 尚未绑定用户会导致认证失败。

        Ok(())
    }

    /// 获取当前用户的发送队列管理器（按用户独立：users/{uid}/queue.db）
    async fn get_queue_manager(&self) -> Result<Arc<SendQueueManager>> {
        let uid = self
            .storage()
            .get_current_user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("未登录，无法获取发送队列".to_string()))?;
        let user_dir = self.storage().user_dir(&uid);
        let mut map = self.user_send_queue_managers.write().await;
        if let Some(mgr) = map.get(&uid).cloned() {
            return Ok(mgr);
        }
        tokio::fs::create_dir_all(&user_dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;
        let queue_db_path = user_dir.join("queue.db");
        let queue_db = sled::open(&queue_db_path)
            .map_err(|e| PrivchatSDKError::KvStore(format!("打开队列数据库失败: {}", e)))?;
        let mgr = Arc::new(SendQueueManager::new(Arc::new(queue_db)));
        map.insert(uid.clone(), mgr.clone());
        Ok(mgr)
    }

    /// 启动 SendConsumer（消息发送队列消费者）
    async fn start_send_consumer(&self) -> Result<()> {
        // 检查是否已经启动
        {
            let consumer_guard = self.send_consumer.read().await;
            if consumer_guard.is_some() {
                debug!("SendConsumer 已经启动，跳过");
                return Ok(());
            }
        }

        let queue_manager = self.get_queue_manager().await?;

        use crate::storage::queue::retry_policy::{RetryManager, RetryPolicy};
        use crate::storage::queue::send_consumer::{SendConsumerConfig, SendConsumerRunner};

        // 传递 StorageManager 给 SendConsumer（它会负责数据库操作）
        let storage_manager = self.storage.clone();

        let retry_manager = Arc::new(RetryManager::new(RetryPolicy::default()));

        let consumer = SendConsumerRunner::new(
            SendConsumerConfig::default(),
            queue_manager.clone() as Arc<dyn crate::storage::queue::TaskQueueTrait>,
            storage_manager,
            self.client.clone(), // 直接传递 client，不再使用 NetworkSender
            self.network.clone(),
            retry_manager,
            self.message_rate_limiter.clone(), // 传递消息限流器
            self.event_manager.clone(),        // 传递事件管理器
        );

        let consumer_arc = Arc::new(consumer);

        // 启动消费者
        consumer_arc.start().await?;
        info!("✅ 消息发送消费者已启动");

        // 保存到 SDK
        *self.send_consumer.write().await = Some(consumer_arc);

        Ok(())
    }

    /// 启动文件发送消费者（2～3 个 worker，处理附件上传与发消息）
    async fn start_file_send_consumer(&self) -> Result<()> {
        {
            let guard = self.file_send_consumer.read().await;
            if guard.is_some() {
                debug!("FileConsumer 已经启动，跳过");
                return Ok(());
            }
        }
        use crate::storage::queue::{FileConsumerConfig, FileConsumerRunner};
        let file_config = FileConsumerConfig {
            image_send_max_edge: self.config.image_send_max_edge,
            ..FileConsumerConfig::default()
        };
        let runner = FileConsumerRunner::new(
            file_config,
            self.file_send_queue.clone(),
            self.config.data_dir.clone(),
            self.storage.clone(),
            self.client.clone(),
            self.http_client.clone(),
            self.event_manager.clone(),
        );
        let arc = Arc::new(runner);
        arc.start().await?;
        info!("✅ 文件发送消费者已启动");
        *self.file_send_consumer.write().await = Some(arc);
        Ok(())
    }

    /// 每次打开 App 时清理 tmp：删除非当日目录
    async fn cleanup_tmp_files(&self) -> Result<()> {
        use crate::storage::media_preprocess::today_yyyymmdd;
        let today = today_yyyymmdd();
        let users_dir = self.config.data_dir.join("users");
        if !users_dir.exists() {
            return Ok(());
        }
        let mut entries = tokio::fs::read_dir(&users_dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("读取 users 目录失败: {}", e)))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("遍历 users 失败: {}", e)))?
        {
            let uid_dir = entry.path();
            if !uid_dir.is_dir() {
                continue;
            }
            let tmp_dir = uid_dir.join("files").join("tmp");
            if !tmp_dir.exists() {
                continue;
            }
            let mut tmp_entries = tokio::fs::read_dir(&tmp_dir)
                .await
                .map_err(|e| PrivchatSDKError::IO(format!("读取 tmp 目录失败: {}", e)))?;
            while let Some(t) = tmp_entries
                .next_entry()
                .await
                .map_err(|e| PrivchatSDKError::IO(format!("遍历 tmp 失败: {}", e)))?
            {
                let name_str = t.file_name().to_string_lossy().into_owned();
                let is_dir = t.file_type().await.map(|ft| ft.is_dir()).unwrap_or(false);
                if name_str != today && is_dir {
                    let _ = tokio::fs::remove_dir_all(t.path()).await;
                }
            }
        }
        Ok(())
    }

    /// 从端点构建服务器URL
    fn build_server_url_from_endpoint(&self, endpoint: &ServerEndpoint) -> String {
        match endpoint.protocol {
            TransportProtocol::Quic => {
                // QUIC强制使用TLS
                format!("quic://{}:{}", endpoint.host, endpoint.port)
            }
            TransportProtocol::Tcp => {
                // TCP通常不使用TLS前缀
                format!("tcp://{}:{}", endpoint.host, endpoint.port)
            }
            TransportProtocol::WebSocket => {
                let protocol_prefix = if endpoint.use_tls { "wss" } else { "ws" };
                let base_url = format!("{}://{}:{}", protocol_prefix, endpoint.host, endpoint.port);
                if let Some(ref path) = endpoint.path {
                    format!("{}{}", base_url, path)
                } else {
                    base_url
                }
            }
        }
    }

    /// 断开连接
    pub async fn disconnect(&self) -> Result<()> {
        // 只有在不是关闭过程中时才检查初始化状态
        if !self.is_shutting_down().await {
            self.check_initialized().await?;
        }

        info!("正在断开连接...");

        // 更新连接状态管理器
        self.connection_state.mark_disconnected().await;

        // 更新连接状态
        {
            let mut state = self.state.write().await;
            state.connection_state = ConnectionState::Disconnected;
            state.last_disconnected = Some(Instant::now());
            state.current_protocol = None;
            state.current_user_id = None;
        }

        // 停止 SendConsumer
        {
            let mut consumer_guard = self.send_consumer.write().await;
            if let Some(consumer) = consumer_guard.take() {
                if let Err(e) = consumer.stop().await {
                    warn!("停止 SendConsumer 失败: {}", e);
                } else {
                    info!("✅ SendConsumer 已停止");
                }
            }
        }

        // 停止文件发送消费者
        {
            let mut guard = self.file_send_consumer.write().await;
            if let Some(consumer) = guard.take() {
                if let Err(e) = consumer.stop().await {
                    warn!("停止 FileConsumer 失败: {}", e);
                } else {
                    info!("✅ FileConsumer 已停止");
                }
            }
        }

        // 断开传输层客户端
        if let Some(_client) = self.client.read().await.as_ref() {
            // client.disconnect("用户主动断开").await?;
        }

        // 清理高级特性
        *self.features.write().await = None;
        *self.client.write().await = None;

        // 触发断开连接事件
        let connection_event = crate::events::event_builders::connection_state_changed(
            EventConnectionState::Connected,
            EventConnectionState::Disconnected,
        );
        self.event_manager.emit(connection_event).await;

        info!("连接已断开");
        Ok(())
    }

    /// 首次自动重连间隔（秒），类似微信：掉线后先等几秒再重连
    const AUTO_RECONNECT_FIRST_DELAY_SECS: u64 = 3;

    /// 启动「transport 断开 → SDK 状态」桥的监听任务（由持有 Arc<Self> 的调用方在 connect 成功后调用）
    ///
    /// 从 `transport_disconnect_rx` 取走 receiver，spawn 任务：recv 到信号则执行 `disconnect()`，
    /// 然后按间隔+退避自动重连（首次 3s，后续由 ReconnectRateLimiter 指数退避），直到成功或 SDK 关闭。
    pub async fn start_transport_disconnect_listener(self: Arc<Self>) {
        let rx = self.transport_disconnect_rx.write().await.take();
        if let Some(mut rx) = rx {
            tokio::spawn(async move {
                while rx.recv().await.is_some() {
                    if let Err(e) = self.disconnect().await {
                        warn!("被动断开（transport 已断）执行 disconnect 失败: {}", e);
                        continue;
                    }
                    info!("✅ 被动断开已同步到 SDK 状态并已发出 ConnectionStateChanged");

                    // 自动重连：先发「重连中」状态，再按间隔重试（首次 3s，后续由限流器退避）
                    {
                        let mut state = self.state.write().await;
                        state.connection_state = ConnectionState::Reconnecting;
                    }
                    self.event_manager
                        .emit(crate::events::event_builders::connection_state_changed(
                            EventConnectionState::Disconnected,
                            EventConnectionState::Reconnecting,
                        ))
                        .await;
                    info!(
                        "开始自动重连（首次 {}s 后尝试）",
                        Self::AUTO_RECONNECT_FIRST_DELAY_SECS
                    );

                    tokio::time::sleep(Duration::from_secs(Self::AUTO_RECONNECT_FIRST_DELAY_SECS))
                        .await;

                    loop {
                        if self.is_shutting_down().await {
                            info!("SDK 正在关闭，停止自动重连");
                            break;
                        }
                        match self.connect().await {
                            Ok(()) => {
                                info!("自动重连成功");
                                break;
                            }
                            Err(e) => {
                                warn!("自动重连失败: {}，将由限流器退避后再次尝试", e);
                            }
                        }
                    }
                }
            });
            info!("transport 断开桥监听任务已启动（含自动重连）");
        }
    }

    /// 登出
    ///
    /// 清除当前用户会话，断开连接，并清理相关状态。
    ///
    /// # 返回
    /// - `Ok(())`: 登出成功
    ///
    /// # 注意
    /// - 登出后会清除本地认证信息
    /// - 会断开网络连接
    /// - 不会清除本地消息数据（除非明确调用清理方法）
    pub async fn logout(&self) -> Result<()> {
        info!("正在登出...");

        // 1. 断开网络连接
        if self.is_connected().await {
            self.disconnect().await?;
        }

        // 2. 清除客户端状态
        {
            let mut client_guard = self.client.write().await;
            *client_guard = None;
        }

        // 3. 停止发送消费者
        {
            let consumer_guard = self.send_consumer.read().await;
            if let Some(consumer) = consumer_guard.as_ref() {
                if consumer.is_running().await {
                    consumer.stop().await?;
                }
            }
        }

        // 4. 清除同步引擎
        {
            let mut sync_engine_guard = self.sync_engine.write().await;
            *sync_engine_guard = None;
        }

        // 5. 停止受监督的同步
        if *self.supervised_sync_running.read().await {
            self.stop_supervised_sync().await?;
        }

        // 6. 清除连接状态
        self.connection_state.mark_disconnected().await;

        // 7. 清除用户ID（通过更新状态）
        {
            let mut state_guard = self.state.write().await;
            state_guard.current_user_id = None;
        }

        // 8. 清除存储层的当前用户信息
        // 注意：这里不清除数据库，只清除内存中的用户信息
        // 如果需要清除数据库，应该调用专门的清理方法

        info!("✅ 登出成功");
        Ok(())
    }

    /// 进入前台
    ///
    /// 当应用进入前台时调用，用于恢复连接和同步。
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn enter_foreground(&self) -> Result<()> {
        self.check_initialized().await?;

        info!("应用进入前台");

        // 1. 恢复网络连接（如果之前已认证）
        if self.user_id().await.is_some() && !self.is_connected().await {
            // 如果有用户ID但未连接，尝试重新连接
            // 注意：这里需要 token，但 token 可能已过期
            // 实际实现中，应该由调用方在进入前台后重新认证
            warn!("检测到用户已登录但未连接，需要重新认证");
        }

        // 2. 恢复发送队列（如果之前被禁用）
        // 这里可以根据需要自动恢复发送队列

        // 3. 触发同步检查
        // 可以在这里触发一次同步检查，确保数据是最新的

        Ok(())
    }

    /// 进入后台
    ///
    /// 当应用进入后台时调用，用于暂停非关键操作。
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn enter_background(&self) -> Result<()> {
        self.check_initialized().await?;

        info!("应用进入后台");

        // 1. 暂停发送队列（可选，根据需求决定）
        // 这里可以选择暂停发送队列，或者继续在后台发送

        // 2. 减少同步频率（可选）
        // 可以降低同步频率以节省资源

        // 3. 保存状态
        // 确保重要状态已保存

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
            rt.block_on(async { self.shutdown().await })
        } else {
            // 如果没有同步运行时，创建一个临时的
            let rt = tokio::runtime::Runtime::new()
                .map_err(|e| PrivchatSDKError::Runtime(format!("创建运行时失败: {}", e)))?;
            rt.block_on(async { self.shutdown().await })
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
    ///
    /// 与 state.connection_state 一致：有 client 或 state 为 Connected 即视为已连接，
    /// 避免仅 client 被清空但 state 未同步时误判未连接（导致 bootstrap 等报 Disconnected）。
    pub async fn is_connected(&self) -> bool {
        if self.client.read().await.is_some() {
            return true;
        }
        let state = self.state.read().await;
        state.connection_state == ConnectionState::Connected
    }

    // ========== 消息操作 ==========

    /// 发送消息（队列化发送）
    ///
    /// 流程：
    /// 1. 先保存到本地数据库（status = pending）
    /// 2. 加入发送队列（持久化到 sled）
    /// 3. 立即返回消息ID
    /// 4. SendConsumer 异步处理队列，实际发送
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `content`: 消息内容
    /// - `options`: 发送选项（可选，默认使用 `SendMessageOptions::default()`）
    ///
    /// # 返回
    /// - `Ok(u64)`: 返回 local_message_id（用于跟踪发送状态）
    pub async fn send_message(&self, channel_id: u64, content: &str) -> Result<u64> {
        self.send_message_with_options(
            channel_id,
            content,
            crate::events::SendMessageOptions::default(),
        )
        .await
    }

    /// 发送消息（带选项）
    ///
    /// 这是发送消息的核心方法，支持回复、@提及等扩展功能。
    /// 设计原则：回复是消息属性，不是消息类型。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `content`: 消息内容
    /// - `options`: 发送选项（回复、提及等）
    ///
    /// # 返回
    /// - `Ok(u64)`: 返回 local_message_id（用于跟踪发送状态）
    pub async fn send_message_with_options(
        &self,
        channel_id: u64,
        content: &str,
        options: crate::events::SendMessageOptions,
    ) -> Result<u64> {
        info!(
            "🔍 [DEBUG] send_message 开始: channel_id={}, content={}",
            channel_id, content
        );

        self.check_initialized().await?;
        info!("🔍 [DEBUG] check_initialized 通过");

        self.check_connected().await?;
        info!("🔍 [DEBUG] check_connected 通过");

        // 获取当前用户 ID
        info!("🔍 [DEBUG] 准备获取 user_id");
        let user_id = self.user_id().await.ok_or_else(|| {
            warn!("❌ user_id() 返回 None");
            PrivchatSDKError::NotConnected
        })?;
        info!("🔍 [DEBUG] user_id 获取成功: {}", user_id);

        info!(
            "准备发送消息: channel_id={}, content={}",
            channel_id, content
        );

        // 本地雪花算法生成 local_message_id，发往服务端用于去重与幂等（服务端要求非 0）
        let local_message_id = self
            .snowflake
            .next_id()
            .map_err(|e| PrivchatSDKError::Other(format!("生成 local_message_id 失败: {:?}", e)))?;

        // 合约 v1：in_reply_to 为 message.id，发送前查库填 server_message_id 到 extra（供协议层使用）
        let reply_to_server_id = if let Some(reply_id) = options.in_reply_to_message_id {
            self.storage
                .get_message_by_id(reply_id as i64)
                .await
                .ok()
                .flatten()
                .and_then(|m| m.server_message_id)
                .unwrap_or(0)
        } else {
            0u64
        };

        // ========== 1. 先插入本地数据库，得到 message.id ==========
        use crate::storage::entities::Message;
        use chrono::Utc;

        let now_millis = Utc::now().timestamp_millis();
        let message = Message {
            id: None,
            server_message_id: None,
            pts: now_millis,
            channel_id,
            channel_type: 1,
            timestamp: Some(now_millis),
            from_uid: user_id,
            message_type: 1,
            content: content.to_string(),
            status: 0,
            voice_status: 0,
            created_at: now_millis,
            updated_at: now_millis,
            searchable_word: content.to_string(),
            local_message_id,
            is_deleted: 0,
            setting: 0,
            order_seq: now_millis,
            extra: {
                let mut extra_obj = serde_json::json!({});
                if reply_to_server_id != 0 {
                    extra_obj["reply_to_message_id"] = serde_json::json!(reply_to_server_id);
                }
                if !options.mentions.is_empty() {
                    extra_obj["mentioned_user_ids"] = serde_json::json!(options.mentions);
                }
                if options.silent {
                    extra_obj["silent"] = serde_json::json!(true);
                }
                if let Some(client_extra) = &options.extra {
                    if let Some(obj) = client_extra.as_object() {
                        for (key, value) in obj {
                            extra_obj[key] = value.clone();
                        }
                    }
                }
                extra_obj.to_string()
            },
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: String::new(),
            expire_time: None,
            expire_timestamp: None,
            revoked: 0,
            revoked_at: 0,
            revoked_by: None,
        };

        let row_id = self.storage.save_received_message(&message, true).await?;
        info!(
            "✅ 发送消息已保存到数据库: row_id={}, status=pending",
            row_id
        );

        // 触发会话列表更新事件（发送消息后也需要更新会话列表显示）
        self.emit_channel_list_update(channel_id, 1).await;

        // ========== 2. 加入发送队列（按 message.id，携带 local_message_id 供协议层） ==========
        let mut message_data = MessageData::new(
            row_id,
            channel_id,
            1,
            user_id,
            content.to_string(),
            1,
            local_message_id,
        );
        if reply_to_server_id != 0 {
            message_data = message_data.with_extra(
                "reply_to_message_id".to_string(),
                reply_to_server_id.to_string(),
            );
        }
        if !options.mentions.is_empty() {
            message_data = message_data.with_extra(
                "mentioned_user_ids".to_string(),
                serde_json::to_string(&options.mentions).unwrap_or_default(),
            );
        }
        if options.silent {
            message_data = message_data.with_extra("silent".to_string(), "true".to_string());
        }
        if let Some(client_extra) = &options.extra {
            if let Some(obj) = client_extra.as_object() {
                for (key, value) in obj {
                    let value_str = if value.is_string() {
                        value.as_str().unwrap_or("").to_string()
                    } else {
                        value.to_string()
                    };
                    message_data = message_data.with_extra(key.clone(), value_str);
                }
            }
        }

        let send_task = SendTask::new(row_id, channel_id, message_data, QueuePriority::Normal);

        let queue_manager = self.get_queue_manager().await?;
        queue_manager.persist_task(&send_task)?;
        queue_manager.enqueue_task(send_task.clone());
        self.notify_send_status_enqueued(&send_task).await;

        {
            let consumer_guard = self.send_consumer.read().await;
            if consumer_guard.is_none() {
                drop(consumer_guard);
                if let Err(e) = self.start_send_consumer().await {
                    error!("❌ 启动 SendConsumer 失败: {}", e);
                    return Err(e);
                }
            }
        }

        self.connection_state
            .increment_sent(content.len() as u64)
            .await;
        info!("✅ 消息已加入发送队列: id={}", row_id);

        Ok(row_id as u64)
    }

    // ========== 文件上传/下载 ==========

    /// 从文件路径发送附件（世界级 IM SDK 标准 API）
    ///
    /// 流程：
    /// 1. RPC: file/request_upload_token → 获取 { upload_token, upload_url }
    /// 2. HTTP: POST upload_url (multipart, header: X-Upload-Token) → 返回 { file_id, file_url }
    /// 3. 发送消息: content 包含 file_id 和 file_url
    ///
    /// 注意：文件发送 = 消息发送的一种，不是两个 API
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `path`: 文件路径
    /// - `options`: 发送选项（回复、提及等）
    /// - `progress`: 进度回调（可选）
    ///
    /// # 返回
    /// - `Ok((u64, AttachmentInfo))`: 返回 (local_message_id, 附件信息)
    pub async fn send_attachment_from_path(
        &self,
        channel_id: u64,
        path: impl AsRef<Path>,
        _options: crate::events::SendMessageOptions,
        _progress: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<(u64, crate::events::AttachmentInfo)> {
        let file_path = path.as_ref();
        info!("📤 开始发送附件（从文件路径）: {}", file_path.display());

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self
            .user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        let uid = user_id.to_string();
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "file".to_string());
        let (file_type_str, mime_type) = self.detect_file_type_and_mime(file_path)?;
        let file_size = tokio::fs::metadata(file_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0) as i64;

        let (send_mode, message_type_str, message_type_i32) = match file_type_str.as_str() {
            "image" => (
                crate::storage::media_preprocess::SendMode::Image,
                "image".to_string(),
                2,
            ),
            "video" => (
                crate::storage::media_preprocess::SendMode::Video,
                "video".to_string(),
                3,
            ),
            _ => (
                crate::storage::media_preprocess::SendMode::Document,
                "file".to_string(),
                4,
            ),
        };

        let local_message_id = self
            .snowflake
            .next_id()
            .map_err(|e| PrivchatSDKError::Other(format!("生成 local_message_id 失败: {:?}", e)))?;
        let channel_type = 1i32;
        let message_id = self
            .storage
            .send_message(channel_id, channel_type, user_id, "{}", message_type_i32)
            .await?;
        let timestamp_ms = chrono::Utc::now().timestamp_millis();

        let video_hook = self.video_process_hook.read().await.clone();
        // 视频且未设置缩略图回调：先上传 1x1 透明 PNG 占位图，拿到 thumbnail_file_id，消费者直接使用不再上传缩略图
        let pre_uploaded_thumbnail_file_id = if message_type_str == "video" && video_hook.is_none()
        {
            let thumb_token_req =
                privchat_protocol::rpc::file::upload::FileRequestUploadTokenRequest {
                    user_id,
                    filename: Some("placeholder.png".to_string()),
                    file_size: crate::storage::media_preprocess::TRANSPARENT_PNG_1X1.len() as i64,
                    mime_type: "image/png".to_string(),
                    file_type: "image".to_string(),
                    business_type: "message".to_string(),
                };
            let thumb_token = {
                let mut guard = self.client.write().await;
                let c = guard
                    .as_mut()
                    .ok_or_else(|| PrivchatSDKError::NotConnected)?;
                c.file_request_upload_token(thumb_token_req).await?
            };
            let http_guard = self.http_client.read().await;
            let http = http_guard
                .as_ref()
                .ok_or_else(|| PrivchatSDKError::Other("HTTP 客户端未初始化".to_string()))?;
            let thumb_resp = http
                .upload_file_bytes(
                    &thumb_token.upload_url,
                    &thumb_token.token,
                    "placeholder.png".to_string(),
                    "image/png".to_string(),
                    crate::storage::media_preprocess::TRANSPARENT_PNG_1X1.to_vec(),
                    None,
                )
                .await?;
            Some(thumb_resp.file_id)
        } else {
            None
        };

        if let Err(e) = crate::storage::media_preprocess::prepare_media_sync(
            &self.config.data_dir,
            &uid,
            message_id,
            timestamp_ms,
            file_path,
            &filename,
            &mime_type,
            send_mode,
            video_hook.as_ref(),
        ) {
            let _ = self.storage.delete_message(message_id).await;
            return Err(e);
        }

        let task = crate::storage::queue::FileSendTask::new(
            message_id,
            uid,
            channel_id,
            channel_type,
            user_id,
            local_message_id,
            message_type_str,
            timestamp_ms,
            pre_uploaded_thumbnail_file_id,
        );
        self.file_send_queue.push(task).await?;
        {
            let guard = self.file_send_consumer.read().await;
            if guard.is_none() {
                drop(guard);
                if let Err(e) = self.start_file_send_consumer().await {
                    error!("❌ 启动 FileConsumer 失败: {}", e);
                    return Err(e);
                }
            }
        }

        let attachment_info = crate::events::AttachmentInfo {
            url: String::new(),
            mime_type,
            size: file_size as u64,
            thumbnail_url: None,
            filename: Some(filename),
            file_id: None,
            width: None,
            height: None,
            duration: None,
        };
        info!(
            "✅ 附件已入文件队列: message_id={}, local_message_id={}",
            message_id, local_message_id
        );
        Ok((local_message_id, attachment_info))
    }

    /// 从内存发送附件（世界级 IM SDK 标准 API）
    ///
    /// 流程与 `send_attachment_from_path` 相同，但文件数据来自内存。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `filename`: 文件名
    /// - `mime_type`: MIME 类型
    /// - `data`: 文件数据（字节）
    /// - `options`: 发送选项（回复、提及等）
    /// - `progress`: 进度回调（可选）
    ///
    /// # 返回
    /// - `Ok((u64, AttachmentInfo))`: 返回 (local_message_id, 附件信息)
    pub async fn send_attachment_bytes(
        &self,
        channel_id: u64,
        filename: String,
        mime_type: String,
        data: Vec<u8>,
        options: crate::events::SendMessageOptions,
        progress: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<(u64, crate::events::AttachmentInfo)> {
        info!(
            "📤 开始发送附件（从内存）: {} ({} bytes)",
            filename,
            data.len()
        );

        self.check_initialized().await?;
        self.check_connected().await?;

        let file_size = data.len() as i64;

        // 1. 检测文件类型（从 MIME 类型推断）
        let file_type_str = self.detect_file_type_from_mime(&mime_type);

        // 2. 请求上传 token（RPC）
        let user_id = self
            .user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        let upload_token_req =
            privchat_protocol::rpc::file::upload::FileRequestUploadTokenRequest {
                user_id,
                filename: Some(filename.clone()),
                file_size,
                mime_type: mime_type.clone(),
                file_type: file_type_str.to_string(),
                business_type: "message".to_string(),
            };

        let upload_token_resp = {
            let mut client = self.client.write().await;
            let client_ref = client
                .as_mut()
                .ok_or_else(|| PrivchatSDKError::NotConnected)?;
            client_ref
                .file_request_upload_token(upload_token_req)
                .await?
        };

        info!(
            "✅ 获取上传 token 成功: upload_url={}",
            upload_token_resp.upload_url
        );

        // 3. 上传文件（HTTP）
        let upload_resp = {
            let http_client_guard = self.http_client.read().await;
            let http_client = http_client_guard.as_ref().ok_or_else(|| {
                PrivchatSDKError::Other("HTTP 客户端未初始化，请配置 file_api_base_url".to_string())
            })?;

            http_client
                .upload_file_bytes(
                    &upload_token_resp.upload_url,
                    &upload_token_resp.token,
                    filename.clone(),
                    mime_type.clone(),
                    data,
                    progress.clone(),
                )
                .await?
        };

        info!(
            "✅ 文件上传成功: file_id={}, file_url={}",
            upload_resp.file_id, upload_resp.file_url
        );

        // 4. 构建消息 content（包含 file_id 和 file_url）
        let content = serde_json::json!({
            "file_id": upload_resp.file_id,
            "file_url": upload_resp.file_url,
            "mime_type": mime_type,
            "size": file_size,
            "filename": filename,
        })
        .to_string();

        // 5. 发送消息
        let local_message_id = self
            .send_message_with_options(channel_id, &content, options)
            .await?;

        // 6. 构建 AttachmentInfo
        let file_id = upload_resp.file_id.clone();
        let attachment_info = crate::events::AttachmentInfo {
            url: upload_resp.file_url,
            mime_type,
            size: file_size as u64,
            thumbnail_url: upload_resp.thumbnail_url,
            filename: Some(filename),
            file_id: Some(file_id.clone()),
            width: upload_resp.width,
            height: upload_resp.height,
            duration: None,
        };

        info!(
            "✅ 附件消息发送成功: local_message_id={}, file_id={}",
            local_message_id, file_id
        );

        Ok((local_message_id, attachment_info))
    }

    /// 检测文件类型和 MIME 类型（从文件路径）
    fn detect_file_type_and_mime(&self, path: &Path) -> Result<(String, String)> {
        // 简单实现：从文件扩展名推断
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let (file_type, mime_type) = match ext.as_str() {
            "jpg" | "jpeg" => ("image", "image/jpeg"),
            "png" => ("image", "image/png"),
            "gif" => ("image", "image/gif"),
            "webp" => ("image", "image/webp"),
            "mp4" => ("video", "video/mp4"),
            "mov" => ("video", "video/quicktime"),
            "avi" => ("video", "video/x-msvideo"),
            "mp3" => ("audio", "audio/mpeg"),
            "wav" => ("audio", "audio/wav"),
            "pdf" => ("file", "application/pdf"),
            "zip" => ("file", "application/zip"),
            "txt" => ("file", "text/plain"),
            _ => ("other", "application/octet-stream"),
        };

        Ok((file_type.to_string(), mime_type.to_string()))
    }

    /// 从 MIME 类型推断文件类型
    fn detect_file_type_from_mime(&self, mime_type: &str) -> &str {
        if mime_type.starts_with("image/") {
            "image"
        } else if mime_type.starts_with("video/") {
            "video"
        } else if mime_type.starts_with("audio/") {
            "audio"
        } else if mime_type == "application/pdf" || mime_type == "application/zip" {
            "file"
        } else {
            "other"
        }
    }

    /// 下载附件到缓存目录
    ///
    /// 下载文件到 SDK 管理的缓存目录，并通过 MediaIndex 进行索引管理。
    /// 如果文件已存在，直接返回缓存路径。
    ///
    /// # 参数
    /// - `file_id`: 文件ID（服务端分配的唯一标识）
    /// - `file_url`: 文件下载 URL
    /// - `progress`: 进度回调（可选）
    ///
    /// # 返回
    /// - `Ok(PathBuf)`: 返回缓存文件路径
    pub async fn download_attachment_to_cache(
        &self,
        file_id: &str,
        file_url: &str,
        progress: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<PathBuf> {
        info!(
            "📥 开始下载附件到缓存: file_id={}, file_url={}",
            file_id, file_url
        );

        self.check_initialized().await?;

        // 1. 检查文件是否已在缓存中
        let media_index = self
            .storage
            .media_index()
            .ok_or_else(|| PrivchatSDKError::Other("媒体索引管理器未初始化".to_string()))?;
        if let Some(cache_path) = media_index.get_file_path(file_id).await? {
            if cache_path.exists() {
                info!("✅ 文件已在缓存中: {}", cache_path.display());
                return Ok(cache_path);
            }
        }

        // 2. 确定缓存路径（通过 MediaIndex 管理）
        // 如果文件不在索引中，需要先添加到索引
        // 这里我们创建一个临时文件路径，下载完成后再添加到索引

        let user_id = self
            .user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        // 确保用户媒体索引已初始化
        let user_id_str = user_id.to_string();
        media_index.switch_user(&user_id_str).await?;

        // 3. 创建临时下载路径
        let temp_dir = self.config.data_dir.join("temp");
        tokio::fs::create_dir_all(&temp_dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建临时目录失败: {}", e)))?;

        let temp_file = temp_dir.join(format!("download_{}", file_id));

        // 4. 下载文件
        let http_client_guard = self.http_client.read().await;
        let http_client = http_client_guard.as_ref().ok_or_else(|| {
            PrivchatSDKError::Other("HTTP 客户端未初始化，请配置 file_api_base_url".to_string())
        })?;

        http_client
            .download_file(file_url, &temp_file, progress.clone())
            .await?;
        drop(http_client_guard);

        // 5. 将文件添加到 MediaIndex（会自动移动到正确的目录）
        let _file_record = media_index
            .add_file(&temp_file, Some(file_id.to_string()))
            .await?;

        // 6. 获取最终缓存路径
        let cache_path = media_index
            .get_file_path(file_id)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other("文件下载后无法获取缓存路径".to_string()))?;

        info!(
            "✅ 附件下载到缓存成功: file_id={}, cache_path={}",
            file_id,
            cache_path.display()
        );

        Ok(cache_path)
    }

    /// 下载附件到指定路径
    ///
    /// 下载文件到用户指定的路径，不经过缓存系统。
    ///
    /// # 参数
    /// - `file_url`: 文件下载 URL
    /// - `output_path`: 输出文件路径
    /// - `progress`: 进度回调（可选）
    ///
    /// # 返回
    /// - `Ok(())`: 下载成功
    pub async fn download_attachment_to_path(
        &self,
        file_url: &str,
        output_path: &Path,
        progress: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<()> {
        info!(
            "📥 开始下载附件到指定路径: file_url={}, output_path={}",
            file_url,
            output_path.display()
        );

        self.check_initialized().await?;

        // 1. 确保输出目录存在
        if let Some(parent) = output_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| PrivchatSDKError::IO(format!("创建输出目录失败: {}", e)))?;
        }

        // 2. 下载文件
        let http_client_guard = self.http_client.read().await;
        let http_client = http_client_guard.as_ref().ok_or_else(|| {
            PrivchatSDKError::Other("HTTP 客户端未初始化，请配置 file_api_base_url".to_string())
        })?;

        http_client
            .download_file(file_url, output_path, progress)
            .await?;
        drop(http_client_guard);

        info!("✅ 附件下载到指定路径成功: {}", output_path.display());

        Ok(())
    }

    /// 收到消息后从 Payload 提取缩略图 file_id 并打印；不执行实际下载。
    /// 由 push 流程在保存消息后 spawn 调用。content 可能为纯文本或 JSON（有 file_id / thumbnail_file_id 时为附件消息）。
    /// 仅当 content 能解析为 JSON 且含 file_id 或 thumbnail_file_id 时打印；纯文本直接跳过。
    async fn download_thumbnail_after_receive(
        _data_dir: PathBuf,
        _base_url: Option<String>,
        _http_client: Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
        _user_id: String,
        message_id: i64,
        content: String,
        _created_at_ms: i64,
    ) -> Result<()> {
        let content_json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => return Ok(()), // 纯文本 content，跳过
        };
        let thumb_file_id = content_json
            .get("thumbnail_file_id")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .or_else(|| {
                content_json.get("file_id").and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
                })
            });
        if let Some(file_id) = thumb_file_id {
            info!(
                "消息缩略图 file_id: message_id={}, file_id={}",
                message_id, file_id
            );
        }
        Ok(())
    }

    /// 下载附件到消息目录（点击查看/下载时保存到 {data_dir}/users/{uid}/files/{yyyymm}/{message.id}/）
    ///
    /// # 参数
    /// - `message_id`: 本地消息主键 message.id
    /// - `file_id`: 文件 ID（用于文件名）
    /// - `file_url`: 文件下载 URL
    /// - `filename`: 可选文件名（缺省为 file_{file_id}）
    /// - `progress`: 进度回调（可选）
    ///
    /// # 返回
    /// - `Ok(PathBuf)`: 下载后的文件路径
    pub async fn download_attachment_to_message_dir(
        &self,
        message_id: i64,
        file_id: &str,
        file_url: &str,
        filename: Option<String>,
        progress: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<PathBuf> {
        self.check_initialized().await?;
        let user_id = self
            .user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        let user_id_str = user_id.to_string();
        let message = self
            .storage
            .get_message_by_id(message_id)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other("消息不存在".to_string()))?;
        let created_at_ms = message.created_at;
        use crate::storage::media_preprocess::{message_files_dir, yyyymm_from_timestamp_ms};
        let yyyymm = yyyymm_from_timestamp_ms(created_at_ms);
        let dir = message_files_dir(&self.config.data_dir, &user_id_str, &yyyymm, message_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建消息文件目录失败: {}", e)))?;
        let filename = filename.unwrap_or_else(|| format!("file_{}", file_id));
        let output_path = dir.join(&filename);
        self.download_attachment_to_path(file_url, &output_path, progress)
            .await?;
        Ok(output_path)
    }

    /// 回复消息
    ///
    /// 发送一条回复消息，自动包含对原消息的引用。
    ///
    /// # 参数
    /// - `channel_id`: 频道/会话 ID
    /// - `in_reply_to_message_id`: 要回复的消息的本地主键 message.id（合约 v1）；发送前会查库填 server_message_id 到协议
    /// - `body`: 回复内容
    ///
    /// # 返回
    /// - `Ok(u64)`: 返回 local_message_id（用于跟踪发送状态）
    ///
    /// 发送消息（完整参数）
    pub async fn send_message_with_input(&self, input: &MessageInput) -> Result<u64> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let message_id = self
            .snowflake
            .next_id()
            .map_err(|e| PrivchatSDKError::Other(format!("生成 message_id 失败: {:?}", e)))?;

        // 通过传输层发送消息
        if let Some(_client) = self.client.read().await.as_ref() {
            // TODO: 集成 MessageSender 进行实际发送
            // 构建完整的 SendMessageRequest 并通过消息队列发送
            tracing::info!("准备发送消息: {} -> {}", message_id, input.session_id);
        }

        debug!("消息发送成功: {}", message_id);
        Ok(message_id)
    }

    /// 标记消息为已读（按 message.id，合约 v1）
    ///
    /// 参数 `message_id` 为本地主键 message.id，不暴露 server_message_id。
    pub async fn mark_as_read(&self, channel_id: u64, message_id: u64) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        info!(
            "标记消息为已读: channel_id={}, message_id(id)={}",
            channel_id, message_id
        );

        // ========== 使用 RPC 标记已读（若服务端需要 server_message_id，可在此按 message.id 查库后传） ==========
        // 当前仅记录；本地已读状态由 DAO mark_as_read(id) 维护
        info!("✅ 消息已标记为已读: id={}", message_id);

        Ok(())
    }

    /// 根据客户端消息编号获取消息
    ///
    /// # 参数
    /// - `local_message_id`: 客户端消息编号（由 send_message 返回）
    ///
    /// # 返回
    /// 根据 message.id 获取消息
    pub async fn get_message_by_id(
        &self,
        id: u64,
    ) -> Result<Option<crate::storage::entities::Message>> {
        self.check_initialized().await?;
        self.storage.get_message_by_id(id as i64).await
    }

    /// 撤回消息（按 message.id）
    pub async fn recall_message(&self, id: u64) -> Result<()> {
        self.check_initialized().await?;
        let msg = self
            .storage
            .get_message_by_id(id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", id)))?;
        self.storage.revoke_message(id as i64).await?;
        if self.check_connected().await.is_ok() {
            if let Some(server_msg_id) = msg.server_message_id {
                use crate::RpcClientExt;
                use privchat_protocol::rpc::MessageRevokeRequest;
                let revoke_request = MessageRevokeRequest {
                    server_message_id: server_msg_id,
                    channel_id: msg.channel_id,
                    user_id: 0,
                };
                let mut client_guard = self.client.write().await;
                if let Some(client) = client_guard.as_mut() {
                    let _ = client.message_revoke(revoke_request).await;
                }
            }
        }
        info!("✅ 消息已撤回: id={}", id);
        Ok(())
    }

    /// 编辑消息（按 message.id）
    pub async fn edit_message(&self, id: u64, new_content: &str) -> Result<()> {
        self.check_initialized().await?;
        self.storage
            .update_message_content(id as i64, new_content)
            .await?;
        info!("✅ 消息已编辑: id={}", id);
        Ok(())
    }

    // ========== 实时交互 ==========

    /// 开始输入状态
    pub async fn start_typing(&self, channel_id: u64) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        // channel_id 已经是 u64 类型
        let _channel_id_u64 = channel_id;

        debug!("开始输入状态: channel_id={}", channel_id);

        // ========== 使用 RPC 发送输入状态 ==========

        // Note: 需要 privchat-protocol 中定义相应的 RPC 接口
        // 这里假设有 TypingIndicatorRequest

        // use crate::RpcClientExt;
        // use privchat_protocol::rpc::message::TypingIndicatorRequest;

        // let typing_request = TypingIndicatorRequest {
        //     channel_id: channel_id_u64,
        //     is_typing: true,
        // };

        // let mut client_guard = self.client.write().await;
        // let client = client_guard.as_mut()
        //     .ok_or(PrivchatSDKError::NotConnected)?;

        // client.typing_indicator(typing_request).await
        //     .map_err(|e| PrivchatSDKError::RpcError(format!("发送输入状态失败: {}", e)))?;

        debug!("✅ 输入状态已发送 (开始): {}", channel_id);
        Ok(())
    }

    /// 添加表情反馈（按 message.id，合约 v1）
    ///
    /// 入参为本地主键 message.id；RPC 需要 server_message_id 时由本方法查库后填入。
    pub async fn add_reaction(&self, message_id: u64, emoji: &str) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let msg = self
            .storage
            .get_message_by_id(message_id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", message_id)))?;
        let server_message_id = msg.server_message_id.ok_or_else(|| {
            PrivchatSDKError::Other("消息尚未同步到服务端，无法添加反应".to_string())
        })?;

        info!(
            "添加表情反馈: id={}, server_msg_id={}, emoji={}",
            message_id, server_message_id, emoji
        );

        use crate::RpcClientExt;
        use privchat_protocol::rpc::MessageReactionAddRequest;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let reaction_request = MessageReactionAddRequest {
            server_message_id: server_message_id,
            channel_id: None,
            user_id: user_id,
            emoji: emoji.to_string(),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let _response = client.message_reaction_add(reaction_request).await?;

        self.storage
            .add_message_reaction(message_id as i64, user_id, emoji)
            .await?;
        info!("✅ 表情反馈已添加: id={} -> {}", message_id, emoji);

        Ok(())
    }

    /// 移除表情反馈（按 message.id，合约 v1）
    ///
    /// 入参为本地主键 message.id；RPC 需要 server_message_id 时由本方法查库后填入。
    pub async fn remove_reaction(&self, message_id: u64, emoji: &str) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let msg = self
            .storage
            .get_message_by_id(message_id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", message_id)))?;
        let server_message_id = msg.server_message_id.ok_or_else(|| {
            PrivchatSDKError::Other("消息尚未同步到服务端，无法移除反应".to_string())
        })?;

        info!(
            "移除表情反馈: id={}, server_msg_id={}, emoji={}",
            message_id, server_message_id, emoji
        );

        use crate::RpcClientExt;
        use privchat_protocol::rpc::MessageReactionRemoveRequest;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let reaction_request = MessageReactionRemoveRequest {
            server_message_id: server_message_id,
            user_id: user_id,
            emoji: emoji.to_string(),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let _response = client.message_reaction_remove(reaction_request).await?;

        info!("✅ 表情反馈已移除: id={} -> {}", message_id, emoji);

        Ok(())
    }

    /// 获取消息的反应列表
    ///
    /// 获取指定消息的所有反应（表情和用户列表）。
    ///
    /// # 参数
    /// - `channel_id`: 频道ID（用于验证，可选）
    /// - `message_id`: 本地主键 message.id（合约 v1）；RPC 需要 server_message_id 时由本方法查库后填入
    ///
    /// # 返回
    /// - `Ok(Vec<ReactionChip>)`: 反应列表，每个 ReactionChip 包含表情符号和用户ID列表
    ///
    /// # 示例
    /// ```rust
    /// let reactions = sdk.reactions(channel_id, message_id).await?;
    /// for chip in reactions {
    ///     println!("{}: {} 个用户", chip.emoji, chip.count);
    /// }
    /// ```
    pub async fn reactions(
        &self,
        _channel_id: u64,
        message_id: u64,
    ) -> Result<Vec<crate::events::ReactionChip>> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let msg = self
            .storage
            .get_message_by_id(message_id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", message_id)))?;
        let server_message_id = msg.server_message_id.ok_or_else(|| {
            PrivchatSDKError::Other("消息尚未同步到服务端，无法获取反应列表".to_string())
        })?;

        use crate::RpcClientExt;
        use privchat_protocol::rpc::MessageReactionListRequest;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let request = MessageReactionListRequest {
            server_message_id: server_message_id,
            user_id,
        };

        // 调用 RPC
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response = client.message_reaction_list(request).await?;

        drop(client_guard);

        // 解析服务器返回的反应数据
        // 服务器返回格式：{ "reactions": [{ "emoji": "👍", "user_ids": [1, 2, 3] }, ...] }
        let mut chips = Vec::new();

        for reaction_value in response.reactions {
            if let Some(obj) = reaction_value.as_object() {
                if let (Some(emoji), Some(user_ids_value)) = (
                    obj.get("emoji").and_then(|v| v.as_str()),
                    obj.get("user_ids"),
                ) {
                    let user_ids: Vec<u64> = if let Some(arr) = user_ids_value.as_array() {
                        arr.iter().filter_map(|v| v.as_u64()).collect()
                    } else {
                        Vec::new()
                    };

                    chips.push(crate::events::ReactionChip {
                        emoji: emoji.to_string(),
                        user_ids: user_ids.clone(),
                        count: user_ids.len(),
                    });
                }
            }
        }

        info!(
            "✅ 获取消息反应列表成功: id={}, 反应数={}",
            message_id,
            chips.len()
        );

        Ok(chips)
    }

    /// 批量获取消息的反应列表
    ///
    /// 一次性获取多个消息的反应列表，提高效率。
    ///
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_ids`: 消息ID列表
    ///
    /// # 返回
    /// - `Ok(HashMap<u64, Vec<ReactionChip>>)`: 消息ID到反应列表的映射
    ///
    /// # 示例
    /// ```rust
    /// let message_ids = vec![123, 456, 789];
    /// let reactions_map = sdk.reactions_batch(channel_id, message_ids).await?;
    /// for (msg_id, chips) in reactions_map {
    ///     println!("消息 {} 有 {} 个反应", msg_id, chips.len());
    /// }
    /// ```
    pub async fn reactions_batch(
        &self,
        channel_id: u64,
        message_ids: Vec<u64>,
    ) -> Result<std::collections::HashMap<u64, Vec<crate::events::ReactionChip>>> {
        self.check_initialized().await?;
        self.check_connected().await?;

        if message_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }

        // 批量查询：为每个消息ID调用 reactions 方法
        // 注意：如果服务器支持批量查询 RPC，应该使用批量接口以提高效率
        let mut results = std::collections::HashMap::new();

        for message_id in message_ids {
            match self.reactions(channel_id, message_id).await {
                Ok(chips) => {
                    results.insert(message_id, chips);
                }
                Err(e) => {
                    warn!("获取消息 {} 的反应失败: {}", message_id, e);
                    // 继续处理其他消息，不中断整个批量查询
                    results.insert(message_id, Vec::new());
                }
            }
        }

        info!("✅ 批量获取消息反应列表成功: 共 {} 条消息", results.len());

        Ok(results)
    }

    /// 检查指定用户是否已读指定消息
    ///
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_id`: 本地消息主键（message.id）
    /// - `user_id`: 用户ID
    ///
    /// # 返回
    /// - `Ok(bool)`: true 表示已读，false 表示未读
    ///
    /// # 示例
    /// ```rust
    /// let is_read = sdk.is_event_read_by(channel_id, message_id, user_id).await?;
    /// if is_read {
    ///     println!("用户 {} 已读消息 {}", user_id, message_id);
    /// }
    /// ```
    pub async fn is_event_read_by(
        &self,
        _channel_id: u64,
        message_id: u64,
        user_id: u64,
    ) -> Result<bool> {
        self.check_initialized().await?;

        // ⚠️ advanced_features 使用 server message_id，需先转换
        let msg = self
            .storage
            .get_message_by_id(message_id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", message_id)))?;
        let server_message_id = msg.server_message_id.ok_or_else(|| {
            PrivchatSDKError::Other(format!("消息尚未同步到服务器: id={}", message_id))
        })?;

        // 通过 AdvancedFeaturesManager 查询已读回执
        let features_guard = self.features.read().await;
        if let Some(features) = features_guard.as_ref() {
            let receipts = features.get_message_read_receipts(server_message_id)?;

            // 检查是否有该用户的已读回执
            let is_read = receipts.iter().any(|receipt| receipt.reader_uid == user_id);

            Ok(is_read)
        } else {
            // 如果 AdvancedFeaturesManager 未初始化，返回 false
            Ok(false)
        }
    }

    /// 获取已读指定消息的用户列表
    ///
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_id`: 本地消息主键（message.id）
    /// - `limit`: 返回的最大用户数量（可选，默认返回所有）
    ///
    /// # 返回
    /// - `Ok(Vec<SeenByEntry>)`: 已读用户列表，按已读时间排序
    ///
    /// # 示例
    /// ```rust
    /// let seen_by = sdk.seen_by_for_event(channel_id, message_id, Some(10)).await?;
    /// for entry in seen_by {
    ///     println!("用户 {} 在 {} 已读", entry.user_id, entry.read_at);
    /// }
    /// ```
    pub async fn seen_by_for_event(
        &self,
        _channel_id: u64,
        message_id: u64,
        limit: Option<u32>,
    ) -> Result<Vec<crate::events::SeenByEntry>> {
        self.check_initialized().await?;

        // ⚠️ advanced_features 使用 server message_id，需先转换
        let msg = self
            .storage
            .get_message_by_id(message_id as i64)
            .await?
            .ok_or_else(|| PrivchatSDKError::Other(format!("消息不存在: id={}", message_id)))?;
        let server_message_id = msg.server_message_id.ok_or_else(|| {
            PrivchatSDKError::Other(format!("消息尚未同步到服务器: id={}", message_id))
        })?;

        // 通过 AdvancedFeaturesManager 查询已读回执
        let features_guard = self.features.read().await;
        if let Some(features) = features_guard.as_ref() {
            let receipts = features.get_message_read_receipts(server_message_id)?;

            // 转换为 SeenByEntry 列表
            let mut entries: Vec<crate::events::SeenByEntry> = receipts
                .into_iter()
                .map(|receipt| crate::events::SeenByEntry {
                    user_id: receipt.reader_uid,
                    read_at: receipt.read_at,
                })
                .collect();

            // 按已读时间排序（从早到晚）
            entries.sort_by_key(|e| e.read_at);

            // 应用限制
            if let Some(limit) = limit {
                entries.truncate(limit as usize);
            }

            Ok(entries)
        } else {
            // 如果 AdvancedFeaturesManager 未初始化，返回空列表
            Ok(Vec::new())
        }
    }

    // ========== 在线状态管理 ==========

    /// 订阅用户在线状态
    ///
    /// 用于私聊会话场景：当打开与某用户的私聊会话时，订阅对方的在线状态
    ///
    /// # 参数
    /// - user_ids: 要订阅的用户ID列表
    ///
    /// # 返回
    /// - 返回初始的在线状态信息
    pub async fn subscribe_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<HashMap<u64, privchat_protocol::presence::OnlineStatusInfo>> {
        self.check_initialized().await?;
        self.check_connected().await?;

        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }

        info!("📡 订阅在线状态: user_ids={:?}", user_ids);

        // 1. 调用服务端RPC
        use crate::RpcClientExt;
        use privchat_protocol::presence::{SubscribePresenceRequest, SubscribePresenceResponse};

        let subscribe_request = SubscribePresenceRequest {
            user_ids: user_ids.clone(),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response: SubscribePresenceResponse =
            client.subscribe_presence(subscribe_request).await?;

        // 2. 更新本地订阅状态
        self.presence_manager
            .add_subscription(user_ids.clone())
            .await;

        // 3. 更新本地缓存
        self.presence_manager
            .batch_update_status(response.initial_statuses.clone())
            .await;

        info!("✅ 已订阅 {} 个用户的在线状态", user_ids.len());

        Ok(response.initial_statuses)
    }

    /// 取消订阅用户在线状态
    ///
    /// 当关闭会话时，取消订阅对方的在线状态
    ///
    /// # 参数
    /// - user_ids: 要取消订阅的用户ID列表
    pub async fn unsubscribe_presence(&self, user_ids: Vec<u64>) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        if user_ids.is_empty() {
            return Ok(());
        }

        info!("📡 取消订阅在线状态: user_ids={:?}", user_ids);

        // 1. 调用服务端RPC
        use crate::RpcClientExt;
        use privchat_protocol::presence::UnsubscribePresenceRequest;

        let unsubscribe_request = UnsubscribePresenceRequest {
            user_ids: user_ids.clone(),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let _response = client.unsubscribe_presence(unsubscribe_request).await?;

        // 2. 更新本地订阅状态
        self.presence_manager
            .remove_subscription(user_ids.clone())
            .await;

        info!("✅ 已取消订阅 {} 个用户的在线状态", user_ids.len());

        Ok(())
    }

    /// 获取用户在线状态（从本地缓存）
    ///
    /// # 参数
    /// - user_id: 用户ID
    ///
    /// # 返回
    /// - 如果缓存命中，返回在线状态信息；否则返回 None
    pub async fn get_presence(
        &self,
        user_id: u64,
    ) -> Option<privchat_protocol::presence::OnlineStatusInfo> {
        self.presence_manager.get_status(user_id).await
    }

    /// 批量获取用户在线状态（从本地缓存）
    ///
    /// 用于好友列表场景：显示好友列表时，批量查询在线状态
    ///
    /// # 参数
    /// - user_ids: 用户ID列表
    ///
    /// # 返回
    /// - 返回缓存中的在线状态信息（未命中的用户不会包含在结果中）
    pub async fn batch_get_presence(
        &self,
        user_ids: &[u64],
    ) -> HashMap<u64, privchat_protocol::presence::OnlineStatusInfo> {
        self.presence_manager.batch_get_status(user_ids).await
    }

    /// 批量查询用户在线状态（从服务端）
    ///
    /// 用于好友列表场景：当需要刷新在线状态时，主动查询服务端
    ///
    /// # 参数
    /// - user_ids: 用户ID列表
    ///
    /// # 返回
    /// - 返回最新的在线状态信息
    pub async fn fetch_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<HashMap<u64, privchat_protocol::presence::OnlineStatusInfo>> {
        self.check_initialized().await?;
        self.check_connected().await?;

        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }

        debug!("🔍 查询在线状态: user_ids={:?}", user_ids);

        // 1. 调用服务端RPC
        use crate::RpcClientExt;
        use privchat_protocol::presence::{GetOnlineStatusRequest, GetOnlineStatusResponse};

        let query_request = GetOnlineStatusRequest {
            user_ids: user_ids.clone(),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response: GetOnlineStatusResponse = client.get_online_status(query_request).await?;

        // 2. 更新本地缓存
        self.presence_manager
            .batch_update_status(response.statuses.clone())
            .await;

        debug!("✅ 查询到 {} 个用户的在线状态", response.statuses.len());

        Ok(response.statuses)
    }

    /// 获取在线状态缓存统计信息
    pub async fn get_presence_stats(&self) -> crate::presence::PresenceCacheStats {
        self.presence_manager.get_cache_stats().await
    }

    /// 清空在线状态缓存
    pub async fn clear_presence_cache(&self) {
        self.presence_manager.clear_cache().await;
    }

    // ========== 输入状态管理 ==========

    /// 发送输入状态（开始输入）
    ///
    /// # 参数
    /// - channel_id: 会话ID
    /// - action_type: 输入动作类型（默认为 Typing）
    ///
    /// # 说明
    /// - 会自动进行防抖处理，避免频繁发送
    /// - 5秒后自动清除，需要持续调用以保持输入状态
    pub async fn send_typing(
        &self,
        channel_id: u64,
        action_type: Option<TypingActionType>,
    ) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let action = action_type.unwrap_or(TypingActionType::Typing);

        // 检查是否需要发送（防抖）
        let should_send = self
            .typing_manager
            .start_typing(channel_id, 1, action.clone()) // channel_type=1 (私聊)
            .await;

        if !should_send {
            return Ok(()); // 防抖中，不发送
        }

        debug!("📤 Sending typing status to channel {}", channel_id);

        // 调用RPC发送输入状态
        let _typing_request = TypingIndicatorRequest {
            channel_id,
            channel_type: 1, // 私聊
            is_typing: true,
            action_type: action,
        };

        let mut client_guard = self.client.write().await;
        let _client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        // TODO: 添加 typing_indicator RPC 方法到 RpcClientExt
        // let _response = client.typing_indicator(typing_request).await?;

        debug!("✅ Typing status sent to channel {}", channel_id);

        Ok(())
    }

    /// 停止输入状态
    ///
    /// # 参数
    /// - channel_id: 会话ID
    pub async fn stop_typing(&self, channel_id: u64) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        debug!("📤 Stopping typing status for channel {}", channel_id);

        // 更新本地状态
        self.typing_manager.stop_typing(channel_id).await;

        // 发送停止输入通知
        let _typing_request = TypingIndicatorRequest {
            channel_id,
            channel_type: 1,
            is_typing: false,
            action_type: TypingActionType::Typing,
        };

        let mut client_guard = self.client.write().await;
        let _client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        // TODO: 添加 typing_indicator RPC 方法
        // let _response = client.typing_indicator(typing_request).await?;

        debug!("✅ Stopped typing for channel {}", channel_id);

        Ok(())
    }

    /// 获取输入状态统计
    pub async fn get_typing_stats(&self) -> crate::typing::TypingStats {
        self.typing_manager.get_stats().await
    }

    // ========== 事件系统 ==========

    /// 注册消息接收回调
    ///
    /// 当收到新消息时，会自动调用此回调函数
    pub fn on_message_received<F>(&self, callback: F)
    where
        F: Fn(MessageOutput) + Send + Sync + 'static,
    {
        let event_manager = self.event_manager.clone();
        let callback = Arc::new(callback);

        tokio::spawn(async move {
            let mut subscriber = event_manager.subscribe().await;

            loop {
                match subscriber.recv().await {
                    Ok(event) => {
                        if let SDKEvent::MessageReceived {
                            server_message_id,
                            channel_id,
                            channel_type: _,
                            from_uid,
                            timestamp,
                            content: _,
                        } = event
                        {
                            let msg_output = MessageOutput {
                                server_message_id,
                                sender_id: from_uid,
                                session_id: channel_id,
                                content: String::new(), // 需要从存储中读取或从 payload 解析
                                message_type: MessageType::Text,
                                status: MessageStatus::Sent,
                                created_at: timestamp,
                                extra: HashMap::new(),
                            };

                            callback(msg_output);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    /// 注册输入状态回调
    pub fn on_typing_indicator<F>(&self, callback: F)
    where
        F: Fn(String, String, bool) + Send + Sync + 'static, // user_id, session_id, is_typing
    {
        let event_manager = self.event_manager.clone();
        let callback = Arc::new(callback);

        tokio::spawn(async move {
            let mut subscriber = event_manager.subscribe().await;

            loop {
                match subscriber.recv().await {
                    Ok(event) => match event {
                        SDKEvent::TypingStarted(typing_event) => {
                            callback(
                                typing_event.user_id.to_string(),
                                typing_event.channel_id.to_string(),
                                true,
                            );
                        }
                        SDKEvent::TypingStopped(typing_event) => {
                            callback(
                                typing_event.user_id.to_string(),
                                typing_event.channel_id.to_string(),
                                false,
                            );
                        }
                        SDKEvent::TypingIndicator(typing_event) => {
                            callback(
                                typing_event.user_id.to_string(),
                                typing_event.channel_id.to_string(),
                                typing_event.is_typing,
                            );
                        }
                        _ => {}
                    },
                    Err(_) => break,
                }
            }
        });
    }

    /// 注册表情反馈回调
    pub fn on_reaction_changed<F>(&self, callback: F)
    where
        F: Fn(String, String, String, bool) + Send + Sync + 'static, // message_id, user_id, emoji, is_added
    {
        let event_manager = self.event_manager.clone();
        let callback = Arc::new(callback);

        tokio::spawn(async move {
            let mut subscriber = event_manager.subscribe().await;

            loop {
                match subscriber.recv().await {
                    Ok(event) => match event {
                        SDKEvent::ReactionAdded(reaction_event) => {
                            callback(
                                reaction_event.message_id.to_string(),
                                reaction_event.user_id.to_string(),
                                reaction_event.emoji,
                                true,
                            );
                        }
                        SDKEvent::ReactionRemoved(reaction_event) => {
                            callback(
                                reaction_event.message_id.to_string(),
                                reaction_event.user_id.to_string(),
                                reaction_event.emoji,
                                false,
                            );
                        }
                        _ => {}
                    },
                    Err(_) => break,
                }
            }
        });
    }

    /// 注册连接状态回调
    pub fn on_connection_state_changed<F>(&self, callback: F)
    where
        F: Fn(bool) + Send + Sync + 'static, // is_connected
    {
        let event_manager = self.event_manager.clone();
        let callback = Arc::new(callback);

        tokio::spawn(async move {
            let mut subscriber = event_manager.subscribe().await;

            loop {
                match subscriber.recv().await {
                    Ok(event) => {
                        if let SDKEvent::ConnectionStateChanged { new_state, .. } = event {
                            let is_connected = matches!(new_state, EventConnectionState::Connected);
                            callback(is_connected);
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    // ========== 好友管理功能 ==========

    /// 搜索用户
    ///
    /// # 参数
    /// - `query`: 搜索关键词（用户名、手机号等）
    ///
    /// # 返回
    /// 返回搜索到的用户列表
    /// 搜索用户，返回搜索会话ID（用于后续发送好友请求）
    pub async fn search_users(&self, query: &str) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::AccountSearchQueryRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = AccountSearchQueryRequest {
            from_user_id: user_id,
            query: query.to_string(),
            page: Some(1),
            page_size: Some(20),
        };

        let response: serde_json::Value = client
            .call_rpc_typed(routes::account_search::QUERY, request)
            .await?;

        Ok(response)
    }

    /// 发送好友请求
    ///
    /// # 参数
    /// - `to_user_id`: 目标用户ID
    /// - `remark`: 验证消息（可选）
    /// 发送好友请求
    ///
    /// # 参数
    /// - `to_user_id`: 目标用户ID
    /// - `remark`: 好友请求消息
    /// - `search_session_id`: 搜索会话ID（由 search_users 返回）
    pub async fn send_friend_request(
        &self,
        to_user_id: u64,
        remark: Option<&str>,
        search_session_id: Option<String>,
    ) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::FriendApplyRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let from_user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = FriendApplyRequest {
            from_user_id: from_user_id,
            target_user_id: to_user_id,
            message: remark.map(|s| s.to_string()),
            source: Some("search".to_string()),
            source_id: search_session_id,
        };

        let response = client
            .call_rpc_typed(routes::friend::APPLY, request)
            .await?;

        info!("✅ 好友请求已发送: to_user={}", to_user_id);
        Ok(response)
    }

    /// 获取待处理好友申请列表（别人申请我为好友的请求）
    pub async fn get_friend_pending_requests(
        &self,
    ) -> Result<privchat_protocol::rpc::contact::friend::FriendPendingResponse> {
        use privchat_protocol::rpc::contact::friend::{
            FriendPendingRequest, FriendPendingResponse,
        };

        self.check_initialized().await?;
        self.check_connected().await?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = FriendPendingRequest { user_id: 0 };
        let response: FriendPendingResponse = client.contact_friend_pending(request).await?;
        info!("✅ 待处理好友申请: {} 条", response.requests.len());
        Ok(response)
    }

    /// 接受好友请求
    ///
    /// # 参数
    /// - `from_user_id`: 发起请求的用户ID
    pub async fn accept_friend_request(&self, from_user_id: u64) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::FriendAcceptRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let target_user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = FriendAcceptRequest {
            from_user_id,
            target_user_id: target_user_id,
            message: None,
        };

        let channel_id: u64 = client
            .call_rpc_typed(routes::friend::ACCEPT, request)
            .await?;

        // ✅ 保存 channel 到数据库（若已存在相同 channel_id 的私聊会话则跳过，避免列表两条）
        if channel_id > 0 {
            if self
                .storage
                .get_direct_channel_by_id(channel_id)
                .await?
                .is_some()
            {
                debug!("私聊会话已存在，跳过保存: channel_id={}", channel_id);
            } else {
                info!(
                    "💾 保存私聊会话到数据库: channel_id={}, target_user={}",
                    channel_id, from_user_id
                );

                use crate::storage::entities::Channel;
                use chrono::Utc;

                let now_millis = Utc::now().timestamp_millis();

                let channel = Channel {
                    id: None,
                    channel_id: channel_id,
                    channel_type: 1, // 1=私聊
                    last_local_message_id: 0,
                    last_msg_timestamp: Some(now_millis),
                    last_msg_content: String::new(),
                    unread_count: 0,
                    last_msg_pts: 0,
                    show_nick: 0,
                    username: from_user_id.to_string(),
                    channel_name: from_user_id.to_string(),
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

                if let Err(e) = self.storage.save_channel(&channel).await {
                    warn!("⚠️ 保存 channel 到数据库失败: {}", e);
                }
            }
        }

        info!(
            "✅ 好友请求已接受: from_user={}, channel_id={}",
            from_user_id, channel_id
        );
        Ok(serde_json::json!({ "channel_id": channel_id }))
    }

    /// 拒绝好友请求
    pub async fn reject_friend_request(&self, from_user_id: u64) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::FriendRejectRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let target_user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = FriendRejectRequest {
            from_user_id,
            target_user_id: target_user_id,
            message: None,
        };

        let _: bool = client
            .call_rpc_typed(routes::friend::REJECT, request)
            .await?;

        info!("✅ 好友请求已拒绝: from_user={}", from_user_id);
        Ok(serde_json::json!(true))
    }

    /// 获取或创建与某用户的私聊会话（非好友发消息流程用）
    ///
    /// 若服务端已有该两人的私聊会话则返回已有 channel_id；否则创建并返回新 channel_id。
    /// 会将 channel 落库，便于本地 find_channel_id_by_user 后续可查。
    ///
    /// # 参数
    /// - `target_user_id`: 对方用户 ID
    /// - `source`: 可选，来源类型（与添加好友一致：search/phone/card_share/group/qrcode）
    /// - `source_id`: 可选，来源 ID
    ///
    /// # 返回
    /// - `Ok((channel_id, created))`: created 表示是否本次新创建
    pub async fn get_or_create_direct_channel(
        &self,
        target_user_id: u64,
        source: Option<String>,
        source_id: Option<String>,
    ) -> Result<(u64, bool)> {
        use privchat_protocol::rpc::channel::{
            GetOrCreateDirectChannelRequest, GetOrCreateDirectChannelResponse,
        };

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        if user_id == target_user_id {
            return Err(PrivchatSDKError::Other(
                "不能与自己创建私聊会话".to_string(),
            ));
        }

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = GetOrCreateDirectChannelRequest {
            target_user_id,
            source,
            source_id,
            user_id: 0, // 服务端填充
        };

        let response: GetOrCreateDirectChannelResponse = client
            .call_rpc_typed(routes::channel::DIRECT_GET_OR_CREATE, request)
            .await?;

        let channel_id = response.channel_id;
        let created = response.created;

        if channel_id > 0 {
            // 避免重复：若已存在相同 channel_id 的私聊会话（type 0 或 1），不再插入，防止列表出现两条
            if self
                .storage
                .get_direct_channel_by_id(channel_id)
                .await?
                .is_some()
            {
                debug!("私聊会话已存在，跳过保存: channel_id={}", channel_id);
            } else {
                info!(
                    "💾 保存私聊会话到数据库: channel_id={}, target_user={}, created={}",
                    channel_id, target_user_id, created
                );

                use crate::storage::entities::Channel;
                use chrono::Utc;

                let now_millis = Utc::now().timestamp_millis();
                let channel = Channel {
                    id: None,
                    channel_id,
                    channel_type: 1, // SDK 本地 1=私聊
                    last_local_message_id: 0,
                    last_msg_timestamp: Some(now_millis),
                    last_msg_content: String::new(),
                    unread_count: 0,
                    last_msg_pts: 0,
                    show_nick: 0,
                    username: target_user_id.to_string(),
                    channel_name: target_user_id.to_string(),
                    channel_remark: String::new(),
                    top: 0,
                    mute: 0,
                    save: 0,
                    forbidden: 0,
                    follow: 0,
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

                if let Err(e) = self.storage.save_channel(&channel).await {
                    warn!("⚠️ 保存 channel 到数据库失败: {}", e);
                }
            }
        }

        Ok((channel_id, created))
    }

    // ========== 好友获取 API（Local-First）==========

    /// 获取好友列表（从本地数据库，瞬间返回，5-20ms）
    ///
    /// 这是 Local-First 模式的核心方法，直接从本地 SQLite 读取，
    /// 不需要网络请求，即使在飞行模式下也能正常工作。
    ///
    /// # 参数
    /// - `limit`: 每页数量
    /// - `offset`: 偏移量
    ///
    /// # 示例
    /// ```rust
    /// // 获取前 50 个好友（含展示信息，瞬间返回）
    /// let friends = sdk.get_friends(50, 0).await?;
    /// ```
    pub async fn get_friends(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<
        Vec<(
            crate::storage::entities::Friend,
            crate::storage::entities::User,
        )>,
    > {
        self.check_initialized().await?;
        self.storage().get_friends(limit, offset).await
    }

    /// 从服务器同步好友列表到本地数据库（支持分页拉取 5000+ 好友）
    ///
    /// 这个方法会自动处理分页，即使有 5000 个好友也能完整同步。
    /// 使用游标分页，按添加时间排序，避免同步过程中新增好友导致重复。
    ///
    /// 统一实体同步入口（ENTITY_SYNC_V1）
    ///
    /// 所有列表型/集合型数据仅通过本接口同步，不再提供 sync_friends / sync_groups / sync_channels / sync_group_members 等独立接口。
    ///
    /// # 参数
    /// - `entity_type`: 实体类型（Friend / Group / Channel / GroupMember / User / UserSettings / UserBlock）
    /// - `scope`: 可选范围，如 GroupMember 时为 Some(group_id)，User 按需拉取时为 Some(user_id)
    ///
    /// # 返回
    /// - `Ok(count)`: 本轮同步并落库的条数
    ///
    /// # 示例
    /// ```rust
    /// use privchat_sdk::sync::EntityType;
    /// let n = sdk.sync_entities(EntityType::Friend, None).await?;   // 好友列表
    /// let n = sdk.sync_entities(EntityType::Group, None).await?;     // 群列表
    /// let n = sdk.sync_entities(EntityType::Channel, None).await?;   // 会话列表
    /// let n = sdk.sync_entities(EntityType::GroupMember, Some(&group_id.to_string())).await?; // 某群成员
    /// ```
    pub async fn sync_entities(
        &self,
        entity_type: crate::sync::EntityType,
        scope: Option<&str>,
    ) -> Result<usize> {
        use crate::sync::{EntitySyncEngine, EntityType, SyncCursorStore};

        self.check_initialized().await?;
        self.check_connected().await?;

        info!(
            "🔄 开始同步实体（ENTITY_SYNC_V1）: {} scope={:?}",
            entity_type.as_str(),
            scope
        );

        let storage = self.storage();
        let kv = storage
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("KV 未初始化".to_string()))?;
        let cursor_store = SyncCursorStore::new(kv);
        let engine = EntitySyncEngine::new(cursor_store);

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let count = engine
            .run_entity_sync(client, storage, entity_type, scope, true)
            .await?;

        drop(client_guard);

        if matches!(entity_type, EntityType::Channel) {
            self.emit_channel_list_reset().await;
        }

        info!("✅ 实体同步完成: {} 共 {} 条", entity_type.as_str(), count);
        Ok(count)
    }

    /// 在后台异步执行实体同步（不阻塞当前任务）
    ///
    /// # 参数
    /// - `sdk`: SDK 的 Arc 引用
    /// - `entity_type`: 实体类型
    /// - `scope`: 可选范围（如 GroupMember 时传 Some(group_id.to_string())）
    pub fn sync_entities_in_background(
        sdk: Arc<Self>,
        entity_type: crate::sync::EntityType,
        scope: Option<String>,
    ) {
        tokio::spawn(async move {
            let scope_ref = scope.as_deref();
            match sdk.sync_entities(entity_type, scope_ref).await {
                Ok(count) => {
                    info!("✅ 后台实体同步完成: {} 条", count);
                }
                Err(e) => {
                    warn!("⚠️ 后台实体同步失败: {}", e);
                }
            }
        });
    }

    /// 是否已完成过首次 Bootstrap（本地曾完整跑完 Friend→Group→Channel→UserSettings）
    ///
    /// 用于「首次登录设备必须强制全量初始化」：若返回 `false`，应阻塞直到 `run_bootstrap_sync()` 成功。
    /// 若当前用户 db/kv 尚未初始化（例如尚未调用 run_bootstrap_sync），返回 `Ok(false)`，不报错。
    pub async fn is_bootstrap_completed(&self) -> Result<bool> {
        self.check_initialized().await?;
        let kv = match self.storage().kv_store().await {
            Some(k) => k,
            None => return Ok(false), // 未初始化视为未完成，由 run_bootstrap_sync 中 ensure 后再同步
        };
        let key = crate::sync::BOOTSTRAP_COMPLETED_KEY;
        let done = kv.get::<&str, u64>(key).await?.is_some();
        Ok(done)
    }

    /// 在 run_bootstrap_sync 入口处调用：检测并初始化当前用户的 db/kv/queue，再启动发送消费者。
    /// 认证后必须运行 run_bootstrap_sync，故 db/kv/queue 统一在此处初始化更合理。
    pub(crate) async fn ensure_user_storage_initialized(&self) -> Result<()> {
        let uid = self
            .user_id()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("未登录，无法初始化用户存储".to_string()))?;
        let uid_str = uid.to_string();
        self.storage.init_user(&uid_str).await?;
        info!("✅ 用户存储已就绪 (db/kv/queue): user_id={}", uid);
        self.start_send_consumer().await?;
        self.start_file_send_consumer().await?;
        Ok(())
    }

    /// 执行启动同步（Bootstrap）：先检测并初始化 db/kv/queue，再按顺序同步 Friend → Group → Channel → UserSettings → sync_all_channels
    ///
    /// 由**生命周期层**在 connect 成功 / resume / foreground 等节点调用。
    /// 全量/增量由 CursorStore 决定；本方法只负责按顺序执行各类型一次，遇错即返。
    /// 成功后写入「Bootstrap 已完成」标记，供 `is_bootstrap_completed()` 判断。
    pub async fn run_bootstrap_sync(&self) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;
        // 先执行 bootstrap sync（内部会 ensure_user_storage_initialized 初始化 KV）
        crate::sync::run_bootstrap_sync(self).await?;
        // 同步完成后写入已完成标记（此时 KV 已初始化）
        let kv = self
            .storage()
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("KV 未初始化".to_string()))?;
        kv.set(crate::sync::BOOTSTRAP_COMPLETED_KEY, &1u64).await?;
        Ok(())
    }

    /// 在后台执行启动同步（不阻塞）；失败只打日志，不重试（重试由外层 Scheduler 负责）
    pub fn run_bootstrap_sync_in_background(sdk: Arc<Self>) {
        tokio::spawn(async move {
            match sdk.run_bootstrap_sync().await {
                Ok(()) => info!("✅ 后台 bootstrap sync 完成"),
                Err(e) => warn!("⚠️ 后台 bootstrap sync 失败: {}", e),
            }
        });
    }

    /// 删除好友
    pub async fn delete_friend(&self, friend_user_id: u64) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::FriendRemoveRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = FriendRemoveRequest {
            user_id: user_id,
            friend_id: friend_user_id,
        };

        let _: bool = client
            .call_rpc_typed(routes::friend::DELETE, request)
            .await?;

        info!("✅ 好友已删除: friend_user={}", friend_user_id);
        Ok(serde_json::json!(true))
    }

    // ========== 群组管理功能 ==========

    /// 创建群组
    ///
    /// # 参数
    /// - `name`: 群组名称
    /// - `member_ids`: 初始成员ID列表
    pub async fn create_group(
        &self,
        name: &str,
        member_ids: Vec<u64>,
    ) -> Result<privchat_protocol::rpc::group::group::GroupCreateResponse> {
        use privchat_protocol::rpc::GroupCreateRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let creator_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = GroupCreateRequest {
            creator_id: creator_id,
            name: name.to_string(),
            description: None,
            member_ids: if member_ids.is_empty() {
                None
            } else {
                Some(member_ids.clone())
            },
        };

        use privchat_protocol::rpc::group::group::GroupCreateResponse;
        let response: GroupCreateResponse = client
            .call_rpc_typed(routes::group::CREATE, request)
            .await?;

        info!(
            "✅ 群组已创建: name={}, members={}, group_id={}",
            name,
            member_ids.len(),
            response.group_id
        );
        Ok(response)
    }

    /// 邀请成员加入群组
    pub async fn invite_to_group(
        &self,
        group_id: u64,
        user_ids: Vec<u64>,
    ) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::GroupMemberAddRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let inviter_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        // 批量添加：逐个调用服务端API
        let mut success_count = 0;
        for user_id in &user_ids {
            let request = GroupMemberAddRequest {
                group_id,
                inviter_id,
                user_id: *user_id,
                role: Some("member".to_string()),
            };

            match client
                .call_rpc_typed::<_, serde_json::Value>(routes::group_member::ADD, request)
                .await
            {
                Ok(_) => success_count += 1,
                Err(e) => warn!("⚠️ 添加成员 {} 失败: {}", user_id, e),
            }
        }

        info!(
            "✅ 成员已邀请加入群组: group={}, 成功={}/{}",
            group_id,
            success_count,
            user_ids.len()
        );
        Ok(serde_json::json!({
            "success": success_count > 0,
            "total": user_ids.len(),
            "success_count": success_count
        }))
    }

    /// 通过二维码加入群组
    ///
    /// 扫描群组二维码后，使用此方法加入群组。
    /// 加入群组后，服务端会自动将用户加入到对应的 channel 中。
    ///
    /// # 参数
    /// - `qr_key`: 二维码 key（从二维码 URL 中提取）
    /// - `token`: 二维码 token（可选，从二维码 URL 中提取）
    /// - `message`: 申请理由（可选，如果需要审批）
    ///
    /// # 返回
    /// - `Ok(GroupQRCodeJoinResponse)`: 加入结果（可能返回 pending 状态，需要审批）
    pub async fn join_group_by_qrcode(
        &self,
        qr_key: String,
        token: Option<String>,
        message: Option<String>,
    ) -> Result<privchat_protocol::rpc::GroupQRCodeJoinResponse> {
        use privchat_protocol::rpc::GroupQRCodeJoinRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = GroupQRCodeJoinRequest {
            user_id,
            qr_key,
            token,
            message,
        };

        let response: privchat_protocol::rpc::GroupQRCodeJoinResponse = client
            .call_rpc_typed(routes::group_qrcode::JOIN, request)
            .await?;

        info!(
            "✅ 已加入群组: group_id={}, status={}",
            response.group_id, response.status
        );

        // 如果成功加入，触发会话列表更新事件
        if response.status == "joined" {
            self.emit_channel_list_update(response.group_id, 2).await;
        }

        Ok(response)
    }

    /// 退出群组
    pub async fn leave_group(&self, group_id: u64) -> Result<bool> {
        use privchat_protocol::rpc::GroupMemberLeaveRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = GroupMemberLeaveRequest {
            group_id,
            user_id: user_id,
        };

        let _: bool = client
            .call_rpc_typed(routes::group_member::LEAVE, request)
            .await?;

        info!("✅ 已退出群组: group={}", group_id);
        Ok(true)
    }

    // ========== 群组成员获取 API（Local-First）==========

    /// 获取群组成员列表（从本地数据库，瞬间返回，5-20ms）
    ///
    /// # 参数
    /// - `group_id`: 群组 ID
    /// - `limit`: 每页数量（可选）
    /// - `offset`: 偏移量（可选）
    ///
    /// # 示例
    /// ```rust
    /// // 获取所有群组成员
    /// let members = sdk.get_group_members(group_id, None, None).await?;
    ///
    /// // 分页获取
    /// let members = sdk.get_group_members(group_id, Some(50), Some(0)).await?;
    /// ```
    pub async fn get_group_members(
        &self,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<crate::storage::entities::ChannelMember>> {
        self.check_initialized().await?;
        self.storage()
            .get_group_members(group_id, limit, offset)
            .await
    }

    /// 从本地数据库获取群列表（分页）
    ///
    /// # 参数
    /// - `limit`: 每页数量
    /// - `offset`: 偏移量
    pub async fn get_groups(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<crate::storage::entities::Group>> {
        self.check_initialized().await?;
        self.storage().get_groups(limit, offset).await
    }

    /// 从本地数据库获取单条用户设置（ENTITY_SYNC_V1 user_settings，只读 DB）
    ///
    /// # 参数
    /// - `key`: 设置键，如 "theme", "notification_enabled"
    ///
    /// # 返回
    /// - `Ok(Some(value))`: 存在则返回 JSON 值（如 `"dark"`, `true`, `123`）
    /// - `Ok(None)`: 不存在
    pub async fn get_user_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        self.check_initialized().await?;
        self.storage().get_user_setting(key).await
    }

    /// 从本地数据库获取当前用户全部设置（用于设置页展示，只读 DB）
    pub async fn get_all_user_settings(
        &self,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>> {
        self.check_initialized().await?;
        self.storage().get_all_user_settings().await
    }

    /// 从服务器同步群组成员列表到本地数据库
    ///
    /// # 参数
    /// - `group_id`: 群组 ID
    ///
    /// # 返回
    /// - `Ok(count)`: 成功同步的成员数量
    ///
    /// # 示例
    /// ```rust
    /// // 同步群组成员：使用统一实体同步
    /// let count = sdk.sync_entities(privchat_sdk::EntityType::GroupMember, Some(&group_id.to_string())).await?;
    /// info!("已同步 {} 个成员", count);
    /// ```
    /// 移除群组成员（需要管理员权限）
    pub async fn remove_group_member(&self, group_id: u64, user_id: u64) -> Result<bool> {
        use privchat_protocol::rpc::GroupMemberRemoveRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let operator_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = GroupMemberRemoveRequest {
            group_id,
            operator_id: operator_id,
            user_id,
        };

        let _: bool = client
            .call_rpc_typed(routes::group_member::REMOVE, request)
            .await?;

        info!("✅ 成员已移出群组: group={}, user={}", group_id, user_id);
        Ok(true)
    }

    // ========== 消息高级功能 ==========

    // ========== 消息获取 API（Local-First）==========

    /// 获取消息历史（从本地数据库，瞬间返回，5-20ms）
    ///
    /// # 参数
    /// - `channel_id`: 频道/会话 ID
    /// - `limit`: 获取数量
    /// - `before_message_id`: 在此消息 ID 之前的消息（用于分页）
    ///
    /// # 示例
    /// ```rust
    /// // 获取最新的 50 条消息
    /// let messages = sdk.get_messages(channel_id, 50, None).await?;
    ///
    /// // 获取更早的消息（分页）
    /// let older_messages = sdk.get_messages(channel_id, 50, Some(last_message_id)).await?;
    /// ```
    pub async fn get_messages(
        &self,
        channel_id: u64,
        limit: u32,
        before_message_id: Option<u64>,
    ) -> Result<Vec<crate::storage::entities::Message>> {
        self.check_initialized().await?;

        // ⭐ 使用 Actor 模型：如果没有 before_message_id，使用 u64::MAX 作为上界
        let before_id = before_message_id.unwrap_or(u64::MAX);
        self.get_messages_before(channel_id, limit, before_id).await
    }

    /// 获取指定 message.id 之前的消息（内部方法，游标为客户端 id）
    async fn get_messages_before(
        &self,
        channel_id: u64,
        limit: u32,
        before_id: u64,
    ) -> Result<Vec<crate::storage::entities::Message>> {
        debug!(
            "[Rust SDK] 📖 准备从本地读: channel_id={}, before_id={}, limit={}",
            channel_id, before_id, limit
        );
        let messages = self
            .storage()
            .get_messages_before(channel_id, before_id, limit)
            .await?;
        debug!(
            "[Rust SDK] 📖 本地读结果: channel_id={}, 返回 {} 条",
            channel_id,
            messages.len()
        );
        debug!(
            "✅ [Local] 查询消息成功: channel_id={}, before_id={}, count={}",
            channel_id,
            before_id,
            messages.len()
        );

        Ok(messages)
    }

    /// 获取频道当前最小的 message.id（用于「加载更早」分页游标）
    pub async fn get_earliest_id(&self, channel_id: u64) -> Result<Option<u64>> {
        self.check_initialized().await?;
        self.storage().get_earliest_id(channel_id).await
    }

    /// 向后分页（加载更早的消息）
    ///
    /// 从指定消息 ID 之前加载更早的消息，用于向上滚动加载历史消息。
    ///
    /// # 参数
    /// - `channel_id`: 频道/会话 ID
    /// - `before_message_id`: 在此消息 ID 之前的消息（通常是当前显示的最早消息 ID）
    /// - `count`: 加载数量
    ///
    /// # 返回
    /// - `Ok(Vec<Message>)`: 加载的消息列表（按时间倒序，最新的在前）
    ///
    /// # 示例
    /// ```rust
    /// // 加载更早的 50 条消息
    /// let older_messages = sdk.paginate_back(channel_id, oldest_message_id, 50).await?;
    /// ```
    pub async fn paginate_back(
        &self,
        channel_id: u64,
        before_message_id: u64,
        count: u32,
    ) -> Result<Vec<crate::storage::entities::Message>> {
        self.check_initialized().await?;

        let messages = self
            .storage()
            .get_messages_before(channel_id, before_message_id, count)
            .await?;

        debug!(
            "✅ [Paginate Back] 加载消息成功: channel_id={}, before_id={}, count={}",
            channel_id,
            before_message_id,
            messages.len()
        );

        // 触发 TimelineDiff 事件（Append 到前面），使用 message.id 作为客户端 id
        if !messages.is_empty() {
            use crate::events::{SDKEvent, TimelineDiffKind, TimelineMessage};
            use std::time::{SystemTime, UNIX_EPOCH};

            let timeline_messages: Vec<TimelineMessage> = messages
                .iter()
                .map(|msg| {
                    let id = msg.id.map(|i| i as u64).unwrap_or(0);
                    TimelineMessage {
                        id,
                        server_message_id: msg.server_message_id,
                        channel_id: msg.channel_id,
                        channel_type: msg.channel_type,
                        from_uid: msg.from_uid,
                        content: msg.content.clone(),
                        message_type: msg.message_type,
                        timestamp: msg.timestamp.map(|t| t as u64).unwrap_or(0),
                        pts: msg.pts as u64,
                    }
                })
                .collect();

            if !timeline_messages.is_empty() {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                self.event_manager
                    .emit(SDKEvent::TimelineDiff {
                        channel_id,
                        diff_kind: TimelineDiffKind::Append {
                            messages: timeline_messages,
                        },
                        timestamp,
                    })
                    .await;
            }
        }

        Ok(messages)
    }

    /// 向前分页（加载更新的消息）
    ///
    /// 从指定消息 ID 之后加载更新的消息，用于向下滚动加载新消息。
    ///
    /// # 参数
    /// - `channel_id`: 频道/会话 ID
    /// - `after_message_id`: 在此消息 ID 之后的消息（通常是当前显示的最新消息 ID）
    /// - `count`: 加载数量
    ///
    /// # 返回
    /// - `Ok(Vec<Message>)`: 加载的消息列表（按时间正序，最早的在前）
    ///
    /// # 示例
    /// ```rust
    /// // 加载更新的 50 条消息
    /// let newer_messages = sdk.paginate_forward(channel_id, newest_message_id, 50).await?;
    /// ```
    pub async fn paginate_forward(
        &self,
        channel_id: u64,
        after_message_id: u64,
        count: u32,
    ) -> Result<Vec<crate::storage::entities::Message>> {
        self.check_initialized().await?;

        let messages = self
            .storage()
            .get_messages_after(channel_id, after_message_id, count)
            .await?;

        debug!(
            "✅ [Paginate Forward] 加载消息成功: channel_id={}, after_id={}, count={}",
            channel_id,
            after_message_id,
            messages.len()
        );

        // 触发 TimelineDiff 事件（Append 到后面），使用 message.id 作为客户端 id
        if !messages.is_empty() {
            use crate::events::{SDKEvent, TimelineDiffKind, TimelineMessage};
            use std::time::{SystemTime, UNIX_EPOCH};

            let timeline_messages: Vec<TimelineMessage> = messages
                .iter()
                .map(|msg| {
                    let id = msg.id.map(|i| i as u64).unwrap_or(0);
                    TimelineMessage {
                        id,
                        server_message_id: msg.server_message_id,
                        channel_id: msg.channel_id,
                        channel_type: msg.channel_type,
                        from_uid: msg.from_uid,
                        content: msg.content.clone(),
                        message_type: msg.message_type,
                        timestamp: msg.timestamp.map(|t| t as u64).unwrap_or(0),
                        pts: msg.pts as u64,
                    }
                })
                .collect();

            if !timeline_messages.is_empty() {
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                self.event_manager
                    .emit(SDKEvent::TimelineDiff {
                        channel_id,
                        diff_kind: TimelineDiffKind::Append {
                            messages: timeline_messages,
                        },
                        timestamp,
                    })
                    .await;
            }
        }

        Ok(messages)
    }

    /// 从服务器同步指定会话的消息历史到本地数据库
    ///
    /// **与 get_messages() 的关系**：sync 只负责入库；get_messages() 只从本地 SQLite 读，两者不冲突。
    /// **与回调的关系**：本方法不触发 on_message_received 等回调；增量由 push 触发的回调通知。
    /// 首次进对话框时：本地空则先调本方法再 get_messages()，由 get_messages() + Reset 给 UI 完整列表。
    ///
    /// # 参数
    /// - `channel_id`: 会话 ID
    /// - `limit`: 每次同步的数量（默认 100）
    ///
    /// # 返回
    /// - `Ok(count)`: 本次新入库的消息数量（已存在的不重复插入）
    pub async fn sync_messages(&self, channel_id: u64, limit: Option<u32>) -> Result<usize> {
        use privchat_protocol::rpc::MessageHistoryGetRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let sync_limit = limit.unwrap_or(100);

        info!(
            "🔄 开始同步频道 {} 的消息历史（最多 {} 条）...",
            channel_id, sync_limit
        );

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = MessageHistoryGetRequest {
            user_id,
            channel_id,
            before_server_message_id: None,
            limit: Some(sync_limit),
        };

        debug!(
            "[Rust SDK] 🔄 准备调用 RPC: route=message_history/get, params={:?}",
            request
        );
        let response: serde_json::Value = client
            .call_rpc_typed(routes::message_history::GET, request)
            .await?;
        debug!(
            "[Rust SDK] 📥 收到服务器响应: {}",
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| "无法序列化".to_string())
        );

        // 释放 client 锁（显式结束借用）
        let _ = client;

        // 解析响应并逐条入库；get_messages() 只从本地 SQLite 读，与 sync 时机无关，只要这里都入库即可
        let mut synced_count = 0;

        if let Some(messages_array) = response.get("messages").and_then(|v| v.as_array()) {
            debug!("[Rust SDK] 📥 收到 {} 条消息", messages_array.len());

            for msg_value in messages_array {
                match self.parse_message_from_json(msg_value, channel_id) {
                    Ok(message) => {
                        // 每条都尝试入库；(channel_id, message_id) 已存在则跳过插入，返回已有 id
                        match self.storage().save_message(&message).await {
                            Ok(row_id) => {
                                synced_count += 1;
                                if row_id > 0 {
                                    if let Some(uid) = self.storage().get_current_user_id().await {
                                        let data_dir = self.config.data_dir.clone();
                                        let base_url = self.config.file_api_base_url.clone();
                                        let http = self.http_client.clone();
                                        let content_for_thumb = message.content.clone();
                                        let created_at_ms = message.created_at;
                                        tokio::spawn(async move {
                                            if let Err(e) = Self::download_thumbnail_after_receive(
                                                data_dir,
                                                base_url,
                                                http,
                                                uid,
                                                row_id,
                                                content_for_thumb,
                                                created_at_ms,
                                            )
                                            .await
                                            {
                                                warn!(
                                                    "同步消息缩略图下载失败: row_id={}, error={:?}",
                                                    row_id, e
                                                );
                                            }
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                debug!("[Rust SDK] ❌ 保存消息失败: channel_id={}, message_id={:?}, error={}", channel_id, message.server_message_id, e);
                                warn!(
                                    "保存消息失败: message_id={:?}, error={}",
                                    message.server_message_id, e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            "[Rust SDK] ❌ 解析消息失败: channel_id={}, error={:?}",
                            channel_id, e
                        );
                        warn!("解析消息失败: {:?}, error={}", msg_value, e);
                    }
                }
            }
        } else {
            debug!("[Rust SDK] ⚠️ 消息响应格式未知或消息为空: {:?}", response);
        }

        debug!(
            "[Rust SDK] ✅ 消息历史同步完成：频道 {}, 本次新入库 {} 条",
            channel_id, synced_count
        );

        Ok(synced_count)
    }

    /// 从 JSON 解析消息对象
    /// 支持服务端字段：sender_id（等价 from_uid）、ISO 字符串 timestamp；消息类型仅由业务扩展 metadata 推断，不依赖服务端
    fn parse_message_from_json(
        &self,
        value: &serde_json::Value,
        channel_id: u64,
    ) -> Result<crate::storage::entities::Message> {
        let now = chrono::Utc::now().timestamp_millis();

        // 支持 from_uid 或 sender_id
        let from_uid = value
            .get("from_uid")
            .and_then(|v| v.as_u64())
            .or_else(|| value.get("sender_id").and_then(|v| v.as_u64()))
            .ok_or_else(|| PrivchatSDKError::Other("缺少 from_uid 或 sender_id".to_string()))?;

        // 支持数字时间戳或 ISO 8601 字符串（如 "2026-01-28T09:49:37.556+00:00"）
        let timestamp = value.get("timestamp").and_then(|v| {
            v.as_i64().or_else(|| {
                v.as_str().and_then(|s| {
                    chrono::DateTime::parse_from_rfc3339(s)
                        .or_else(|_| chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.3f%z"))
                        .ok()
                        .map(|dt| dt.timestamp_millis())
                })
            })
        });

        // 消息类型仅由业务侧决定：从业务扩展字段 metadata 推断，不依赖服务端；默认 text(1)
        let message_type = value
            .get("metadata")
            .and_then(|m| m.as_object())
            .and_then(|o| {
                if o.contains_key("image") {
                    Some(2)
                } else if o.contains_key("video") {
                    Some(3)
                } else if o.contains_key("audio") {
                    Some(4)
                } else if o.contains_key("file") {
                    Some(5)
                } else {
                    None
                }
            })
            .unwrap_or(1);

        Ok(crate::storage::entities::Message {
            id: None,
            server_message_id: value.get("message_id").and_then(|v| v.as_u64()), // JSON 键仍为 message_id
            pts: value.get("pts").and_then(|v| v.as_i64()).unwrap_or(0), // ⭐ message_seq -> pts
            channel_id,
            channel_type: value
                .get("channel_type")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            timestamp,
            from_uid,
            message_type,
            content: value
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            status: value.get("status").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            voice_status: value
                .get("voice_status")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            created_at: value
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now),
            updated_at: value
                .get("updated_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now),
            searchable_word: value
                .get("searchable_word")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            // 协议字段，仅存库；所有操作只用 message.id，无值时写 0
            local_message_id: 0,
            is_deleted: value
                .get("is_deleted")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            setting: value.get("setting").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            order_seq: value.get("order_seq").and_then(|v| v.as_i64()).unwrap_or(0),
            extra: value
                .get("extra")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            flame: value.get("flame").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
            flame_second: value
                .get("flame_second")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            viewed: value.get("viewed").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
            viewed_at: value.get("viewed_at").and_then(|v| v.as_i64()).unwrap_or(0),
            topic_id: value
                .get("topic_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            expire_time: value.get("expire_time").and_then(|v| v.as_i64()),
            expire_timestamp: value.get("expire_timestamp").and_then(|v| v.as_i64()),
            // 消息撤回状态
            revoked: value.get("revoked").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
            revoked_at: value
                .get("revoked_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            revoked_by: value.get("revoked_by").and_then(|v| v.as_u64()),
        })
    }

    /// 在后台异步同步消息历史
    ///
    /// # 参数
    /// - `sdk`: SDK 的 Arc 引用
    /// - `channel_id`: 会话 ID
    /// - `limit`: 同步数量限制
    pub fn sync_messages_in_background(sdk: Arc<Self>, channel_id: u64, limit: Option<u32>) {
        tokio::spawn(async move {
            match sdk.sync_messages(channel_id, limit).await {
                Ok(count) => {
                    info!("✅ 后台同步消息完成: 频道 {}, {} 条", channel_id, count);
                }
                Err(e) => {
                    warn!("⚠️ 后台同步消息失败: 频道 {}, {}", channel_id, e);
                }
            }
        });
    }

    /// 搜索会话内的消息
    ///
    /// 在指定会话内搜索消息（从本地数据库）。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `query`: 搜索关键词
    /// - `limit`: 每页数量
    /// - `offset`: 偏移量（可选）
    ///
    /// # 返回
    /// - `Ok(SearchPage)`: 搜索结果页面
    ///
    /// # 示例
    /// ```rust
    /// // 搜索会话内的消息
    /// let page = sdk.search_channel(channel_id, "关键词", 20, Some(0)).await?;
    /// ```
    pub async fn search_channel(
        &self,
        channel_id: u64,
        query: &str,
        limit: u32,
        offset: Option<u32>,
    ) -> Result<crate::events::SearchPage> {
        self.check_initialized().await?;

        // 从本地数据库搜索
        let messages = self
            .storage
            .search_messages(channel_id, 1, query, Some(limit))
            .await?;

        // 转换为 SearchHit
        let hits: Vec<crate::events::SearchHit> = messages
            .iter()
            .filter_map(|msg| {
                msg.server_message_id
                    .map(|msg_id| crate::events::SearchHit {
                        channel_id: msg.channel_id,
                        server_message_id: msg_id,
                        sender: msg.from_uid,
                        body: msg.content.clone(),
                        timestamp_ms: msg.timestamp.map(|t| t as u64).unwrap_or(0),
                    })
            })
            .collect();

        // 计算下一个偏移量
        let next_offset = if hits.len() == limit as usize {
            Some(offset.unwrap_or(0) + limit)
        } else {
            None
        };

        Ok(crate::events::SearchPage { hits, next_offset })
    }

    /// 搜索消息（本地搜索，不调服务端）
    ///
    /// # 参数
    /// - `query`: 搜索关键词
    /// - `channel_id`: 可选的会话 ID（限定范围）；为 None 时返回空结果
    pub async fn search_messages(
        &self,
        query: &str,
        channel_id: Option<&str>,
    ) -> Result<serde_json::Value> {
        self.check_initialized().await?;
        let channel_type = 1i32;
        let limit = 100u32;
        let channel_id_u64 = channel_id.and_then(|s| s.parse::<u64>().ok());
        let messages = match channel_id_u64 {
            Some(cid) => {
                self.storage
                    .search_messages(cid, channel_type, query, Some(limit))
                    .await?
            }
            None => vec![],
        };
        let arr: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let ts = m.timestamp.unwrap_or(m.created_at);
                serde_json::json!({
                    "message_id": m.server_message_id,
                    "channel_id": m.channel_id,
                    "channel_type": m.channel_type,
                    "from_uid": m.from_uid,
                    "content": m.content,
                    "timestamp": ts,
                })
            })
            .collect();
        Ok(serde_json::json!({ "messages": arr }))
    }

    // ========== 黑名单功能 ==========

    /// 添加到黑名单
    pub async fn add_to_blacklist(&self, blocked_user_id: u64) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::BlacklistAddRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = BlacklistAddRequest {
            user_id: user_id,
            blocked_user_id,
        };

        let response = client
            .call_rpc_typed(routes::blacklist::ADD, request)
            .await?;

        info!("✅ 用户已加入黑名单: {}", blocked_user_id);
        Ok(response)
    }

    /// 从黑名单移除
    pub async fn remove_from_blacklist(&self, blocked_user_id: u64) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::BlacklistRemoveRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = BlacklistRemoveRequest {
            user_id: user_id,
            blocked_user_id,
        };

        let response = client
            .call_rpc_typed(routes::blacklist::REMOVE, request)
            .await?;

        info!("✅ 用户已从黑名单移除: {}", blocked_user_id);
        Ok(response)
    }

    /// 获取黑名单列表
    pub async fn get_blacklist(&self) -> Result<serde_json::Value> {
        use privchat_protocol::rpc::BlacklistListRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = BlacklistListRequest { user_id: user_id };

        let response = client
            .call_rpc_typed(routes::blacklist::LIST, request)
            .await?;

        Ok(response)
    }

    // ========== 会话管理功能 ==========

    // ========== 会话获取 API（Local-First）==========

    /// 获取会话列表（从本地数据库，瞬间返回，5-20ms）
    ///
    /// # 参数
    /// - `query`: 会话查询条件
    ///
    /// # 示例
    /// ```rust
    /// let query = ChannelQuery {
    ///     limit: Some(50),
    ///     ..Default::default()
    /// };
    /// let channels = sdk.get_channels(&query).await?;
    /// ```
    pub async fn get_channels(
        &self,
        query: &crate::storage::entities::ChannelQuery,
    ) -> Result<Vec<crate::storage::entities::Channel>> {
        self.check_initialized().await?;

        self.storage().get_channels(query).await
    }

    /// 辅助方法：发送会话列表重置事件
    ///
    /// 获取所有会话，构建完整的 ChannelListEntry 列表并发送 Reset 事件
    async fn emit_channel_list_reset(&self) {
        let storage = self.storage.clone();
        let event_manager = self.event_manager.clone();

        tokio::spawn(async move {
            // 1. 获取所有会话
            let query = crate::storage::entities::ChannelQuery {
                limit: None,
                offset: None,
                ..Default::default()
            };

            let channels = match storage.get_channels(&query).await {
                Ok(convs) => convs,
                Err(e) => {
                    warn!("获取会话列表失败: {:?}", e);
                    return;
                }
            };

            // 2. 为每个会话构建 ChannelListEntry
            let mut entries = Vec::new();

            for conv in channels {
                // 获取频道信息
                let channel = storage
                    .get_channel(conv.channel_id, conv.channel_type)
                    .await
                    .ok()
                    .flatten();

                // 获取群组成员数量（如果是群聊）
                let member_count = if conv.channel_type == 2 {
                    storage
                        .get_group_members(conv.channel_id, None, None)
                        .await
                        .map(|members| members.len() as u32)
                        .unwrap_or(0)
                } else {
                    0
                };

                // 构建 ChannelListEntry
                let entry = Self::build_channel_list_entry(
                    &conv,
                    channel.as_ref(),
                    member_count,
                    None, // Reset 事件不需要 latest_event
                );

                entries.push(entry);
            }

            // 3. 发送 Reset 事件
            use crate::events::ChannelListUpdateKind;
            use std::time::{SystemTime, UNIX_EPOCH};

            let entries_count = entries.len();
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let conv_event = crate::events::SDKEvent::ChannelListUpdate {
                update_kind: ChannelListUpdateKind::Reset { channels: entries },
                timestamp,
            };

            event_manager.emit(conv_event).await;
            info!("✅ 会话列表重置事件已发送，共 {} 个会话", entries_count);
        });
    }

    /// 获取会话列表条目（包含完整信息）
    ///
    /// 从 Channel 和 Channel 获取完整信息，构建 ChannelListEntry 列表
    /// 这是业务逻辑层，返回 SDK 层的 ChannelListEntry
    pub async fn get_channel_list_entries(
        &self,
        query: &crate::storage::entities::ChannelQuery,
    ) -> Result<Vec<crate::events::ChannelListEntry>> {
        self.check_initialized().await?;

        // 1. 获取会话列表
        let channels = self.storage().get_channels(query).await?;

        // 2. 分类收集需要查询的 ID
        let mut dm_user_ids: Vec<u64> = Vec::new();
        let mut group_ids: Vec<u64> = Vec::new();
        for c in &channels {
            if c.channel_type == 0 || c.channel_type == 1 {
                // 私聊：channel_id 就是对方用户 ID
                dm_user_ids.push(c.channel_id);
            } else if c.channel_type == 2 {
                // 群聊：channel_id 就是群 ID
                group_ids.push(c.channel_id);
            }
        }

        // 3. 批量获取用户信息（私聊）
        let user_map: std::collections::HashMap<u64, crate::storage::entities::User> =
            if !dm_user_ids.is_empty() {
                self.storage()
                    .get_users_by_ids(dm_user_ids)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|u| (u.user_id, u))
                    .collect()
            } else {
                std::collections::HashMap::new()
            };

        // 4. 批量获取群信息（群聊）
        let group_map: std::collections::HashMap<u64, crate::storage::entities::Group> =
            if !group_ids.is_empty() {
                let mut map = std::collections::HashMap::new();
                for gid in group_ids {
                    if let Ok(Some(g)) = self.storage().get_group(gid).await {
                        map.insert(gid, g);
                    }
                }
                map
            } else {
                std::collections::HashMap::new()
            };

        // 5. 为每个会话构建 ChannelListEntry
        let mut entries = Vec::new();

        for conv in channels {
            // 构建基础 ChannelListEntry
            let mut entry = Self::build_channel_list_entry(&conv, None, 0, None);

            // 根据 channel_type 获取名称和头像
            if conv.channel_type == 0 || conv.channel_type == 1 {
                // 私聊：从 user 表获取（优先级：alias > nickname > username）
                if let Some(user) = user_map.get(&conv.channel_id) {
                    // 获取显示名
                    let name = user
                        .alias
                        .as_ref()
                        .filter(|s| !s.is_empty())
                        .or(user.nickname.as_ref().filter(|s| !s.is_empty()))
                        .or(user.username.as_ref().filter(|s| !s.is_empty()))
                        .cloned()
                        .unwrap_or_else(|| format!("用户{}", conv.channel_id));
                    entry.name = name;

                    // 获取头像
                    if !user.avatar.is_empty() {
                        entry.avatar_url = Some(user.avatar.clone());
                    }
                } else {
                    // user 表没有数据，使用默认名称
                    entry.name = format!("用户{}", conv.channel_id);
                }
            } else if conv.channel_type == 2 {
                // 群聊：从 group 表获取
                if let Some(group) = group_map.get(&conv.channel_id) {
                    entry.name = group
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("群聊{}", conv.channel_id));
                    if !group.avatar.is_empty() {
                        entry.avatar_url = Some(group.avatar.clone());
                    }
                    // 获取群成员数量
                    entry.member_count = self
                        .storage()
                        .get_group_members(conv.channel_id, None, None)
                        .await
                        .map(|m| m.len() as u32)
                        .unwrap_or(0);
                } else {
                    entry.name = format!("群聊{}", conv.channel_id);
                }
            }

            // 设置最后消息内容和时间
            if !conv.last_msg_content.is_empty() {
                // channel 表有缓存的最后消息
                entry.latest_event = Some(crate::events::LatestChannelEvent {
                    event_type: "message".to_string(),
                    content: conv.last_msg_content.clone(),
                    timestamp: conv.last_msg_timestamp.unwrap_or(0) as u64,
                });
                entry.last_ts = conv.last_msg_timestamp.unwrap_or(0) as u64;
            } else {
                // channel 表没有缓存，从 message 表查询最后一条消息
                if let Ok(messages) = self
                    .storage()
                    .get_messages_before(conv.channel_id, u64::MAX, 1)
                    .await
                {
                    if let Some(last_msg) = messages.first() {
                        // 获取消息内容摘要
                        let content = Self::get_message_preview(&last_msg.content);
                        let ts = last_msg.timestamp.unwrap_or(0) as u64;
                        entry.latest_event = Some(crate::events::LatestChannelEvent {
                            event_type: "message".to_string(),
                            content,
                            timestamp: ts,
                        });
                        entry.last_ts = ts;
                    }
                }
            }

            entries.push(entry);
        }

        Ok(entries)
    }

    /// 辅助方法：从消息内容提取预览文本
    fn get_message_preview(content: &str) -> String {
        // 尝试解析 JSON 格式
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
            // 优先取 "content" 字段
            if let Some(text) = json.get("content").and_then(|v| v.as_str()) {
                return text.to_string();
            }
            // 其次取 "text" 字段
            if let Some(text) = json.get("text").and_then(|v| v.as_str()) {
                return text.to_string();
            }
            // 检查消息类型，返回对应的描述
            if let Some(msg_type) = json.get("message_type").and_then(|v| v.as_str()) {
                return match msg_type {
                    "image" => "[图片]".to_string(),
                    "video" => "[视频]".to_string(),
                    "voice" | "audio" => "[语音]".to_string(),
                    "file" => "[文件]".to_string(),
                    "location" => "[位置]".to_string(),
                    "red_packet" | "red_package" => "[红包]".to_string(),
                    "sticker" => "[表情]".to_string(),
                    _ => "[消息]".to_string(),
                };
            }
        }
        // 不是 JSON，直接返回原内容（纯文本消息）
        content.to_string()
    }

    /// 辅助方法：构建 ChannelListEntry（用于私聊名称处理）
    pub(crate) fn build_channel_list_entry_with_user_id(
        conv: &crate::storage::entities::Channel,
        channel: Option<&crate::storage::entities::Channel>,
        member_count: u32,
        latest_event: Option<crate::events::LatestChannelEvent>,
    ) -> crate::events::ChannelListEntry {
        // 先调用原方法构建基本条目
        let mut entry = Self::build_channel_list_entry(conv, channel, member_count, latest_event);

        // 私聊：空名称或 "Channel {id}" 时，显示用户昵称(channel_name)或「用户 {对端 user_id}」；无昵称时留空，不填充占位符
        // 严禁使用 channel_id 作为展示名（channel_id 对用户无感）
        let peer_display = |c: &crate::storage::entities::Channel| -> String {
            if !c.channel_name.is_empty() {
                c.channel_name.clone()
            } else if !c.username.is_empty() {
                format!("用户 {}", c.username)
            } else {
                String::new()
            }
        };
        if (conv.channel_type == 0 || conv.channel_type == 1)
            && (entry.name.is_empty() || entry.name.starts_with("Channel "))
        {
            if let Some(ch) = channel {
                entry.name = peer_display(ch);
            } else {
                entry.name = if !conv.username.is_empty() {
                    format!("用户 {}", conv.username)
                } else {
                    String::new()
                };
            }
        }
        if (conv.channel_type == 0 || conv.channel_type == 1)
            && (entry.name == "User " || entry.name == "User")
        {
            if let Some(ch) = channel {
                entry.name = peer_display(ch);
            } else {
                entry.name = if !conv.username.is_empty() {
                    format!("用户 {}", conv.username)
                } else {
                    String::new()
                };
            }
        }
        // 群聊：空名称或 "Channel {id}" 时，用 channel_name 作为群名
        if conv.channel_type == 2 && (entry.name.is_empty() || entry.name.starts_with("Channel ")) {
            if let Some(ch) = channel {
                if !ch.channel_name.is_empty() {
                    entry.name = ch.channel_name.clone();
                }
            }
        }

        entry
    }

    /// 辅助方法：构建 ChannelListEntry
    ///
    /// 从 Channel、Channel 和可选的最新消息事件构建完整的会话列表条目
    pub(crate) fn build_channel_list_entry(
        conv: &crate::storage::entities::Channel,
        channel: Option<&crate::storage::entities::Channel>,
        member_count: u32,
        latest_event: Option<crate::events::LatestChannelEvent>,
    ) -> crate::events::ChannelListEntry {
        use crate::events::{ChannelListEntry, LatestChannelEvent};

        // 如果没有传入 latest_event，但有 last_msg_content，则从中构建
        let latest_event = latest_event.or_else(|| {
            if !conv.last_msg_content.is_empty() {
                Some(LatestChannelEvent {
                    event_type: "message".to_string(),
                    content: conv.last_msg_content.clone(),
                    timestamp: conv.last_msg_timestamp.unwrap_or(0) as u64,
                })
            } else {
                None
            }
        });

        let name = channel
            .map(|c| {
                if !c.channel_remark.is_empty() {
                    c.channel_remark.clone()
                } else {
                    c.channel_name.clone()
                }
            })
            .unwrap_or_else(|| format!("Channel {}", conv.channel_id));

        let avatar_url = channel.and_then(|c| {
            if !c.avatar.is_empty() {
                Some(c.avatar.clone())
            } else {
                None
            }
        });

        let is_favourite = channel.map(|c| c.save == 1).unwrap_or(false);

        let notifications = channel
            .map(|c| {
                if c.mute == 1 {
                    0
                } else {
                    conv.unread_count as u32
                }
            })
            .unwrap_or(conv.unread_count as u32);

        let marked_unread = channel.map(|c| c.top == 1).unwrap_or(false);

        ChannelListEntry {
            channel_id: conv.channel_id,
            channel_type: conv.channel_type,
            name,
            last_ts: conv.last_msg_timestamp.unwrap_or(0) as u64,
            notifications,
            messages: conv.unread_count as u32,
            mentions: 0, // TODO: 从消息中提取 @mention 信息
            marked_unread,
            is_favourite,
            is_low_priority: {
                // 从 Channel extra 中获取 low_priority
                serde_json::from_str::<serde_json::Value>(&conv.extra)
                    .ok()
                    .and_then(|extra| extra.get("low_priority")?.as_bool())
                    .unwrap_or(false)
            },
            avatar_url,
            is_dm: conv.channel_type == 1,
            is_encrypted: false, // TODO: 从 Channel 中获取加密状态
            member_count,
            topic: None, // TODO: 从 Channel 中获取话题信息
            latest_event,
        }
    }

    /// 标记会话为已读
    ///
    /// 将指定会话的未读消息数清零
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `channel_type`: 频道类型 (1=私聊, 2=群聊)
    ///
    /// # 示例
    /// ```rust
    /// // 标记私聊会话已读
    /// sdk.mark_channel_read(12345, 1).await?;
    /// ```
    pub async fn mark_channel_read(&self, channel_id: u64, channel_type: i32) -> Result<()> {
        self.check_initialized().await?;

        // 参数验证
        if channel_type != 1 && channel_type != 2 {
            return Err(PrivchatSDKError::InvalidInput(format!(
                "无效的 channel_type: {}，必须是 1(私聊) 或 2(群聊)",
                channel_type
            )));
        }

        debug!(
            "标记会话已读: channel_id={}, channel_type={}",
            channel_id, channel_type
        );

        // 调用存储层标记已读
        self.storage
            .mark_channel_read(channel_id, channel_type)
            .await?;

        // 发送会话列表更新事件
        self.emit_channel_list_update(channel_id, channel_type)
            .await;

        debug!("✅ 会话已标记为已读: channel_id={}", channel_id);

        Ok(())
    }

    /// 辅助方法：发送会话列表更新事件
    ///
    /// 从 Channel 和 Channel 获取完整信息，构建 ChannelListEntry 并发送事件
    async fn emit_channel_list_update(&self, channel_id: u64, channel_type: i32) {
        let storage = self.storage.clone();
        let event_manager = self.event_manager.clone();

        tokio::spawn(async move {
            // 1. 获取会话信息
            let query = crate::storage::entities::ChannelQuery {
                limit: None,
                offset: None,
                channel_id: Some(channel_id),
                channel_type: Some(channel_type),
                ..Default::default()
            };

            let conv = match storage.get_channels(&query).await {
                Ok(channels) => channels.first().cloned(),
                Err(e) => {
                    warn!("获取会话信息失败: {:?}", e);
                    None
                }
            };

            // 2. 获取频道信息
            let channel = match storage.get_channel(channel_id, channel_type).await {
                Ok(ch) => ch,
                Err(e) => {
                    warn!("获取频道信息失败: {:?}", e);
                    None
                }
            };

            // 3. 获取群组成员数量（如果是群聊）
            let member_count = if channel_type == 2 {
                match storage.get_group_members(channel_id, None, None).await {
                    Ok(members) => members.len() as u32,
                    Err(_) => 0,
                }
            } else {
                0
            };

            // 4. 构建 ChannelListEntry
            if let Some(conv) = conv {
                use crate::events::ChannelListUpdateKind;
                use std::time::{SystemTime, UNIX_EPOCH};

                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                let entry = Self::build_channel_list_entry(
                    &conv,
                    channel.as_ref(),
                    member_count,
                    None, // 已读操作不更新最新消息
                );

                let conv_event = SDKEvent::ChannelListUpdate {
                    update_kind: ChannelListUpdateKind::Update { channel: entry },
                    timestamp,
                };
                event_manager.emit(conv_event).await;
            }
        });
    }

    /// 从 JSON 解析会话对象
    /// current_user_id 用于私聊时从 members 中取对端用户的 display_name 和 uid 作为 channel_name / username
    #[allow(dead_code)]
    fn parse_channel_from_json(
        &self,
        value: &serde_json::Value,
        current_user_id: u64,
    ) -> Result<crate::storage::entities::Channel> {
        let now = chrono::Utc::now().timestamp_millis();

        // 支持两种字段名：id 或 channel_id（服务器返回的是 id）
        let channel_id = value
            .get("id")
            .and_then(|v| v.as_u64())
            .or_else(|| value.get("channel_id").and_then(|v| v.as_u64()))
            .ok_or_else(|| PrivchatSDKError::Other("缺少 id 或 channel_id".to_string()))?;

        // 解析 channel_type（支持字符串 "Direct"/"Group" 或数字 0/1/2）
        let channel_type = value
            .get("channel_type")
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    match s {
                        "Direct" => Some(0),
                        "Group" => Some(2),
                        _ => Some(0),
                    }
                } else {
                    v.as_i64().map(|i| i as i32)
                }
            })
            .unwrap_or(0);

        // 私聊：从 members 取对端用户的 display_name 和 uid，用于 channel_name / username
        let direct_user1_id = value.get("direct_user1_id").and_then(|v| v.as_u64());
        let direct_user2_id = value.get("direct_user2_id").and_then(|v| v.as_u64());
        let other_uid = (channel_type == 0)
            .then(|| match (direct_user1_id, direct_user2_id) {
                (Some(a), Some(b)) if a == current_user_id => Some(b),
                (Some(a), Some(_b)) => Some(a),
                (Some(a), None) if a != current_user_id => Some(a),
                (None, Some(b)) if b != current_user_id => Some(b),
                _ => None,
            })
            .flatten();
        let (channel_name_from_members, username_from_members) = if channel_type == 0 {
            let (name, uid_str) = if let Some(uid) = other_uid {
                let display_name = value
                    .get("members")
                    .and_then(|m| m.get(uid.to_string()))
                    .and_then(|mem| mem.get("display_name").and_then(|v| v.as_str()))
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                let name = display_name.unwrap_or_default();
                (name, uid.to_string())
            } else {
                (String::new(), String::new())
            };
            (name, uid_str)
        } else {
            (String::new(), String::new())
        };

        Ok(crate::storage::entities::Channel {
            id: None,
            channel_id,
            channel_type,
            last_local_message_id: value
                .get("last_message_id")
                .and_then(|v| v.as_u64())
                .or_else(|| value.get("last_local_message_id").and_then(|v| v.as_u64()))
                .unwrap_or(0),
            last_msg_timestamp: value
                .get("last_message_at")
                .and_then(|v| {
                    // 支持 DateTime 字符串或时间戳
                    if let Some(ts_str) = v.as_str() {
                        // 尝试解析 ISO 8601 格式
                        chrono::DateTime::parse_from_rfc3339(ts_str)
                            .ok()
                            .map(|dt| dt.timestamp_millis())
                    } else {
                        v.as_i64()
                    }
                })
                .or_else(|| value.get("last_msg_timestamp").and_then(|v| v.as_i64())),
            last_msg_content: value
                .get("last_msg_content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            unread_count: value
                .get("unread_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            is_deleted: value
                .get("is_deleted")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            version: value.get("version").and_then(|v| v.as_i64()).unwrap_or(0),
            extra: value
                .get("extra")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            last_msg_pts: value
                .get("last_msg_pts") // ⭐ last_msg_seq -> last_msg_pts
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            // 频道信息字段（使用默认值）
            show_nick: value.get("show_nick").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            // 私聊时 username 必须为对端用户 id，不能是 current_user_id；用已算好的 other_uid
            username: if !username_from_members.is_empty() {
                username_from_members
            } else if channel_type == 0 {
                other_uid.map(|uid| uid.to_string()).unwrap_or_else(|| {
                    value
                        .get("username")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                })
            } else {
                value
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            },
            channel_name: if !channel_name_from_members.is_empty() {
                channel_name_from_members
            } else {
                value
                    .get("channel_name")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        value
                            .get("metadata")
                            .and_then(|m| m.get("name"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or("")
                    .to_string()
            },
            channel_remark: value
                .get("channel_remark")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            top: value.get("top").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            mute: value.get("mute").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            save: value.get("save").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            forbidden: value.get("forbidden").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            follow: value.get("follow").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            receipt: value.get("receipt").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            status: value.get("status").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
            invite: value.get("invite").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            robot: value.get("robot").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            online: value.get("online").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
            last_offline: value
                .get("last_offline")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
            avatar: value
                .get("avatar")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            category: value
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            created_at: value
                .get("created_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now),
            updated_at: value
                .get("updated_at")
                .and_then(|v| v.as_i64())
                .unwrap_or(now),
            avatar_cache_key: value
                .get("avatar_cache_key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            remote_extra: value
                .get("remote_extra")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            flame: value.get("flame").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
            flame_second: value
                .get("flame_second")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            device_flag: value
                .get("device_flag")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            parent_channel_id: value
                .get("parent_channel_id")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            parent_channel_type: value
                .get("parent_channel_type")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
        })
    }

    /// 置顶会话
    /// 置顶/取消置顶会话
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `pin`: true 表示置顶，false 表示取消置顶
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功
    pub async fn pin_channel(&self, channel_id: u64, pin: bool) -> Result<bool> {
        use privchat_protocol::rpc::ChannelPinRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let channel_id = channel_id;

        let request = ChannelPinRequest {
            user_id: user_id,
            channel_id,
            pinned: pin,
        };

        let _: bool = client.call_rpc_typed(routes::channel::PIN, request).await?;

        info!("✅ 会话置顶状态已更新: channel={}, pin={}", channel_id, pin);

        // 发送会话列表更新事件
        // 需要从响应中获取 channel_type，这里假设从本地数据库获取
        let storage = self.storage.clone();
        let channel_type = storage
            .get_channel(channel_id, 1)
            .await
            .ok()
            .flatten()
            .map(|ch| ch.channel_type)
            .unwrap_or(1); // 默认为私聊

        self.emit_channel_list_update(channel_id, channel_type)
            .await;

        Ok(true)
    }

    /// 隐藏频道
    ///
    /// 隐藏频道不会删除频道，只是不在用户的会话列表中显示。
    /// 好友关系和群组关系仍然保留。
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功
    pub async fn hide_channel(&self, channel_id: u64) -> Result<bool> {
        use privchat_protocol::rpc::ChannelHideRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        // 先获取 channel_type，用于发送隐藏事件
        let storage = self.storage.clone();
        let _channel_type = storage
            .get_channel(channel_id, 1)
            .await
            .ok()
            .flatten()
            .map(|ch| ch.channel_type)
            .unwrap_or(1); // 默认为私聊

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let channel_id = channel_id;

        let request = ChannelHideRequest {
            user_id: user_id,
            channel_id,
        };

        let _: bool = client
            .call_rpc_typed(routes::channel::HIDE, request)
            .await?;

        info!("✅ 频道已隐藏: channel={}", channel_id);

        // 发送会话列表更新事件
        use crate::events::{ChannelListUpdateKind, SDKEvent};
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conv_event = SDKEvent::ChannelListUpdate {
            update_kind: ChannelListUpdateKind::Remove { channel_id },
            timestamp,
        };

        self.event_manager.emit(conv_event).await;

        Ok(true)
    }

    /// 设置频道静音
    ///
    /// 设置频道静音后，该频道的新消息将不会推送通知。
    /// 这是用户个人的偏好设置，适用于私聊和群聊。
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `muted`: true 表示静音，false 表示取消静音
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功
    pub async fn mute_channel(&self, channel_id: u64, muted: bool) -> Result<bool> {
        use privchat_protocol::rpc::ChannelMuteRequest;

        self.check_initialized().await?;
        self.check_connected().await?;

        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let request = ChannelMuteRequest {
            user_id,
            channel_id,
            muted,
        };

        let _: bool = client
            .call_rpc_typed(routes::channel::MUTE, request)
            .await?;

        info!(
            "✅ 频道已{}: channel={}",
            if muted { "静音" } else { "取消静音" },
            channel_id
        );

        Ok(true)
    }

    /// 获取会话的未读统计
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    ///
    /// # 返回
    /// - `Ok(UnreadStats)`: 未读统计（messages, notifications, mentions）
    pub async fn channel_unread_stats(
        &self,
        channel_id: u64,
    ) -> Result<crate::events::UnreadStats> {
        self.check_initialized().await?;

        let _user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        // 获取会话信息
        let storage = self.storage.clone();
        let query = crate::storage::entities::ChannelQuery {
            channel_id: Some(channel_id),
            ..Default::default()
        };

        let channels = storage.get_channels(&query).await?;
        let conv = channels
            .first()
            .ok_or_else(|| PrivchatSDKError::InvalidInput("会话不存在".to_string()))?;

        // 获取未读消息数（从会话表）
        let messages = conv.unread_count as u64;

        // 获取 @ 提及数（从 mention 表）
        // 注意：当前 MentionDao 需要直接连接，暂时使用 0（后续可以通过 db_actor 扩展）
        // TODO: 通过 db_actor 添加获取未读提及数的方法
        let mentions = 0u64; // 暂时返回 0，后续实现

        // notifications 暂时等于 messages（后续可以扩展）
        let notifications = messages;

        Ok(crate::events::UnreadStats {
            messages,
            notifications,
            mentions,
        })
    }

    /// 获取自己的最后已读位置
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    ///
    /// # 返回
    /// - `Ok((Option<u64>, Option<u64>))`: (message_id, timestamp) 或 (None, None) 如果未读取
    pub async fn own_last_read(&self, _channel_id: u64) -> Result<(Option<u64>, Option<u64>)> {
        self.check_initialized().await?;

        let _user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        // 从 channel_read_states 表获取最后已读位置
        // 暂时返回 None（后续可以通过 db_actor 扩展）
        // TODO: 通过 db_actor 添加获取最后已读位置的方法
        Ok((None, None))
    }

    /// 标记完全已读到指定消息
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `message_id`: 要标记为已读的消息 ID
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn mark_fully_read_at(&self, channel_id: u64, _message_id: u64) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let _user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        // 标记会话为已读（简化实现）
        // TODO: 通过 db_actor 添加获取消息 pts 的方法，然后更新 channel_read_states
        // 暂时使用 mark_channel_read 来标记已读
        self.storage.mark_channel_read(channel_id, 1).await?;

        // 发送会话列表更新事件
        self.emit_channel_list_update(channel_id, 1).await;

        Ok(())
    }

    /// 离开会话
    ///
    /// 离开一个群聊会话（私聊不能离开，只能删除）。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功
    ///
    /// # 示例
    /// ```rust
    /// // 离开群聊
    /// let success = sdk.leave_channel(channel_id).await?;
    /// ```
    pub async fn leave_channel(&self, channel_id: u64) -> Result<bool> {
        // 对于群聊，使用 leave_group 方法
        // 这里提供一个统一的接口
        self.leave_group(channel_id).await?;

        // 发送会话列表删除事件
        use crate::events::{ChannelListUpdateKind, SDKEvent};
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conv_event = SDKEvent::ChannelListUpdate {
            update_kind: ChannelListUpdateKind::Remove {
                channel_id: channel_id,
            },
            timestamp,
        };

        self.event_manager.emit(conv_event).await;

        Ok(true)
    }

    /// 获取会话成员列表
    ///
    /// 获取会话的成员列表（主要用于群聊）。
    ///
    /// 添加会话成员
    ///
    /// 向会话添加成员（主要用于群聊）。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `channel_type`: 会话类型（1: 私聊, 2: 群聊）
    /// - `user_ids`: 要添加的用户ID列表
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功（至少成功添加一个用户）
    ///
    /// # 示例
    /// ```rust
    /// // 添加群聊成员
    /// let success = sdk.add_channel_members(channel_id, 2, vec![user1, user2]).await?;
    /// ```
    pub async fn add_channel_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_ids: Vec<u64>,
    ) -> Result<bool> {
        // 对于群聊，使用 invite_to_group
        if channel_type == 2 {
            let result = self.invite_to_group(channel_id, user_ids).await?;
            let success = result
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            return Ok(success);
        }

        // 私聊不能添加成员
        Err(PrivchatSDKError::InvalidInput(
            "私聊会话不能添加成员".to_string(),
        ))
    }

    /// 移除会话成员
    ///
    /// 从会话移除成员（主要用于群聊）。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `channel_type`: 会话类型（1: 私聊, 2: 群聊）
    /// - `user_id`: 要移除的用户ID
    ///
    /// # 返回
    /// - `Ok(bool)`: 操作成功
    ///
    /// # 示例
    /// ```rust
    /// // 移除群聊成员
    /// let success = sdk.remove_channel_member(channel_id, 2, user_id).await?;
    /// ```
    pub async fn remove_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<bool> {
        // 对于群聊，使用 remove_group_member
        if channel_type == 2 {
            return self.remove_group_member(channel_id, user_id).await;
        }

        // 私聊不能移除成员
        Err(PrivchatSDKError::InvalidInput(
            "私聊会话不能移除成员".to_string(),
        ))
    }

    // ========== 通用RPC调用接口 ==========

    /// 通用RPC调用方法（用于调用任何未封装的RPC接口）
    ///
    /// # 参数
    /// - `route`: RPC路由路径
    /// - `params`: 请求参数（JSON格式）
    pub async fn rpc_call(
        &self,
        route: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.check_initialized().await?;
        self.check_connected().await?;

        let mut client = self.client.write().await;
        let client = client.as_mut().ok_or(PrivchatSDKError::NotConnected)?;

        let response = client.call_rpc(route, params).await?;

        Ok(response)
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
    pub fn config(&self) -> &PrivchatConfig {
        &self.config
    }

    /// 获取当前用户 ID
    pub async fn user_id(&self) -> Option<u64> {
        let state = self.state.read().await;
        state.current_user_id
    }

    // ========== 事件订阅相关方法 ==========

    /// 订阅 SDK 事件流
    ///
    /// 返回一个事件接收器，可以用来接收所有类型的 SDK 事件。
    /// 适用于需要自定义事件处理逻辑的场景（如 FFI 层）。
    ///
    /// # 返回
    /// - `broadcast::Receiver<SDKEvent>`: 事件接收器
    ///
    /// # 示例
    /// ```rust
    /// let mut receiver = sdk.subscribe_events().await;
    ///
    /// loop {
    ///     match receiver.recv().await {
    ///         Ok(event) => {
    ///             // 处理事件
    ///             println!("收到事件: {:?}", event);
    ///         }
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    ///
    /// # 注意
    /// - 如果不需要自定义处理，建议使用 `on_message_received` 等回调方法
    /// - 每个订阅者都会收到所有事件的副本
    /// - 如果处理速度跟不上，可能会丢失事件（lagged）
    pub async fn subscribe_events(&self) -> broadcast::Receiver<SDKEvent> {
        self.event_manager.subscribe().await
    }

    /// 获取事件管理器（仅供内部使用）
    ///
    /// 注意：此方法仅供 SDK 内部使用，不对外暴露。
    /// 外部调用者应使用 `subscribe_events()` 方法订阅事件。
    #[allow(dead_code)]
    pub(crate) fn events(&self) -> &Arc<EventManager> {
        &self.event_manager
    }

    /// 获取存储管理器
    ///
    /// 注意：此方法用于访问本地存储管理器。
    /// 外部调用者可以使用此方法进行高级存储操作。
    pub fn storage(&self) -> &Arc<StorageManager> {
        &self.storage
    }

    /// 通知发送状态：Enqueued（内部方法）
    async fn notify_send_status_enqueued(&self, task: &crate::storage::queue::send_task::SendTask) {
        use crate::events::{SDKEvent, SendStatusState};
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.event_manager
            .emit(SDKEvent::SendStatusUpdate {
                channel_id: task.message_data.channel_id,
                id: task.id as u64,
                state: SendStatusState::Enqueued,
                attempts: 0,
                error: None,
                timestamp,
            })
            .await;
    }

    /// 获取网络监控（仅供内部使用）
    ///
    /// 注意：此方法仅供 SDK 内部使用，不对外暴露。
    /// 外部调用者应使用 SDK 提供的公开网络状态查询方法。
    #[allow(dead_code)]
    pub(crate) fn network(&self) -> &Arc<NetworkMonitor> {
        &self.network
    }

    /// 订阅网络状态变化（平台网络：Online/Offline/Connecting/Limited）
    ///
    /// 返回的 Receiver 在 SDK 生命周期内有效；可用于 FFI/平台层监听网络变化并回调到 UI。
    pub fn subscribe_network_status(&self) -> tokio::sync::broadcast::Receiver<NetworkStatusEvent> {
        self.network.subscribe()
    }

    // ========== 连接状态相关方法 ==========

    /// 获取当前连接状态
    pub async fn get_connection_state(&self) -> crate::connection_state::ConnectionState {
        self.connection_state.get_state().await
    }

    /// 获取连接状态摘要（用于日志打印）
    pub async fn get_connection_summary(&self) -> String {
        self.connection_state.get_summary().await
    }

    /// 打印连接状态到日志
    pub async fn log_connection_state(&self) {
        self.connection_state.log_state().await
    }

    // ========== Phase 8: 同步相关 API ==========

    /// 手动同步单个频道
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `channel_type`: 频道类型（1=私聊，2=群聊）
    ///
    /// # 返回
    /// 同步状态
    pub async fn sync_channel(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<crate::sync::ChannelSyncState> {
        self.check_initialized().await?;

        let sync_engine = self.sync_engine.read().await;
        let sync_engine = sync_engine
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        sync_engine.sync_channel(channel_id, channel_type).await
    }

    /// 同步所有频道
    ///
    /// # 返回
    /// 所有频道的同步状态
    pub async fn sync_all_channels(&self) -> Result<Vec<crate::sync::ChannelSyncState>> {
        self.check_initialized().await?;

        // 获取所有会话
        let channels = self
            .storage
            .get_channels(&crate::storage::entities::ChannelQuery::default())
            .await?;

        let channels: Vec<(u64, u8)> = channels
            .iter()
            .map(|c| (c.channel_id, c.channel_type as u8))
            .collect();

        if channels.is_empty() {
            info!("没有需要同步的频道");
            return Ok(Vec::new());
        }

        let sync_engine = self.sync_engine.read().await;
        let sync_engine = sync_engine
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        info!("开始同步 {} 个频道", channels.len());
        sync_engine.batch_sync_channels(&channels).await
    }

    /// 获取频道的同步状态
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `channel_type`: 频道类型
    ///
    /// # 返回
    /// 频道的本地 pts 和服务器 pts
    pub async fn get_channel_sync_state(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<(u64, u64)> {
        self.check_initialized().await?;

        // 获取本地 pts
        let local_pts = self
            .pts_manager
            .get_local_pts(channel_id, channel_type)
            .await?;

        // 获取服务器 pts
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let request = privchat_protocol::rpc::sync::GetChannelPtsRequest {
            channel_id,
            channel_type,
        };

        let request_value = serde_json::to_value(&request)
            .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;

        let response_value = client
            .call_rpc("sync/get_channel_pts", request_value)
            .await?;

        let response: privchat_protocol::rpc::sync::GetChannelPtsResponse =
            serde_json::from_value(response_value)
                .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;

        drop(client_guard); // 释放锁

        // GetChannelPtsResponse 现在只包含 current_pts: u64
        // 成功/失败由协议层的 code 字段处理，这里直接使用 current_pts
        let server_pts = response.current_pts;

        Ok((local_pts, server_pts))
    }

    /// 检查频道是否需要同步
    ///
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `channel_type`: 频道类型
    ///
    /// # 返回
    /// 是否需要同步（true = 需要同步）
    pub async fn needs_sync(&self, channel_id: u64, channel_type: u8) -> Result<bool> {
        let (local_pts, server_pts) = self
            .get_channel_sync_state(channel_id, channel_type)
            .await?;
        Ok(local_pts < server_pts)
    }

    /// 启动受监督的同步
    ///
    /// 启动一个后台同步任务，并通过观察者回调报告同步状态。
    ///
    /// # 参数
    /// - `observer`: 同步状态观察者回调
    ///
    /// # 返回
    /// - `Ok(())`: 启动成功
    ///
    /// # 示例
    /// ```rust
    /// struct MySyncObserver;
    /// impl SyncObserver for MySyncObserver {
    ///     fn on_state(&self, status: SyncStatus) {
    ///         println!("同步状态: {:?}", status.phase);
    ///     }
    /// }
    ///
    /// sdk.start_supervised_sync(Arc::new(MySyncObserver)).await?;
    /// ```
    pub async fn start_supervised_sync(
        &self,
        observer: Arc<dyn Fn(crate::events::SyncStatus) + Send + Sync>,
    ) -> Result<()> {
        self.check_initialized().await?;
        self.check_connected().await?;

        // 检查是否已经在运行
        {
            let running = self.supervised_sync_running.read().await;
            if *running {
                return Err(PrivchatSDKError::InvalidInput(
                    "受监督的同步已经在运行".to_string(),
                ));
            }
        }

        // 保存观察者
        *self.sync_observer.write().await = Some(observer.clone());

        // 标记为运行中
        *self.supervised_sync_running.write().await = true;

        // 启动后台同步任务
        let sync_engine = self.sync_engine.clone();
        let _event_manager = self.event_manager.clone();
        let observer_clone = observer.clone();
        let running_flag = self.supervised_sync_running.clone();

        tokio::spawn(async move {
            info!("🔄 受监督的同步已启动");

            // 通知观察者：开始同步
            observer_clone(crate::events::SyncStatus {
                phase: crate::events::SyncPhase::Running,
                message: Some("开始同步".to_string()),
            });

            loop {
                // 检查是否应该停止
                {
                    let running = running_flag.read().await;
                    if !*running {
                        info!("🛑 受监督的同步已停止");
                        break;
                    }
                }

                // 获取所有会话
                let sync_engine_guard = sync_engine.read().await;
                if let Some(_engine) = sync_engine_guard.as_ref() {
                    // 获取所有会话（简化实现，实际应该从存储层获取）
                    // 这里暂时跳过，等待后续完善

                    // 通知观察者：同步中
                    observer_clone(crate::events::SyncStatus {
                        phase: crate::events::SyncPhase::Running,
                        message: Some("同步中...".to_string()),
                    });

                    // 等待一段时间后再次检查
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                } else {
                    // 同步引擎未初始化，通知错误
                    observer_clone(crate::events::SyncStatus {
                        phase: crate::events::SyncPhase::Error,
                        message: Some("同步引擎未初始化".to_string()),
                    });
                    break;
                }
            }

            // 通知观察者：同步停止
            observer_clone(crate::events::SyncStatus {
                phase: crate::events::SyncPhase::Idle,
                message: Some("同步已停止".to_string()),
            });
        });

        Ok(())
    }

    /// 停止受监督的同步
    ///
    /// # 返回
    /// - `Ok(())`: 停止成功
    pub async fn stop_supervised_sync(&self) -> Result<()> {
        self.check_initialized().await?;

        // 标记为停止
        *self.supervised_sync_running.write().await = false;

        // 清除观察者
        *self.sync_observer.write().await = None;

        info!("🛑 受监督的同步已停止");
        Ok(())
    }

    /// 设置视频处理钩子（缩略图/压缩由上层实现）
    ///
    /// 未设置时，视频消息的缩略图将使用 1x1 全透明 PNG 占位上传。
    ///
    /// # 参数
    /// - `hook`: `Some(hook)` 设置回调，`None` 清除
    pub async fn set_video_process_hook(
        &self,
        hook: Option<crate::storage::media_preprocess::VideoProcessHook>,
    ) {
        *self.video_process_hook.write().await = hook;
    }

    /// 设置发送队列启用状态
    ///
    /// 控制全局发送队列的启用/禁用状态。
    ///
    /// # 参数
    /// - `enabled`: true 表示启用，false 表示禁用
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn send_queue_set_enabled(&self, enabled: bool) -> Result<()> {
        self.check_initialized().await?;

        // 通过 SendConsumer 控制队列状态
        let consumer_guard = self.send_consumer.read().await;
        if let Some(consumer) = consumer_guard.as_ref() {
            let is_running = consumer.is_running().await;

            if enabled {
                // 如果启用且未运行，启动消费者
                if !is_running {
                    drop(consumer_guard);
                    self.start_send_consumer().await?;
                }
            } else {
                // 如果禁用且正在运行，停止消费者
                if is_running {
                    consumer.stop().await?;
                }
            }
        } else if enabled {
            // 如果消费者不存在但需要启用，启动它
            drop(consumer_guard);
            self.start_send_consumer().await?;
        }

        info!("✅ 发送队列状态已设置为: {}", enabled);
        Ok(())
    }

    /// 设置指定会话的发送队列启用状态
    ///
    /// 控制特定会话的发送队列启用/禁用状态。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `enabled`: true 表示启用，false 表示禁用
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    ///
    /// # 注意
    /// 当前实现中，所有会话共享同一个发送队列，此方法暂时与全局设置相同。
    pub async fn channel_send_queue_set_enabled(
        &self,
        channel_id: u64,
        enabled: bool,
    ) -> Result<()> {
        // 当前实现中，所有会话共享同一个发送队列
        // 未来可以实现会话级别的队列控制
        self.send_queue_set_enabled(enabled).await?;

        info!("✅ 会话 {} 的发送队列状态已设置为: {}", channel_id, enabled);
        Ok(())
    }

    /// 入队文本消息（带事务ID）
    ///
    /// 将消息加入发送队列，返回事务ID用于后续重试。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `body`: 消息内容
    /// - `txn_id`: 可选的事务ID（如果不提供，将自动生成）
    ///
    /// # 返回
    /// - `Ok(String)`: 事务ID（可用于重试）
    ///
    /// # 示例
    /// ```rust
    /// // 入队消息
    /// let local_message_id_str = sdk.enqueue_text(channel_id, "消息内容".to_string(), None).await?;
    ///
    /// // 使用 message.id 重试
    /// let message_id: i64 = local_message_id_str.parse()?;
    /// sdk.retry_message(message_id).await?;
    /// ```
    pub async fn enqueue_text(
        &self,
        channel_id: u64,
        body: String,
        txn_id: Option<String>,
    ) -> Result<String> {
        // 使用 send_message 方法，它已经实现了入队逻辑
        let local_message_id = self.send_message(channel_id, &body).await?;

        // 如果提供了 txn_id，将其与 local_message_id 关联（可以通过 extra_data 存储）
        // 当前实现中，直接返回 local_message_id 作为事务ID
        Ok(txn_id.unwrap_or_else(|| local_message_id.to_string()))
    }

    /// 通过 message.id 重试消息
    ///
    /// 按本地消息主键（message.id）重试失败的消息。每次重试会生成新的 local_message_id
    /// 用于服务端去重，符合业界 IM 架构（message.id = 消息身份，txnId = 发送尝试身份）。
    ///
    /// # 参数
    /// - `message_id`: 本地消息主键（message.id，SQLite 主键）
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn retry_message(&self, message_id: i64) -> Result<()> {
        self.check_initialized().await?;

        let queue_manager = self.get_queue_manager().await?;

        let id_str = message_id.to_string();
        if let Some(mut task) = queue_manager.load_task(&id_str)? {
            // 每次重试生成新的 local_message_id，用于服务端去重（避免 seen(txnId) => drop）
            let new_local_message_id = self.snowflake.next_id().map_err(|e| {
                PrivchatSDKError::Other(format!("生成 local_message_id 失败: {:?}", e))
            })?;

            task.message_data.local_message_id = new_local_message_id;
            task.status = crate::storage::queue::send_task::TaskStatus::Pending;
            task.retry_count = 0;
            task.last_error = None;
            task.last_failure_reason = None;
            task.next_retry_at = None;
            queue_manager.persist_task(&task)?;
            queue_manager.enqueue_task(task.clone());
            self.notify_send_status_enqueued(&task).await;
            info!(
                "✅ 消息已重新入队: message.id={}, new_local_message_id={}",
                task.id, new_local_message_id
            );
        } else {
            return Err(PrivchatSDKError::InvalidInput(format!(
                "未找到 message.id={} 对应的消息",
                message_id
            )));
        }

        Ok(())
    }

    /// 设置会话收藏状态
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `favourite`: true 表示收藏，false 表示取消收藏
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn set_channel_favourite(&self, channel_id: u64, favourite: bool) -> Result<()> {
        self.check_initialized().await?;

        // 获取频道类型（先尝试私聊，再尝试群聊）
        let channel = if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 1).await {
            Some(ch)
        } else if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 2).await {
            Some(ch)
        } else {
            None
        };

        let channel = channel.ok_or_else(|| {
            PrivchatSDKError::InvalidInput(format!("会话不存在: channel_id={}", channel_id))
        })?;

        let channel_type = channel.channel_type;
        let save = if favourite { 1 } else { 0 };

        // 更新 channel 的 save 字段
        self.storage
            .update_channel_save(channel_id, channel_type, save)
            .await?;

        // 触发会话列表更新
        self.emit_channel_list_update(channel_id, channel_type)
            .await;

        info!(
            "✅ 会话收藏状态已更新: channel_id={}, favourite={}",
            channel_id, favourite
        );
        Ok(())
    }

    /// 设置会话低优先级状态
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `low_priority`: true 表示低优先级，false 表示正常优先级
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn set_channel_low_priority(
        &self,
        channel_id: u64,
        low_priority: bool,
    ) -> Result<()> {
        self.check_initialized().await?;

        // 获取会话（先尝试私聊，再尝试群聊）
        let channel = if let Ok(Some(conv)) =
            self.storage.get_channel_by_channel(channel_id, 1).await
        {
            Some(conv)
        } else if let Ok(Some(conv)) = self.storage.get_channel_by_channel(channel_id, 2).await {
            Some(conv)
        } else {
            None
        };

        let channel = channel.ok_or_else(|| {
            PrivchatSDKError::InvalidInput(format!("会话不存在: channel_id={}", channel_id))
        })?;

        let channel_type = channel.channel_type as u8;

        // 解析现有的 extra JSON
        let mut extra: serde_json::Value =
            serde_json::from_str(&channel.extra).unwrap_or_else(|_| serde_json::json!({}));

        // 更新 low_priority 字段
        extra["low_priority"] = serde_json::Value::Bool(low_priority);

        // 保存更新后的 extra
        let extra_str = serde_json::to_string(&extra)
            .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;

        self.storage
            .update_channel_extra(channel_id, channel_type, extra_str)
            .await?;

        // 触发会话列表更新
        self.emit_channel_list_update(channel_id, channel.channel_type)
            .await;

        info!(
            "✅ 会话优先级状态已更新: channel_id={}, low_priority={}",
            channel_id, low_priority
        );
        Ok(())
    }

    /// 获取会话标签（收藏和低优先级状态）
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    ///
    /// # 返回
    /// - `Ok(ChannelTags)`: 会话标签
    pub async fn channel_tags(&self, channel_id: u64) -> Result<crate::events::ChannelTags> {
        self.check_initialized().await?;

        // 获取频道信息（用于 favourite）- 先尝试私聊，再尝试群聊
        let channel = if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 1).await {
            Some(ch)
        } else if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 2).await {
            Some(ch)
        } else {
            None
        };

        let favourite = channel.as_ref().map(|c| c.save == 1).unwrap_or(false);

        // 获取会话信息（用于 low_priority）- 先尝试私聊，再尝试群聊
        let channel = if let Ok(Some(conv)) =
            self.storage.get_channel_by_channel(channel_id, 1).await
        {
            Some(conv)
        } else if let Ok(Some(conv)) = self.storage.get_channel_by_channel(channel_id, 2).await {
            Some(conv)
        } else {
            None
        };

        let low_priority = channel
            .as_ref()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c.extra).ok())
            .and_then(|extra| extra.get("low_priority")?.as_bool())
            .unwrap_or(false);

        Ok(crate::events::ChannelTags {
            favourite,
            low_priority,
        })
    }

    /// 获取会话通知模式
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    ///
    /// # 返回
    /// - `Ok(NotificationMode)`: 通知模式
    pub async fn channel_notification_mode(
        &self,
        channel_id: u64,
    ) -> Result<crate::events::NotificationMode> {
        self.check_initialized().await?;

        // 获取频道信息（先尝试私聊，再尝试群聊）
        let channel = if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 1).await {
            Some(ch)
        } else if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 2).await {
            Some(ch)
        } else {
            None
        };

        let channel = channel.ok_or_else(|| {
            PrivchatSDKError::InvalidInput(format!("会话不存在: channel_id={}", channel_id))
        })?;

        // mute 字段：0 = 所有通知, 1 = 仅 @ 提及, 2 = 无通知
        let mode = match channel.mute {
            0 => crate::events::NotificationMode::All,
            1 => crate::events::NotificationMode::Mentions,
            2 => crate::events::NotificationMode::None,
            _ => crate::events::NotificationMode::All, // 默认值
        };

        Ok(mode)
    }

    /// 设置会话通知模式
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `mode`: 通知模式
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub async fn set_channel_notification_mode(
        &self,
        channel_id: u64,
        mode: crate::events::NotificationMode,
    ) -> Result<()> {
        self.check_initialized().await?;

        // 获取频道信息（先尝试私聊，再尝试群聊）
        let channel = if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 1).await {
            Some(ch)
        } else if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 2).await {
            Some(ch)
        } else {
            None
        };

        let channel = channel.ok_or_else(|| {
            PrivchatSDKError::InvalidInput(format!("会话不存在: channel_id={}", channel_id))
        })?;

        let channel_type = channel.channel_type;

        // 将 NotificationMode 转换为 mute 值
        let mute = match mode {
            crate::events::NotificationMode::All => 0,
            crate::events::NotificationMode::Mentions => 1,
            crate::events::NotificationMode::None => 2,
        };

        // 更新 channel 的 mute 字段
        self.storage
            .update_channel_mute(channel_id, channel_type, mute)
            .await?;

        // 触发会话列表更新
        self.emit_channel_list_update(channel_id, channel_type)
            .await;

        info!(
            "✅ 会话通知模式已更新: channel_id={}, mode={:?}",
            channel_id, mode
        );
        Ok(())
    }

    /// 获取我的设备列表
    ///
    /// 获取当前用户的所有设备列表，包括当前设备和其他已登录设备。
    ///
    /// # 返回
    /// - `Ok(Vec<DeviceSummary>)`: 设备列表
    ///
    /// # 示例
    /// ```rust
    /// let devices = sdk.list_my_devices().await?;
    /// for device in devices {
    ///     println!("设备: {}, 名称: {}, 当前设备: {}",
    ///              device.device_id, device.device_name, device.is_current);
    /// }
    /// ```
    pub async fn list_my_devices(&self) -> Result<Vec<crate::events::DeviceSummary>> {
        self.check_initialized().await?;
        self.check_connected().await?;

        // 获取当前用户ID
        let user_id = self.user_id().await.ok_or(PrivchatSDKError::NotConnected)?;

        // 获取当前设备ID
        let connection_state = self.connection_state.get_state().await;
        let device_id = connection_state.user.as_ref().map(|u| u.device_id.clone());

        // 构造 RPC 请求
        use crate::rpc_client::{DeviceListRequest, RpcClientExt};
        let request = DeviceListRequest { user_id, device_id };

        // 调用 RPC
        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response = client.device_list(request).await?;

        drop(client_guard);

        // 将服务器返回的 DeviceListItem 转换为 SDK 的 DeviceSummary
        // 注意：时间字段已经是 UNIX 时间戳（毫秒），直接使用
        let devices: Vec<crate::events::DeviceSummary> = response
            .devices
            .into_iter()
            .map(|item| crate::events::DeviceSummary {
                device_id: item.device_id,
                device_name: item.device_name,
                device_model: Some(item.device_model),
                app_id: Some(item.app_id),
                device_type: Some(item.device_type),
                last_active_at: Some(item.last_active_at),
                created_at: Some(item.created_at),
                ip_address: Some(item.ip_address),
                is_current: item.is_current,
            })
            .collect();

        info!("✅ 获取设备列表成功: 共 {} 个设备", devices.len());

        Ok(devices)
    }

    /// 更新设备推送状态
    ///
    /// 当客户端切换到后台或前台时调用此方法，通知服务器设备的推送状态。
    ///
    /// # 参数
    /// - `device_id`: 设备ID
    /// - `apns_armed`: 是否需要推送（true: 需要推送, false: 不需要推送）
    /// - `push_token`: 可选的推送令牌（如果提供则更新）
    ///
    /// # 返回
    /// - `Ok(DevicePushUpdateResponse)`: 更新后的设备推送状态
    ///
    /// # 示例
    /// ```rust
    /// // 切换到后台时
    /// let response = sdk.update_device_push_state("device_123", true, None).await?;
    /// println!("推送状态已更新: apns_armed={}, user_push_enabled={}",
    ///          response.apns_armed, response.user_push_enabled);
    ///
    /// // 切换到前台时
    /// let response = sdk.update_device_push_state("device_123", false, None).await?;
    /// ```
    pub async fn update_device_push_state(
        &self,
        device_id: &str,
        apns_armed: bool,
        push_token: Option<String>,
    ) -> Result<privchat_protocol::rpc::device::DevicePushUpdateResponse> {
        self.check_initialized().await?;
        self.check_connected().await?;

        use crate::rpc_client::RpcClientExt;
        use privchat_protocol::rpc::device::DevicePushUpdateRequest;

        let request = DevicePushUpdateRequest {
            device_id: device_id.to_string(),
            apns_armed,
            push_token,
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response = client.device_push_update(request).await?;

        drop(client_guard);

        info!(
            "✅ 设备推送状态已更新: device_id={}, apns_armed={}, user_push_enabled={}",
            device_id, response.apns_armed, response.user_push_enabled
        );

        Ok(response)
    }

    /// 获取设备推送状态
    ///
    /// 查询当前用户所有设备或指定设备的推送状态。
    ///
    /// # 参数
    /// - `device_id`: 可选的设备ID（不提供则返回所有设备）
    ///
    /// # 返回
    /// - `Ok(DevicePushStatusResponse)`: 设备推送状态列表
    ///
    /// # 示例
    /// ```rust
    /// // 查询所有设备
    /// let response = sdk.get_device_push_status(None).await?;
    /// for device in response.devices {
    ///     println!("设备: {}, apns_armed: {}, connected: {}",
    ///              device.device_id, device.apns_armed, device.connected);
    /// }
    ///
    /// // 查询指定设备
    /// let response = sdk.get_device_push_status(Some("device_123")).await?;
    /// ```
    pub async fn get_device_push_status(
        &self,
        device_id: Option<&str>,
    ) -> Result<privchat_protocol::rpc::device::DevicePushStatusResponse> {
        self.check_initialized().await?;
        self.check_connected().await?;

        use crate::rpc_client::RpcClientExt;
        use privchat_protocol::rpc::device::DevicePushStatusRequest;

        let request = DevicePushStatusRequest {
            device_id: device_id.map(|s| s.to_string()),
        };

        let mut client_guard = self.client.write().await;
        let client = client_guard
            .as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;

        let response = client.device_push_status(request).await?;

        drop(client_guard);

        info!(
            "✅ 设备推送状态查询成功: 设备数量={}, user_push_enabled={}",
            response.devices.len(),
            response.user_push_enabled
        );

        Ok(response)
    }

    /// App 切换到后台
    ///
    /// 这是 SDK 的一级生命周期事件，会触发：
    /// - 更新设备推送状态（push_armed = true）
    /// - 降级实时连接策略（降低心跳频率）
    /// - 停止非必要操作（消息预拉、大文件上传等）
    /// - Flush 关键状态（presence、ack、本地缓存）
    ///
    /// # 示例
    /// ```rust
    /// // iOS: 在 AppDelegate.applicationDidEnterBackground 中调用
    /// sdk.on_app_background().await?;
    ///
    /// // Android: 在 Activity.onPause 中调用
    /// sdk.on_app_background().await?;
    /// ```
    pub async fn on_app_background(&self) -> Result<()> {
        self.check_initialized().await?;

        info!("🔄 App 切换到后台，触发生命周期事件");

        // 通知所有注册的生命周期 Hook
        let manager = self.lifecycle_manager.read().await;
        manager.notify_background().await?;

        info!("✅ App 后台切换完成");
        Ok(())
    }

    /// App 切换到前台
    ///
    /// 这是 SDK 的一级生命周期事件，会触发：
    /// - 更新设备推送状态（push_armed = false）
    /// - 恢复实时连接策略（正常心跳频率）
    /// - 同步离线消息
    /// - 恢复后台暂停的任务
    ///
    /// # 示例
    /// ```rust
    /// // iOS: 在 AppDelegate.applicationWillEnterForeground 中调用
    /// sdk.on_app_foreground().await?;
    ///
    /// // Android: 在 Activity.onResume 中调用
    /// sdk.on_app_foreground().await?;
    /// ```
    pub async fn on_app_foreground(&self) -> Result<()> {
        self.check_initialized().await?;

        info!("🔄 App 切换到前台，触发生命周期事件");

        // 通知所有注册的生命周期 Hook
        let manager = self.lifecycle_manager.read().await;
        manager.notify_foreground().await?;

        info!("✅ App 前台切换完成");
        Ok(())
    }

    /// 注册生命周期回调 Hook
    ///
    /// 各模块可以通过此方法注册生命周期回调，在 App 前后台切换时自动调用。
    ///
    /// # 示例
    /// ```rust
    /// struct MyModule;
    ///
    /// #[async_trait]
    /// impl LifecycleHook for MyModule {
    ///     async fn on_background(&self) -> Result<()> {
    ///         // 处理后台切换
    ///         Ok(())
    ///     }
    ///
    ///     async fn on_foreground(&self) -> Result<()> {
    ///         // 处理前台切换
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let module = Arc::new(MyModule);
    /// sdk.register_lifecycle_hook(module).await?;
    /// ```
    pub async fn register_lifecycle_hook(
        &self,
        hook: Arc<dyn crate::lifecycle::LifecycleHook>,
    ) -> Result<()> {
        self.check_initialized().await?;

        let mut manager = self.lifecycle_manager.write().await;
        manager.register_hook(hook);

        Ok(())
    }

    /// 获取私聊会话的对等用户ID
    ///
    /// 对于私聊会话（channel_type = 1），返回另一个用户的 user_id。
    /// 对于群聊或其他类型的会话，返回 `None`。
    ///
    /// # 参数
    /// - `channel_id`: 会话ID
    ///
    /// # 返回
    /// - `Ok(Option<u64>)`: 对等用户ID（如果是私聊），否则返回 `None`
    ///
    /// # 示例
    /// ```rust
    /// if let Some(peer_user_id) = sdk.dm_peer_user_id(channel_id).await? {
    ///     println!("私聊对等用户ID: {}", peer_user_id);
    /// } else {
    ///     println!("这不是私聊会话");
    /// }
    /// ```
    pub async fn dm_peer_user_id(&self, channel_id: u64) -> Result<Option<u64>> {
        self.check_initialized().await?;

        // 获取频道信息（先尝试私聊）
        let channel = if let Ok(Some(ch)) = self.storage.get_channel(channel_id, 1).await {
            Some(ch)
        } else {
            // 如果不是私聊，返回 None
            None
        };

        let channel = match channel {
            Some(ch) => ch,
            None => return Ok(None),
        };

        // 对于私聊，username 字段存储的是对方的 user_id（字符串形式）
        let peer_user_id = channel.username.parse::<u64>().ok().map(|id| {
            info!(
                "✅ 获取私聊对等用户ID成功: channel_id={}, peer_user_id={}",
                channel_id, id
            );
            id
        });

        Ok(peer_user_id)
    }

    /// 获取平台信息
    fn get_platform_info() -> String {
        #[cfg(target_os = "macos")]
        {
            format!("macOS {}", std::env::consts::ARCH)
        }
        #[cfg(target_os = "windows")]
        {
            format!("Windows {}", std::env::consts::ARCH)
        }
        #[cfg(target_os = "linux")]
        {
            format!("Linux {}", std::env::consts::ARCH)
        }
        #[cfg(target_os = "ios")]
        {
            format!("iOS {}", std::env::consts::ARCH)
        }
        #[cfg(target_os = "android")]
        {
            format!("Android {}", std::env::consts::ARCH)
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux",
            target_os = "ios",
            target_os = "android"
        )))]
        {
            format!("Unknown {}", std::env::consts::ARCH)
        }
    }
}

// ========== 同步接口（用于 FFI） ==========

impl PrivchatSDK {
    /// 同步连接
    pub fn connect_blocking(&self) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async { self.connect().await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步发送消息（阻塞版本，用于FFI）
    pub fn send_message_blocking(&self, channel_id: u64, content: &str) -> Result<u64> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async { self.send_message(channel_id, content).await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步标记已读（按 message.id，合约 v1）
    pub fn mark_as_read_blocking(&self, session_id: &str, message_id: u64) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            let channel_id = session_id
                .parse::<u64>()
                .map_err(|_| PrivchatSDKError::InvalidInput("无效的 session_id".to_string()))?;
            rt.block_on(async { self.mark_as_read(channel_id, message_id).await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步撤回消息
    pub fn recall_message_blocking(&self, message_id: u64) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async { self.recall_message(message_id).await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步编辑消息
    pub fn edit_message_blocking(&self, message_id: u64, new_content: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async { self.edit_message(message_id, new_content).await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步开始输入状态
    pub fn start_typing_blocking(&self, session_id: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            let channel_id = session_id
                .parse::<u64>()
                .map_err(|_| PrivchatSDKError::InvalidInput("无效的 session_id".to_string()))?;
            rt.block_on(async { self.start_typing(channel_id).await })
        } else {
            Err(PrivchatSDKError::Runtime("同步运行时未初始化".to_string()))
        }
    }

    /// 同步添加表情反馈
    pub fn add_reaction_blocking(&self, message_id: u64, emoji: &str) -> Result<()> {
        if let Some(rt) = &self.sync_runtime {
            rt.block_on(async { self.add_reaction(message_id, emoji).await })
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

        let config = PrivchatConfig::builder()
            .data_dir(temp_dir.path())
            .add_server("tcp://test.example.com:9001")
            .build();

        let sdk = PrivchatSDK::initialize(config).await.unwrap();

        assert!(sdk.is_initialized().await);
        assert_eq!(sdk.user_id().await, None); // 未连接时没有用户ID
        assert!(!sdk.is_connected().await);

        sdk.shutdown().await.unwrap();
        assert!(!sdk.is_initialized().await);
    }

    #[tokio::test]
    async fn test_sdk_lifecycle() {
        let temp_dir = TempDir::new().unwrap();

        let config = PrivchatConfig::builder()
            .data_dir(temp_dir.path())
            .add_server("tcp://test.example.com:9001")
            .build();

        let sdk = PrivchatSDK::initialize(config).await.unwrap();

        // 测试连接
        // sdk.connect("test_user", "test_token").await.unwrap();
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
        let config = PrivchatConfig::builder()
            .data_dir("/tmp/test")
            .servers(vec![
                "quic://127.0.0.1:9001",
                "tcp://127.0.0.1:9001",
                "wss://127.0.0.1:9080/path",
            ])
            .connection_timeout(60)
            .debug_mode(true)
            .build();

        assert_eq!(config.data_dir, PathBuf::from("/tmp/test"));
        assert_eq!(config.server_config.endpoints.len(), 3);

        // 检查QUIC配置（第一个端点）
        let quic_endpoint = &config.server_config.endpoints[0];
        assert_eq!(quic_endpoint.protocol, TransportProtocol::Quic);
        assert_eq!(quic_endpoint.host, "127.0.0.1");
        assert_eq!(quic_endpoint.port, 9001);
        assert!(quic_endpoint.use_tls); // QUIC强制TLS

        // 检查TCP配置（第二个端点）
        let tcp_endpoint = &config.server_config.endpoints[1];
        assert_eq!(tcp_endpoint.protocol, TransportProtocol::Tcp);
        assert_eq!(tcp_endpoint.host, "127.0.0.1");
        assert_eq!(tcp_endpoint.port, 9001);
        assert!(!tcp_endpoint.use_tls); // TCP通常不使用TLS

        // 检查WebSocket配置（第三个端点）
        let ws_endpoint = &config.server_config.endpoints[2];
        assert_eq!(ws_endpoint.protocol, TransportProtocol::WebSocket);
        assert_eq!(ws_endpoint.host, "127.0.0.1");
        assert_eq!(ws_endpoint.port, 9080);
        assert_eq!(ws_endpoint.path, Some("/path".to_string()));
        assert!(ws_endpoint.use_tls); // wss://使用TLS

        assert_eq!(config.connection_timeout, 60);
        assert!(config.debug_mode);
    }

    /// 集成测试：断网 → 重连 → 再 sync_entities(Friend)，校验数量与幂等。
    /// 需能连真实或 mock 服务端时运行。
    ///
    /// 环境变量（缺一则跳过）：
    /// - `PRIVCHAT_TEST_SERVER_URL`: 服务端地址，如 `tcp://127.0.0.1:9001`
    /// - `PRIVCHAT_TEST_USER_ID`: 测试用户 ID
    /// - `PRIVCHAT_TEST_TOKEN`: 测试用户 JWT token
    ///
    /// 运行：`cargo test -p privchat-sdk --lib -- --ignored test_sync_friends_after_reconnect --nocapture`
    #[tokio::test]
    #[ignore]
    async fn test_sync_friends_after_reconnect() {
        let server_url = match std::env::var("PRIVCHAT_TEST_SERVER_URL") {
            Ok(u) if !u.is_empty() => u,
            _ => {
                eprintln!("skip: PRIVCHAT_TEST_SERVER_URL not set");
                return;
            }
        };
        let user_id_str = match std::env::var("PRIVCHAT_TEST_USER_ID") {
            Ok(u) if !u.is_empty() => u,
            _ => {
                eprintln!("skip: PRIVCHAT_TEST_USER_ID not set");
                return;
            }
        };
        let user_id: u64 = match user_id_str.parse() {
            Ok(id) => id,
            Err(_) => {
                eprintln!("skip: PRIVCHAT_TEST_USER_ID invalid u64");
                return;
            }
        };
        let token = match std::env::var("PRIVCHAT_TEST_TOKEN") {
            Ok(t) if !t.is_empty() => t,
            _ => {
                eprintln!("skip: PRIVCHAT_TEST_TOKEN not set");
                return;
            }
        };

        let temp_dir = TempDir::new().unwrap();
        let config = PrivchatConfig::builder()
            .data_dir(temp_dir.path())
            .add_server(&server_url)
            .connection_timeout(15)
            .build();

        let sdk = PrivchatSDK::initialize(config).await.expect("sdk init");
        sdk.connect().await.expect("first connect");

        use privchat_protocol::protocol::{DeviceInfo, DeviceType};
        let device_info = DeviceInfo {
            device_id: format!("test-device-{}", user_id),
            device_type: DeviceType::Android,
            app_id: "privchat.test".to_string(),
            push_token: None,
            push_channel: None,
            device_name: "integration-test".to_string(),
            device_model: None,
            os_version: None,
            app_version: None,
            manufacturer: None,
            device_fingerprint: None,
        };
        sdk.authenticate(user_id, &token, device_info.clone())
            .await
            .expect("first authenticate");

        let count1 = sdk
            .sync_entities(crate::sync::EntityType::Friend, None)
            .await
            .expect("first sync_entities friend");
        let friends_count1 = sdk
            .get_friends_count()
            .await
            .expect("get_friends_count after first sync");
        assert_eq!(
            count1 as u32, friends_count1,
            "first sync: sync_entities friend count should match get_friends_count"
        );

        sdk.disconnect().await.expect("disconnect");
        assert!(!sdk.is_connected().await);

        sdk.connect().await.expect("second connect");
        sdk.authenticate(user_id, &token, device_info)
            .await
            .expect("second authenticate");

        let count2 = sdk
            .sync_entities(crate::sync::EntityType::Friend, None)
            .await
            .expect("second sync_entities friend");
        let friends_count2 = sdk
            .get_friends_count()
            .await
            .expect("get_friends_count after second sync");
        assert_eq!(
            count2 as u32, friends_count2,
            "second sync: sync_entities friend count should match get_friends_count"
        );

        assert_eq!(
            count1, count2,
            "idempotency: friend count after reconnect+sync should equal first sync (no loss)"
        );
        assert_eq!(
            friends_count1, friends_count2,
            "idempotency: get_friends_count should be same after second sync (no duplicate)"
        );

        sdk.shutdown().await.expect("shutdown");
    }
}
