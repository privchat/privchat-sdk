//! Configuration types and builder for FFI

use std::sync::Arc;
use crate::error::PrivchatError;
use crate::helpers::unwrap_or_clone_arc;
use privchat_sdk::sdk::{RetryConfig, QueueConfig, EventConfig};

/// Transport protocol type
#[derive(Debug, Clone, uniffi::Enum)]
pub enum TransportProtocol {
    Quic,
    Tcp,
    WebSocket,
}

impl From<TransportProtocol> for privchat_sdk::TransportProtocol {
    fn from(protocol: TransportProtocol) -> Self {
        match protocol {
            TransportProtocol::Quic => privchat_sdk::TransportProtocol::Quic,
            TransportProtocol::Tcp => privchat_sdk::TransportProtocol::Tcp,
            TransportProtocol::WebSocket => privchat_sdk::TransportProtocol::WebSocket,
        }
    }
}

/// Server endpoint configuration
#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerEndpoint {
    pub protocol: TransportProtocol,
    pub host: String,
    pub port: u16,
    pub path: Option<String>,
    pub use_tls: bool,
}

/// 解析服务器 URL 为 ServerEndpoint
/// 支持格式：quic://host:port, wss://host:port/path, ws://host:port, tcp://host:port
pub fn parse_server_url(url: String) -> Result<ServerEndpoint, PrivchatError> {
    let (protocol, use_tls, prefix) = if url.starts_with("quic://") {
        (TransportProtocol::Quic, true, "quic://")
    } else if url.starts_with("wss://") {
        (TransportProtocol::WebSocket, true, "wss://")
    } else if url.starts_with("ws://") {
        (TransportProtocol::WebSocket, false, "ws://")
    } else if url.starts_with("tcp://") {
        (TransportProtocol::Tcp, false, "tcp://")
    } else {
        return Err(PrivchatError::invalid_parameter("url", "unsupported scheme, use quic://, wss://, ws://, or tcp://"));
    };

    let remainder = url
        .strip_prefix(prefix)
        .ok_or_else(|| PrivchatError::invalid_parameter("url", "invalid url format"))?;

    let (host_port, path) = if let Some(slash_pos) = remainder.find('/') {
        (
            &remainder[..slash_pos],
            Some(remainder[slash_pos..].to_string()),
        )
    } else {
        (remainder, None)
    };

    let (host, port) = parse_host_port(host_port)
        .ok_or_else(|| PrivchatError::invalid_parameter("url", "invalid host:port"))?;

    Ok(ServerEndpoint {
        protocol,
        host,
        port,
        path,
        use_tls,
    })
}

fn parse_host_port(host_port: &str) -> Option<(String, u16)> {
    if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port_str = &host_port[colon_pos + 1..];
        if let Ok(port) = port_str.parse::<u16>() {
            return Some((host.to_string(), port));
        }
    }
    // 无端口时使用默认端口
    Some((host_port.to_string(), 8080))
}

impl From<ServerEndpoint> for privchat_sdk::ServerEndpoint {
    fn from(endpoint: ServerEndpoint) -> Self {
        privchat_sdk::ServerEndpoint {
            protocol: endpoint.protocol.into(),
            host: endpoint.host,
            port: endpoint.port,
            path: endpoint.path,
            use_tls: endpoint.use_tls,
        }
    }
}

/// Server configuration
#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerConfig {
    pub endpoints: Vec<ServerEndpoint>,
}

impl From<ServerConfig> for privchat_sdk::ServerConfig {
    fn from(config: ServerConfig) -> Self {
        privchat_sdk::ServerConfig {
            endpoints: config.endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

/// HTTP 客户端配置（FFI 层）
#[derive(Debug, Clone, uniffi::Record)]
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

/// Main SDK configuration
#[derive(Debug, Clone, uniffi::Record)]
pub struct PrivchatConfig {
    pub data_dir: String,
    /// Assets directory path (SQL migration files) - OPTIONAL
    /// If not set, SDK uses built-in embedded migrations (recommended).
    /// Set only when you need custom/override migrations (e.g., development).
    pub assets_dir: Option<String>,
    pub server_config: ServerConfig,
    pub connection_timeout: u64,
    pub heartbeat_interval: u64,
    pub debug_mode: bool,
    /// 文件服务 API 基础 URL（可选）
    /// 
    /// 例如：https://files.example.com/api/app
    /// 如果不提供，将从 RPC 响应的 upload_url 中提取
    pub file_api_base_url: Option<String>,
    /// HTTP 客户端配置（可选）
    pub http_client_config: Option<HttpClientConfig>,
}

impl TryFrom<PrivchatConfig> for privchat_sdk::PrivchatConfig {
    type Error = PrivchatError;
    
    fn try_from(config: PrivchatConfig) -> Result<Self, Self::Error> {
        Ok(privchat_sdk::PrivchatConfig {
            data_dir: std::path::PathBuf::from(config.data_dir),
            assets_dir: config.assets_dir.map(std::path::PathBuf::from),
            server_config: config.server_config.into(),
            connection_timeout: config.connection_timeout,
            heartbeat_interval: config.heartbeat_interval,
            retry_config: RetryConfig {
                max_retries: 3,
                base_delay_ms: 1000,
                max_delay_ms: 30000,
                backoff_factor: 2.0,
            },
            queue_config: QueueConfig {
                send_queue_size: 1000,
                receive_queue_size: 1000,
                batch_size: 100,
                worker_threads: 4,
            },
            event_config: EventConfig {
                buffer_size: 1000,
                filters: Vec::new(), // No filters for Phase 0
            },
            timezone_offset_seconds: None,
            debug_mode: config.debug_mode,
            file_api_base_url: config.file_api_base_url,
            http_client_config: privchat_sdk::HttpClientConfig {
                connect_timeout_secs: config.http_client_config.as_ref().and_then(|c| c.connect_timeout_secs),
                request_timeout_secs: config.http_client_config.as_ref().and_then(|c| c.request_timeout_secs),
                enable_retry: config.http_client_config.as_ref().map(|c| c.enable_retry).unwrap_or(true),
                max_retries: config.http_client_config.as_ref().map(|c| c.max_retries).unwrap_or(3),
            },
            image_send_max_edge: None,
        })
    }
}

/// Configuration builder with fluent API
#[derive(Clone, uniffi::Object)]
pub struct PrivchatConfigBuilder {
    data_dir: Option<String>,
    assets_dir: Option<String>,
    endpoints: Vec<ServerEndpoint>,
    connection_timeout: u64,
    heartbeat_interval: u64,
    debug_mode: bool,
    file_api_base_url: Option<String>,
    http_client_config: Option<HttpClientConfig>,
}

#[privchat_ffi_macros::export]
impl PrivchatConfigBuilder {
    /// Create a new configuration builder
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            data_dir: None,
            assets_dir: None,
            endpoints: Vec::new(),
            connection_timeout: 30,
            heartbeat_interval: 30,
            debug_mode: false,
            file_api_base_url: None,
            http_client_config: None,
        }
    }
    
    /// Set the data directory path
    pub fn data_dir(self: Arc<Self>, path: String) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.data_dir = Some(path);
        Arc::new(builder)
    }
    
    /// Set the assets directory path (SQL migration files) - optional
    /// If not set, SDK uses built-in embedded migrations (no external SQL files needed)
    pub fn assets_dir(self: Arc<Self>, path: String) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.assets_dir = Some(path);
        Arc::new(builder)
    }
    
    /// Add a server endpoint
    pub fn server_endpoint(self: Arc<Self>, endpoint: ServerEndpoint) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.endpoints.push(endpoint);
        Arc::new(builder)
    }

    /// Add server by URL string (quic://, wss://, ws://, tcp://)
    pub fn server_url(self: Arc<Self>, url: String) -> Result<Arc<Self>, PrivchatError> {
        let endpoint = parse_server_url(url)?;
        Ok(self.server_endpoint(endpoint))
    }
    
    /// Set connection timeout in seconds
    pub fn connection_timeout(self: Arc<Self>, seconds: u64) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.connection_timeout = seconds;
        Arc::new(builder)
    }
    
    /// Set heartbeat interval in seconds
    pub fn heartbeat_interval(self: Arc<Self>, seconds: u64) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.heartbeat_interval = seconds;
        Arc::new(builder)
    }
    
    /// Enable or disable debug mode
    pub fn debug_mode(self: Arc<Self>, enabled: bool) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.debug_mode = enabled;
        Arc::new(builder)
    }
    
    /// Set file API base URL
    pub fn file_api_base_url(self: Arc<Self>, url: String) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.file_api_base_url = Some(url);
        Arc::new(builder)
    }
    
    /// Set HTTP client configuration
    pub fn http_client_config(self: Arc<Self>, config: HttpClientConfig) -> Arc<Self> {
        let mut builder = unwrap_or_clone_arc(self);
        builder.http_client_config = Some(config);
        Arc::new(builder)
    }
    
    /// Build the configuration
    pub fn build(self: Arc<Self>) -> Result<PrivchatConfig, PrivchatError> {
        let builder = unwrap_or_clone_arc(self);
        
        let data_dir = builder.data_dir
            .ok_or_else(|| PrivchatError::invalid_parameter("data_dir", "data_dir is required"))?;
        
        if builder.endpoints.is_empty() {
            return Err(PrivchatError::invalid_parameter("endpoints", "at least one server endpoint is required"));
        }
        
        Ok(PrivchatConfig {
            data_dir,
            assets_dir: builder.assets_dir,
            server_config: ServerConfig {
                endpoints: builder.endpoints,
            },
            connection_timeout: builder.connection_timeout,
            heartbeat_interval: builder.heartbeat_interval,
            debug_mode: builder.debug_mode,
            file_api_base_url: builder.file_api_base_url,
            http_client_config: builder.http_client_config,
        })
    }
}

impl Default for PrivchatConfigBuilder {
    fn default() -> Self {
        Self {
            data_dir: None,
            assets_dir: None,
            endpoints: Vec::new(),
            connection_timeout: 30,
            heartbeat_interval: 30,
            debug_mode: false,
            file_api_base_url: None,
            http_client_config: None,
        }
    }
}
