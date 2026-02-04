use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::collections::HashMap;
use tokio::sync::{RwLock, mpsc};
use rusqlite::{Connection, params};
use uuid::Uuid;
use bytes::Bytes;
use serde::{Serialize, Deserialize};
use serde::de::DeserializeOwned;
use msgtrans::{
    transport::client::TransportClientBuilder,
    protocol::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig},
    transport::TransportOptions,
    event::ClientEvent,
};
use crate::error::{PrivchatSDKError, Result};
use crate::message_type::message_type_str_to_u32;
use crate::storage::deduplication::DeduplicationManager;
use privchat_protocol::{
    encode_message, decode_message, MessageType,
    AuthorizationRequest, AuthorizationResponse, DisconnectRequest,
    SubscribeRequest, SubscribeResponse,
    SendMessageRequest, SendMessageResponse, PushMessageRequest, PushMessageResponse,
    PushBatchRequest, PushBatchResponse, PublishRequest, PublishResponse,
    RpcRequest, RpcResponse,
    AuthType, ClientInfo, DeviceInfo, DisconnectReason,
    MessageSetting, ErrorCode,
};

// ========== RPC 调用相关类型定义 ==========

/// RPC 调用结果类型（使用 SDK 错误类型）
pub type RpcResult<T> = Result<T>;

/// RPC 请求消息
#[derive(Serialize, Deserialize)]
pub struct RPCMessageRequest {
    pub route: String,
    pub body: serde_json::Value,
}

/// RPC 响应消息
#[derive(Serialize, Deserialize)]
pub struct RPCMessageResponse {
    pub code: i32,
    pub message: String,
    pub data: Option<serde_json::Value>,
}

impl RPCMessageResponse {
    /// 检查响应是否成功
    /// 
    /// # 返回
    /// - `true`: code == 0，表示成功
    /// - `false`: code != 0，表示错误
    #[inline]
    pub fn is_ok(&self) -> bool {
        self.code == 0
    }

    /// 检查响应是否失败
    /// 
    /// # 返回
    /// - `true`: code != 0，表示错误
    /// - `false`: code == 0，表示成功
    #[inline]
    pub fn is_err(&self) -> bool {
        self.code != 0
    }
}

/// 用户会话信息
#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub session_id: Option<String>,
    pub login_time: chrono::DateTime<chrono::Utc>,
    pub server_key: Option<String>,
    pub node_id: Option<String>,
    pub server_info: Option<privchat_protocol::protocol::ServerInfo>,
}

/// 传输协议类型
#[derive(Debug, Clone, PartialEq)]
pub enum TransportProtocol {
    Quic,
    Tcp,
    WebSocket,
}

/// 服务器端点配置
#[derive(Debug, Clone)]
pub struct ServerEndpoint {
    pub protocol: TransportProtocol,
    pub host: String,
    pub port: u16,
    pub path: Option<String>,
    pub use_tls: bool,
}

/// Privchat 客户端 - 连接与会话管理层
/// 
/// 职责范围：
/// - 网络连接与认证
/// - 用户目录和数据库初始化
/// - 会话状态管理
/// - 基础传输层协议处理
/// 
/// 更新说明：
/// - 使用 msgtrans::transport::client::TransportClient 和 request_with_options
/// - 为每种消息类型设置正确的 biz_type
/// - 支持事件驱动架构
pub struct PrivchatClient {
    /// SDK 工作根目录
    work_dir: PathBuf,
    /// 当前用户目录（连接后创建）
    user_dir: Option<PathBuf>,
    /// 当前用户ID（服务器返回）
    user_id: Option<u64>,
    /// 加密数据库连接（延迟初始化）
    db: Option<Arc<Mutex<Connection>>>,
    /// 用户会话信息
    session: Option<UserSession>,
    /// 传输层客户端 - 使用 msgtrans::transport::client::TransportClient
    transport: Option<msgtrans::transport::client::TransportClient>,
    /// 连接状态
    connected: Arc<RwLock<bool>>,
    /// 服务器端点配置
    server_endpoints: Vec<ServerEndpoint>,
    /// 连接超时时间
    connection_timeout: Duration,
    /// 消息接收回调通道（用于通知外部消息接收）
    message_receiver_tx: Option<mpsc::UnboundedSender<PushMessageRequest>>,
    /// ✨ 本地 pts（消息同步指针）
    local_pts: Arc<RwLock<u64>>,
    /// ✨ 最后在线时间
    last_online_time: Arc<RwLock<i64>>,
    /// ✨ 消息去重管理器（基于 message_id）
    message_dedup_manager: Arc<DeduplicationManager>,
    /// RPC 限流器（可选，由 SDK 注入）
    rpc_rate_limiter: Option<Arc<crate::rate_limiter::RpcRateLimiter>>,
    /// Snowflake ID 生成器
    snowflake: Arc<snowflake_me::Snowflake>,
    /// transport 断开时通知 SDK 的 sender（桥：Client 只发，SDK 收后执行被动断开）
    transport_disconnect_tx: Option<mpsc::UnboundedSender<()>>,
}

impl PrivchatClient {
    /// 获取默认数据目录 ~/.privchat/
    pub fn default_data_dir() -> PathBuf {
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

    /// 创建新的客户端实例
    pub async fn new<P: AsRef<Path>>(
        work_dir: P, 
        server_endpoints: Vec<ServerEndpoint>,
        connection_timeout: Duration,
    ) -> Result<Self> {
        let work_dir = work_dir.as_ref().to_path_buf();
        
        // 确保工作目录存在
        tokio::fs::create_dir_all(&work_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建工作目录失败: {}", e)))?;
        
        // 初始化 Snowflake ID 生成器（避免 IP 检测失败）
        // 注意：使用 StdRng 而不是 thread_rng()，因为 thread_rng() 不是 Send 的，不能在 async 函数中使用
        use rand::{Rng, SeedableRng};
        use rand::rngs::StdRng;
        let mut rng = StdRng::from_entropy();
        let machine_id: u16 = rng.gen_range(0..32);
        let data_center_id: u16 = rng.gen_range(0..32);
        
        let snowflake = snowflake_me::Snowflake::builder()
            .machine_id(&|| Ok(machine_id))
            .data_center_id(&|| Ok(data_center_id))
            .finalize()
            .map_err(|e| PrivchatSDKError::Other(format!("初始化 Snowflake 失败: {:?}", e)))?;
        
        Ok(Self {
            work_dir,
            user_dir: None,
            user_id: None,
            db: None,
            session: None,
            transport: None,
            connected: Arc::new(RwLock::new(false)),
            server_endpoints,
            connection_timeout,
            message_receiver_tx: None,
            local_pts: Arc::new(RwLock::new(0)),
            last_online_time: Arc::new(RwLock::new(0)),
            message_dedup_manager: Arc::new(DeduplicationManager::new()),
            rpc_rate_limiter: None,
            snowflake: Arc::new(snowflake),
            transport_disconnect_tx: None,
        })
    }

    /// 使用默认数据目录创建新的客户端实例
    pub async fn with_default_dir(
        server_endpoints: Vec<ServerEndpoint>,
        connection_timeout: Duration,
    ) -> Result<Self> {
        Self::new(Self::default_data_dir(), server_endpoints, connection_timeout).await
    }
    
    /// 连接到服务器（只建立底层网络连接，不发送 ConnectRequest）
    /// 
    /// 支持多协议自动降级：QUIC → TCP → WebSocket
    pub async fn connect(&mut self) -> Result<()> {
        tracing::info!("正在建立网络连接...");
        
        // 按优先级尝试连接到不同的服务器端点
        let mut last_error = None;
        let endpoints = self.server_endpoints.clone();
        
        for endpoint in &endpoints {
            tracing::info!("尝试连接到 {:?} 服务器: {}:{}", endpoint.protocol, endpoint.host, endpoint.port);
            
            match self.try_connect_to_endpoint(endpoint).await {
                Ok(transport) => {
                    self.transport = Some(transport);
                    *self.connected.write().await = true;
                    
                    tracing::info!("✅ 网络连接建立成功: {:?}", endpoint.protocol);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!("连接到 {:?} 服务器失败: {}", endpoint.protocol, e);
                    last_error = Some(e);
                    continue;
                }
            }
        }
        
        Err(last_error.unwrap_or_else(|| PrivchatSDKError::Transport("无可用的服务器端点".to_string())))
    }
    
    /// 发送 ConnectRequest 进行认证
    /// 
    /// 在调用此方法前必须先调用 connect() 建立网络连接
    pub async fn authenticate(&mut self, user_id: u64, token: &str, device_info: DeviceInfo) -> Result<UserSession> {
        tracing::info!("正在认证用户: user_id={}", user_id);
        
        // 检查连接
        if self.transport.is_none() {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        // 在发认证请求前先启动消息接收事件循环，否则服务端认证后立即下发的欢迎消息（PushMessageRequest）
        // 会因 broadcast 尚无订阅者而转发失败，导致消息无法落库、get_messages() 查不到
        self.start_message_event_loop().await;
        
        // 发送 ConnectRequest（服务端可能在此后立即下发欢迎消息，此时事件循环已在订阅）
        let session = self.authenticate_with_transport(user_id, token, device_info).await?;
        
        self.session = Some(session.clone());
        
        // 初始化用户环境
        self.initialize_user_environment(&session).await?;
        
        tracing::info!("✅ 认证成功，用户ID: {}", session.user_id);
        Ok(session)
    }
    
    /// 解析主机名或 IP 地址到 SocketAddr（支持 DNS 解析）
    async fn resolve_to_socket_addr(host: &str, port: u16) -> Result<std::net::SocketAddr> {
        // 首先尝试直接解析为 SocketAddr（适用于 IP 地址）
        let addr_str = format!("{}:{}", host, port);
        if let Ok(addr) = addr_str.parse::<std::net::SocketAddr>() {
            return Ok(addr);
        }
        
        // 如果直接解析失败，尝试 DNS 解析（适用于主机名）
        let mut addrs = tokio::net::lookup_host((host, port)).await
            .map_err(|e| PrivchatSDKError::Transport(format!("DNS 解析失败: {}", e)))?;
        
        addrs.next()
            .ok_or_else(|| PrivchatSDKError::Transport(format!("无法解析主机名: {}", host)))
    }
    
    /// 尝试连接到指定端点
    async fn try_connect_to_endpoint(&self, endpoint: &ServerEndpoint) -> Result<msgtrans::transport::client::TransportClient> {
        let mut client = match endpoint.protocol {
            TransportProtocol::Quic => {
                let addr = Self::resolve_to_socket_addr(&endpoint.host, endpoint.port).await
                    .map_err(|e| PrivchatSDKError::Transport(format!("解析服务器地址失败 {}:{}: {}", endpoint.host, endpoint.port, e)))?;
                let config = QuicClientConfig::new(&addr.to_string())
                    .map_err(|e| PrivchatSDKError::Transport(format!("创建 QUIC 配置失败: {}", e)))?
                    .with_connect_timeout(self.connection_timeout);
                
                TransportClientBuilder::new()
                    .with_protocol(config)
                    .connect_timeout(self.connection_timeout)
                    .build()
                    .await
                    .map_err(|e| PrivchatSDKError::Transport(format!("构建 QUIC 客户端失败: {}", e)))?
            }
            TransportProtocol::Tcp => {
                let addr = Self::resolve_to_socket_addr(&endpoint.host, endpoint.port).await
                    .map_err(|e| PrivchatSDKError::Transport(format!("解析服务器地址失败 {}:{}: {}", endpoint.host, endpoint.port, e)))?;
                let config = TcpClientConfig::default()
                    .with_target_address(addr)
                    .with_connect_timeout(self.connection_timeout)
                    .with_nodelay(true);
                
                TransportClientBuilder::new()
                    .with_protocol(config)
                    .connect_timeout(self.connection_timeout)
                    .build()
                    .await
                    .map_err(|e| PrivchatSDKError::Transport(format!("构建 TCP 客户端失败: {}", e)))?
            }
            TransportProtocol::WebSocket => {
                let url = if endpoint.use_tls {
                    format!("wss://{}:{}{}", endpoint.host, endpoint.port, endpoint.path.as_deref().unwrap_or("/"))
                } else {
                    format!("ws://{}:{}{}", endpoint.host, endpoint.port, endpoint.path.as_deref().unwrap_or("/"))
                };
                
                let config = WebSocketClientConfig::new(&url)
                    .map_err(|e| PrivchatSDKError::Transport(format!("创建 WebSocket 配置失败: {}", e)))?
                    .with_connect_timeout(self.connection_timeout)
                    .with_verify_tls(endpoint.use_tls);
                
                TransportClientBuilder::new()
                    .with_protocol(config)
                    .connect_timeout(self.connection_timeout)
                    .build()
                    .await
                    .map_err(|e| PrivchatSDKError::Transport(format!("构建 WebSocket 客户端失败: {}", e)))?
            }
        };
        
        // 连接到服务器
        client.connect().await
            .map_err(|e| PrivchatSDKError::Transport(format!("连接失败: {}", e)))?;
        
        Ok(client)
    }
    
    /// 使用指定的传输层客户端执行认证流程
    async fn authenticate_with_transport(
        &mut self,
        user_id: u64, 
        token: &str,
        device_info: DeviceInfo,
    ) -> Result<UserSession> {
        // 1. 构建 AuthorizationRequest（ConnectRequest）
        let local_pts = *self.local_pts.read().await;
        let last_online_time = *self.last_online_time.read().await;
        
        let connect_request = AuthorizationRequest {
            auth_type: AuthType::JWT,
            auth_token: token.to_string(),
            client_info: ClientInfo {
                client_type: "privchat-sdk".to_string(),
                version: "1.0.0".to_string(),
                os: std::env::consts::OS.to_string(),
                os_version: std::env::consts::OS.to_string(),
                device_model: None,
                app_package: Some("com.privchat.sdk".to_string()),
            },
            device_info,
            protocol_version: "1.0".to_string(),
            properties: {
                let mut props = HashMap::new();
                props.insert("user_id".to_string(), user_id.to_string());
                props.insert("client_timestamp".to_string(), chrono::Utc::now().timestamp_millis().to_string());
                // ✨ 通过 properties 传递 pts 同步信息
                props.insert("local_pts".to_string(), local_pts.to_string());
                props.insert("last_online_time".to_string(), last_online_time.to_string());
                props
            },
        };
        
        let request_data = encode_message(&connect_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码连接请求失败: {}", e)))?;
        
        // 2. 发送请求 - 使用 request_with_options 并设置正确的 biz_type
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::AuthorizationRequest as u8)
            .with_timeout(self.connection_timeout);
        
        tracing::info!("📤 发送认证请求: user_id={}, request_size={} bytes", user_id, request_data.len());
        
        let transport = self.transport.as_mut()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        
        let response_data = transport.request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("发送认证请求失败: {}", e)))?;
        tracing::info!("📥 收到认证响应: response_size={} bytes", response_data.len());
        
        // 3. 解析响应
        let connect_response: AuthorizationResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码连接响应失败: {}", e)))?;
        
        if !connect_response.success {
            let error_code = connect_response.error_code.unwrap_or_else(|| "UNKNOWN".to_string());
            let error_message = connect_response.error_message.unwrap_or_else(|| "认证失败".to_string());
            return Err(PrivchatSDKError::Auth(format!("认证失败，错误码: {}, 消息: {}", error_code, error_message)));
        }
        
        // ✨ 3.5. 处理 pts 同步信息
        // 注意：当前协议版本不支持 server_pts 字段，暂时跳过 pts 同步
        // TODO: 等待协议更新支持 server_pts 字段后再启用
        tracing::info!(
            "📊 pts 同步: local_pts={} (server_pts 暂不支持)",
            local_pts
        );
        
        // 更新最后在线时间
        *self.last_online_time.write().await = chrono::Utc::now().timestamp();
        
        // 注意：未读消息数由客户端基于本地数据库计算，不依赖服务器返回
        
        // 4. 创建用户会话
        let user_id = connect_response.user_id.ok_or_else(|| {
            PrivchatSDKError::Auth("服务器未返回用户ID".to_string())
        })?;
        let session = UserSession {
            user_id,
            token: token.to_string(),
            device_id: connect_request.device_info.device_id.clone(),
            session_id: connect_response.session_id.clone(),
            login_time: chrono::Utc::now(),
            server_key: connect_response.session_id.clone(),
            node_id: connect_response.connection_id.clone(),
            server_info: connect_response.server_info.clone(),
        };
        
        // 打印服务器信息
        if let Some(server_info) = &session.server_info {
            tracing::info!(
                "服务器信息: 版本={}, 名称={}, 功能={:?}",
                server_info.version,
                server_info.name,
                server_info.features
            );
        }
        
        tracing::info!("认证成功，用户ID: {}, 会话ID: {:?}", session.user_id, session.session_id);
        
        Ok(session)
    }
    
    /// 断开连接
    pub async fn disconnect(&mut self, reason: &str) -> Result<()> {
        if !self.is_connected().await {
            return Ok(());
        }
        
        tracing::info!("正在断开连接，原因: {}", reason);
        
        // 发送断开连接请求
        if let Some(transport) = &mut self.transport {
            let disconnect_request = DisconnectRequest {
                reason: DisconnectReason::UserInitiated,
                message: Some(reason.to_string()),
            };
            
            let request_data = encode_message(&disconnect_request)
                .map_err(|e| PrivchatSDKError::Serialization(format!("编码断开请求失败: {}", e)))?;
            
            let transport_options = TransportOptions::new()
                .with_biz_type(MessageType::DisconnectRequest as u8)
                .with_timeout(Duration::from_secs(5));
            
            let _ = transport.request_with_options(Bytes::from(request_data), transport_options).await;
        }
        
        // 断开传输层连接
        if let Some(transport) = self.transport.take() {
            let _ = transport.disconnect().await;
        }
        
        // 清理状态
        self.session = None;
        self.user_id = None;
        self.user_dir = None;
        self.db = None;
        *self.connected.write().await = false;
        
        tracing::info!("断开连接完成");
        Ok(())
    }
    
    /// 检查连接状态
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }
    
    /// 获取当前用户ID
    pub fn user_id(&self) -> Option<u64> {
        self.user_id
    }
    
    /// 获取当前会话
    pub fn session(&self) -> Option<&UserSession> {
        self.session.as_ref()
    }
    
    /// 获取用户目录
    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }
    
    /// 获取数据库连接
    pub fn database(&self) -> Option<Arc<Mutex<Connection>>> {
        self.db.as_ref().cloned()
    }
    
    /// 心跳检测
    pub async fn ping(&mut self) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let ping_request = privchat_protocol::protocol::PingRequest {
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        
        let request_data = encode_message(&ping_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码心跳请求失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::PingRequest as u8)
            .with_timeout(Duration::from_secs(10));
        
        let response_data = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("心跳请求失败: {}", e)))?;
        
        let _ping_response: privchat_protocol::protocol::PongResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码心跳响应失败: {}", e)))?;
        
        tracing::debug!("心跳检测成功");
        Ok(())
    }
    
    /// 订阅频道
    pub async fn subscribe_channel(&mut self, channel_id: u64) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        // channel_id 已经是 u64 类型
        let channel_id_u64 = channel_id;
        
        let local_message_id = self.snowflake.next_id()
            .map_err(|e| PrivchatSDKError::Other(format!("生成 local_message_id 失败: {:?}", e)))?;
        
        let subscribe_request = SubscribeRequest {
            setting: 1,
            local_message_id,
            channel_id: channel_id_u64,
            channel_type: 1,
            action: 1,
            param: "".to_string(),
        };
        
        let request_data = encode_message(&subscribe_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码订阅请求失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::SubscribeRequest as u8)
            .with_timeout(Duration::from_secs(10));
        
        let response_data = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("订阅请求失败: {}", e)))?;
        
        let subscribe_response: SubscribeResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码订阅响应失败: {}", e)))?;
        
        if subscribe_response.reason_code != 0 {
            return Err(PrivchatSDKError::Transport(format!("订阅频道失败，错误码: {}", subscribe_response.reason_code)));
        }
        
        tracing::info!("成功订阅频道: {}", channel_id);
        Ok(())
    }
    
    /// 发送消息到指定频道
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `content`: 消息内容（纯文本，不是 JSON）
    /// - `message_type`: 消息类型字符串 ("text", "image", "video", "red_package" 等)
    pub async fn send_message(&mut self, channel_id: u64, content: &str, message_type: &str) -> Result<(u64, u64)> {
        self.send_message_with_metadata(channel_id, content, message_type, None).await
    }
    
    /// 发送消息到指定频道（带 metadata）
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `content`: 消息内容（纯文本，不是 JSON）
    /// - `message_type`: 消息类型字符串 ("text", "image", "video", "red_package" 等)
    /// - `metadata`: 可选的元数据 JSON 对象
    pub async fn send_message_with_metadata(
        &mut self,
        channel_id: u64,
        content: &str,
        message_type: &str,
        metadata: Option<serde_json::Value>,
    ) -> Result<(u64, u64)> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let local_message_id = self.snowflake.next_id()
            .map_err(|e| PrivchatSDKError::Other(format!("生成 local_message_id 失败: {:?}", e)))?;
        
        self.send_message_internal(channel_id, content, message_type, metadata, local_message_id).await
    }
    
    /// 发送消息（内部方法，接受指定的 local_message_id，用于队列系统）
    pub(crate) async fn send_message_internal(
        &mut self,
        channel_id: u64,
        content: &str,
        message_type: &str,
        metadata: Option<serde_json::Value>,
        local_message_id: u64,
    ) -> Result<(u64, u64)> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let from_uid = self.user_id.ok_or(PrivchatSDKError::NotConnected)?;
        
        // 消息类型由协议层 SendMessageRequest.message_type（u32）提供，payload 仅含 MessagePayloadEnvelope
        let metadata_value = metadata.as_ref().cloned().unwrap_or(serde_json::Value::Null);
        let mut payload_json = serde_json::json!({
            "content": content,
            "metadata": metadata_value.clone(),
        });
        
        // 如果 metadata 中包含新功能字段，提取到顶层
        if let Some(meta) = metadata.as_ref() {
            if let Some(obj) = meta.as_object() {
                if let Some(reply_id) = obj.get("reply_to_message_id") {
                    payload_json["reply_to_message_id"] = reply_id.clone();
                }
                if let Some(mentioned) = obj.get("mentioned_user_ids") {
                    payload_json["mentioned_user_ids"] = mentioned.clone();
                }
                if let Some(source) = obj.get("message_source") {
                    payload_json["message_source"] = source.clone();
                }
            }
        }
        
        let payload = serde_json::to_vec(&payload_json)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化 payload 失败: {}", e)))?;
        
        // channel_id 已经是 u64 类型
        let channel_id_u64 = channel_id;
        
        let send_message_request = SendMessageRequest {
            setting: MessageSetting {
                need_receipt: true,
                signal: 0,
            },
            client_seq: 1,
            local_message_id,
            stream_no: format!("stream_{}", Uuid::new_v4()),
            channel_id: channel_id_u64,
            channel_type: 1, // 1: 个人聊天, 2: 群聊
            message_type: message_type_str_to_u32(message_type),
            expire: 3600,
            from_uid,
            topic: "chat".to_string(),
            payload,
        };
        
        let request_data = encode_message(&send_message_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码发送请求失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::SendMessageRequest as u8)
            .with_timeout(Duration::from_secs(10));
        
        let response_data = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("发送消息请求失败: {}", e)))?;
        
        let send_message_response: SendMessageResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码发送响应失败: {}", e)))?;
        
        if send_message_response.reason_code != 0 {
            return Err(PrivchatSDKError::Transport(format!("发送消息失败，错误码: {}", send_message_response.reason_code)));
        }
        
        // 返回 (local_message_id, server_message_id)
        tracing::info!("✅ 成功发送消息: local_message_id={}, server_message_id={}, channel_id={}", 
            local_message_id, send_message_response.server_message_id, channel_id);
        Ok((local_message_id, send_message_response.server_message_id))
    }
    
    /// 处理接收到的消息并发送确认
    pub async fn handle_received_message(&mut self, push_message_request: PushMessageRequest) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::info!(
            "收到消息: {} 来自: {} 频道: {} 内容: {}",
            push_message_request.local_message_id,
            push_message_request.from_uid,
            push_message_request.channel_id,
            String::from_utf8_lossy(&push_message_request.payload)
        );
        
        // 发送接收确认
        let push_message_response = PushMessageResponse {
            succeed: true,
            message: Some("消息接收成功".to_string()),
        };
        
        let response_data = encode_message(&push_message_response)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码接收响应失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::PushMessageResponse as u8)
            .with_timeout(Duration::from_secs(5));
        
        let _ = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(response_data), transport_options).await;
        
        Ok(())
    }
    
    /// 处理批量接收消息
    pub async fn handle_batch_messages(&mut self, batch_request: PushBatchRequest) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::info!("收到批量消息，数量: {}", batch_request.message_count());
        
        // 处理每条消息
        for message in &batch_request.messages {
            tracing::info!(
                "批量消息: {} 来自: {} 频道: {}",
                message.local_message_id,
                message.from_uid,
                message.channel_id
            );
        }
        
        // 发送批量确认
        let batch_response = PushBatchResponse {
            succeed: true,
            message: Some(format!("成功处理 {} 条消息", batch_request.message_count())),
        };
        
        let response_data = encode_message(&batch_response)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码批量响应失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::PushBatchResponse as u8)
            .with_timeout(Duration::from_secs(5));
        
        let _ = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(response_data), transport_options).await;
        
        Ok(())
    }
    
    /// 处理推送消息并发送确认
    pub async fn handle_publish_message(&mut self, publish_request: PublishRequest) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::info!(
            "收到推送消息: 频道: {} 发布者: {:?} 内容: {}",
            publish_request.channel_id,
            publish_request.publisher,
            String::from_utf8_lossy(&publish_request.payload)
        );
        
        // 发送推送确认
        let publish_response = PublishResponse {
            succeed: true,
            message: Some("推送消息接收成功".to_string()),
        };
        
        let response_data = encode_message(&publish_response)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码推送响应失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::PublishResponse as u8)
            .with_timeout(Duration::from_secs(5));
        
        let _ = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(response_data), transport_options).await;
        
        Ok(())
    }
    
    /// 通用 RPC 调用方法
    /// 
    /// # 参数
    /// - `route`: RPC 路由，如 "message/revoke"
    /// - `params`: 请求参数
    /// 
    /// # 返回
    /// - 成功返回响应数据
    pub async fn call_rpc(&mut self, route: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::debug!("[SDK] 调用 RPC: route={}, params={}", route, params);
        
        // 创建 RPC 请求
        let rpc_request = RpcRequest {
            route: route.to_string(),
            body: params,
        };
        
        let request_data = encode_message(&rpc_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码 RPC 请求失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(Duration::from_secs(10));
        
        let response_bytes = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(request_data), transport_options)
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("RPC 请求失败: {}", e)))?;
        
        // 解码响应
        let rpc_response: RpcResponse = decode_message(&response_bytes)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码 RPC 响应失败: {}", e)))?;
        
        // 检查状态码 (0 表示成功)
        // 使用 is_err() 方法提供更清晰的语义
        if rpc_response.is_err() {
            // 将 RPC 响应中的错误码转换为协议层的 ErrorCode
            let _error_code = ErrorCode::from_code(rpc_response.code as u32)
                .unwrap_or(ErrorCode::SystemError);
            
            // 使用统一的 RPC 错误类型
            return Err(PrivchatSDKError::from_rpc_response(
                rpc_response.code as u32,
                rpc_response.message.clone(),
            ));
        }
        
        tracing::debug!("[SDK] RPC 调用成功: {}", rpc_response.message);
        
        Ok(rpc_response.data.unwrap_or(serde_json::Value::Null))
    }
    
    /// 撤回消息（已废弃，请使用 RpcClientExt trait 的 message_revoke 方法）
    /// 
    /// # 参数
    /// - `message_id`: 要撤回的消息ID（u64）
    /// - `channel_id`: 频道ID（u64）
    /// 
    /// # 返回
    // ========== RPC 调用方法 ==========
    
    /// 通用 RPC 调用方法
    /// 
    /// # 参数
    /// - `route`: RPC 路由路径，格式：system/module/action（如：account/user/get）
    /// - `body`: 请求参数的 JSON 值
    /// 
    /// # 返回值
    /// - `RpcResult<T>`: 自动反序列化为指定类型 T 的结果
    /// 
    /// # 示例
    /// ```rust,no_run
    /// use serde::Deserialize;
    /// use serde_json::json;
    /// 
    /// #[derive(Deserialize)]
    /// struct UserInfo {
    ///     id: String,
    ///     username: String,
    ///     avatar_url: Option<String>,
    /// }
    /// 
    /// let user: RpcResult<UserInfo> = client
    ///     .call("account/user/get", json!({ "id": "123" }))
    ///     .await;
    /// 
    /// match user {
    ///     Ok(user_info) => println!("用户名: {}", user_info.username),
    ///     Err(err) => eprintln!("调用失败: {}", err),
    /// }
    /// ```
    pub async fn call<T: DeserializeOwned>(
        &mut self,
        route: &str,
        body: serde_json::Value,
    ) -> RpcResult<T> {
        // 检查连接状态
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        // 构建 RPC 请求
        let request = RPCMessageRequest {
            route: route.to_string(),
            body,
        };
        
        tracing::debug!("🚀 发送 RPC 请求: route={}, body={}", route, request.body);
        
        // 序列化请求
        let request_data = serde_json::to_vec(&request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("请求序列化失败: {}", e)))?;
        
        // 设置传输选项 - 使用 RpcRequest 消息类型 (17)
        let transport_options = TransportOptions::new()
            .with_biz_type(17u8)  // RpcRequest = 17
            .with_timeout(self.connection_timeout);
        
        // 发送请求并等待响应
        let response_data = self.transport.as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?
            .request_with_options(Bytes::from(request_data), transport_options)
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("传输层错误: {}", e)))?;
        
        // 反序列化响应
        let rpc_response: RPCMessageResponse = serde_json::from_slice(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("响应反序列化失败: {}", e)))?;
        
        tracing::debug!("📥 收到 RPC 响应: route={}, code={}, message={}", 
                       route, rpc_response.code, rpc_response.message);
        
        // 检查响应状态码 (0 表示成功)
        // 使用 is_err() 方法提供更清晰的语义
        if rpc_response.is_err() {
            // 将 RPC 错误转换为 SDK 错误
            return Err(PrivchatSDKError::from_rpc_response(
                rpc_response.code as u32,
                rpc_response.message,
            ));
        }
        
        // 提取数据并反序列化为目标类型
        let data = rpc_response.data.ok_or_else(|| {
            PrivchatSDKError::InvalidData("成功响应中缺少数据字段".to_string())
        })?;
        
        serde_json::from_value(data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("数据反序列化失败: {}", e)))
    }
    
    /// 初始化用户环境（用户目录统一在 work_dir/users/{uid}/，与 StorageManager 一致，避免数据根目录出现裸 uid 目录）
    async fn initialize_user_environment(&mut self, session: &UserSession) -> Result<()> {
        let user_dir = self.work_dir.join("users").join(session.user_id.to_string());
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;
        
        // 创建数据库连接
        let db_path = user_dir.join("privchat.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;
        
        // 创建表结构
        Self::create_database_tables(&conn)?;
        
        // 存储连接
        self.db = Some(Arc::new(Mutex::new(conn)));
        self.user_dir = Some(user_dir);
        self.user_id = Some(session.user_id);
        
        tracing::info!("用户环境初始化完成，用户ID: {}", session.user_id);
        Ok(())
    }
    
    /// 生成设备ID
    #[allow(dead_code)]
    fn generate_device_id(&self) -> String {
        format!("privchat_device_{}", Uuid::new_v4())
    }
    
    /// 派生加密密钥
    pub fn derive_encryption_key(user_id: u64) -> String {
        format!("encryption_key_{}", user_id)
    }
    
    /// ✨ 获取本地 pts
    pub async fn get_local_pts(&self) -> u64 {
        *self.local_pts.read().await
    }
    
    /// ✨ 设置本地 pts（用于测试）
    pub async fn set_local_pts(&self, pts: u64) {
        *self.local_pts.write().await = pts;
    }
    
    /// 创建数据库表结构
    pub fn create_database_tables(conn: &Connection) -> Result<()> {
        // 创建基础表结构
        let create_tables_sql = r#"
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT UNIQUE NOT NULL,
                channel_id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                content TEXT NOT NULL,
                message_type INTEGER NOT NULL,
                timestamp INTEGER NOT NULL,
                is_read INTEGER DEFAULT 0,
                is_deleted INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            
            CREATE TABLE IF NOT EXISTS channels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT UNIQUE NOT NULL,
                title TEXT,
                channel_type INTEGER NOT NULL,
                participants TEXT NOT NULL,
                last_message_id TEXT,
                last_message_time INTEGER,
                unread_count INTEGER DEFAULT 0,
                is_archived INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            
            CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id);
            CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
            CREATE INDEX IF NOT EXISTS idx_channels_last_message_time ON channels(last_message_time);
        "#;
        
        conn.execute_batch(create_tables_sql)
            .map_err(|e| PrivchatSDKError::Database(format!("创建数据库表失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 设置消息接收回调通道
    pub fn set_message_receiver(&mut self, tx: mpsc::UnboundedSender<PushMessageRequest>) {
        self.message_receiver_tx = Some(tx);
    }

    /// 设置 transport 断开时通知 SDK 的 sender（桥：收到 Disconnected 时 send(())，SDK 侧 recv 后执行被动断开）
    pub fn set_transport_disconnect_sender(&mut self, tx: mpsc::UnboundedSender<()>) {
        self.transport_disconnect_tx = Some(tx);
    }
    
    /// 启动消息接收事件循环
    async fn start_message_event_loop(&self) {
        let transport = match &self.transport {
            Some(t) => t,
            None => {
                tracing::warn!("[SDK] Transport 未初始化，无法启动事件循环");
                return;
            }
        };
        
        let connected = Arc::clone(&self.connected);
        let transport_disconnect_tx = self.transport_disconnect_tx.clone();
        let message_tx = self.message_receiver_tx.clone();
        let message_dedup_manager = Arc::clone(&self.message_dedup_manager);
        let user_id = self.user_id.clone(); // 获取当前用户ID
        let db = self.db.clone(); // 获取数据库连接
        let mut event_receiver = transport.subscribe_events();
        
        tokio::spawn(async move {
            tracing::debug!("[SDK] 消息接收事件循环已启动");
            
            while let Ok(event) = event_receiver.recv().await {
                if !*connected.read().await {
                    tracing::debug!("[SDK] 客户端已断开，停止事件循环");
                    break;
                }
                
                match event {
                    ClientEvent::MessageReceived(context) => {
                        tracing::debug!("[SDK] 收到消息事件: message_id={}, biz_type={}", 
                                       context.message_id, context.biz_type);
                        
                        // 检查是否是 PushMessageRequest 消息 (biz_type = 7)
                        if context.biz_type == MessageType::PushMessageRequest as u8 {
                            // 先解码，立即处理结果，避免跨 await 边界的问题
                            let push_message_request = match decode_message::<PushMessageRequest>(&context.data) {
                                Ok(req) => req,
                                Err(e) => {
                                    tracing::warn!("[SDK] 解码 PushMessageRequest 失败: {}", e);
                                    continue;
                                }
                            };
                            
                            tracing::info!(
                                "[SDK] 解码 PushMessageRequest 成功: {} 来自: {} 频道: {}",
                                push_message_request.local_message_id,
                                push_message_request.from_uid,
                                push_message_request.channel_id
                            );
                            
                            // ✅ 新增：检查是否是已读回执通知
                            // 通过 payload 中的消息类型来判断是否是系统通知
                            if let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&push_message_request.payload) {
                                if let Some("notification") = payload.get("message_type").and_then(|v| v.as_str()) {
                                    if let Some(metadata) = payload.get("metadata") {
                                        if let Some("read_receipt") = metadata.get("notification_type").and_then(|v| v.as_str()) {
                                            // 这是已读回执通知
                                            let message_id = metadata.get("message_id")
                                                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())))
                                                .unwrap_or(0);
                                            let reader_id = metadata.get("reader_id")
                                                .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())))
                                                .unwrap_or(0);
                                            let read_at = metadata.get("read_at")
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                                .unwrap_or_default();
                                            
                                            tracing::info!(
                                                "📨 [SDK] 收到已读回执通知: 消息 {} 已被 {} 读取 ({})",
                                                message_id, reader_id, read_at
                                            );
                                            
                                            // TODO: 触发 ReadReceiptReceived 事件
                                            // 当前简化实现：只打印日志
                                            // 未来可以通过事件系统通知应用层
                                            
                                            // 已读通知不需要转发给应用层，直接返回
                                            continue;
                                        }
                                    }
                                }
                            }
                            
                            // ✨ 检查消息去重（基于 message_id）
                            let message_id = push_message_request.server_message_id;
                            if message_dedup_manager.is_duplicate(message_id) {
                                tracing::debug!("[SDK] 检测到重复消息，已忽略: message_id={}", message_id);
                                continue;
                            }
                            
                            // 标记消息为已处理
                            message_dedup_manager.mark_as_processed(message_id);
                            
                            // ✨ 处理@提及（客户端记录）
                            if let (Some(current_user_id), Some(ref db_conn)) = (user_id.as_ref().copied(), db.as_ref()) {
                                let db_clone = Arc::clone(db_conn);
                                if let Err(e) = Self::process_mentions(
                                    &push_message_request,
                                    current_user_id,
                                    db_clone,
                                ).await {
                                    tracing::warn!("[SDK] 处理@提及失败: {}", e);
                                }
                            }
                            
                            // 通过通道发送消息给外部处理
                            if let Some(ref tx) = message_tx {
                                if let Err(e) = tx.send(push_message_request) {
                                    tracing::warn!("[SDK] 发送消息到回调通道失败: {}", e);
                                }
                            } else {
                                tracing::debug!("[SDK] 未设置消息接收回调，消息将被忽略");
                            }
                        }
                    }
                    ClientEvent::Connected { .. } => {
                        tracing::debug!("[SDK] Transport 连接已建立");
                    }
                    ClientEvent::Disconnected { .. } => {
                        tracing::info!("[SDK] Transport 连接已断开，上报 SDK 状态机");
                        *connected.write().await = false;
                        if let Some(ref tx) = transport_disconnect_tx {
                            let _ = tx.send(());
                        }
                        break;
                    }
                    ClientEvent::Error { error } => {
                        tracing::warn!("[SDK] Transport 错误: {}", error);
                    }
                    _ => {
                        tracing::trace!("[SDK] 收到其他事件: {:?}", event);
                    }
                }
            }
            
            tracing::debug!("[SDK] 消息接收事件循环已结束");
        });
    }
    
    /// ✨ 处理@提及（客户端记录）
    /// 
    /// 解析 PushMessageRequest 的 payload，检查是否包含 `mentioned_user_ids`
    /// 如果当前用户被@了，记录到数据库
    async fn process_mentions(
        push_message_request: &PushMessageRequest,
        current_user_id: u64,
        db: Arc<Mutex<Connection>>,
    ) -> Result<()> {
        // 解析 payload（JSON 格式）
        let payload_json: serde_json::Value = match serde_json::from_slice(&push_message_request.payload) {
            Ok(json) => json,
            Err(_) => {
                // payload 不是 JSON 格式，没有@提及信息
                return Ok(());
            }
        };
        
        // 检查是否包含 `mentioned_user_ids` 字段
        let mentioned_user_ids: Vec<u64> = match payload_json.get("mentioned_user_ids") {
            Some(ids) => {
                match ids.as_array() {
                    Some(arr) => {
                        arr.iter()
                            .filter_map(|v| v.as_u64())
                            .collect()
                    }
                    None => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        
        // 检查是否@全体成员（从 content 中解析，或者从 payload 中获取）
        let is_mention_all = payload_json.get("is_mention_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        
        // 如果当前用户被@了，记录到数据库
        let is_mentioned = mentioned_user_ids.contains(&current_user_id) || is_mention_all;
        
        if is_mentioned {
            let conn = db.lock().unwrap();
            
            // 初始化表（如果不存在）
            let mention_dao = crate::storage::dao::MentionDao::new(&conn);
            if let Err(e) = mention_dao.initialize_table() {
                tracing::warn!("[SDK] 初始化@提及表失败: {}", e);
                return Err(crate::error::PrivchatSDKError::Database(format!("初始化@提及表失败: {}", e)));
            }
            
            // 记录@提及到 mentions 表（用于快速统计）
            let channel_type = push_message_request.channel_type as i32;
            if let Err(e) = mention_dao.record_mention(
                push_message_request.server_message_id,
                push_message_request.channel_id,
                channel_type,
                current_user_id,
                push_message_request.from_uid,
                is_mention_all,
            ) {
                tracing::warn!("[SDK] 记录@提及失败: {}", e);
                return Err(crate::error::PrivchatSDKError::Database(format!("记录@提及失败: {}", e)));
            }
            
            // ✨ 更新消息表的 extra 字段，标记 has_mention: true
            // 这样在显示消息时可以直接知道是否有提及，不需要再查询 mentions 表
            if let Err(e) = Self::update_message_mention_flag(&conn, push_message_request.server_message_id, true) {
                tracing::warn!("[SDK] 更新消息提及标记失败: {}（消息可能还未存储）", e);
                // 不返回错误，因为消息可能还未存储到数据库
                // 等消息存储时，可以通过查询 mentions 表来标记
            }
            
            tracing::info!(
                "[SDK] 记录@提及: 消息 {} 在频道 {} 中@了用户 {}",
                push_message_request.server_message_id,
                push_message_request.channel_id,
                current_user_id
            );
        }
        
        Ok(())
    }
    
    /// ✨ 更新消息的提及标记（在 extra 字段中）
    /// 
    /// 如果消息已存在，更新 extra.has_mention 字段
    /// 如果消息不存在，不报错（消息可能还未存储）
    fn update_message_mention_flag(
        conn: &Connection,
        message_id: u64,
        has_mention: bool,
    ) -> Result<()> {
        // 查询当前消息的 extra 字段
        let current_extra: Option<String> = conn.query_row(
            "SELECT extra FROM message WHERE message_id = ?1",
            params![message_id],
            |row| Ok(row.get::<_, String>(0)?),
        ).ok();
        
        let mut extra_json: serde_json::Value = if let Some(extra_str) = current_extra {
            serde_json::from_str(&extra_str)
                .unwrap_or_else(|_| serde_json::json!({}))
        } else {
            // 消息不存在，返回成功（消息可能还未存储）
            return Ok(());
        };
        
        // 更新 has_mention 字段
        if let Some(obj) = extra_json.as_object_mut() {
            obj.insert("has_mention".to_string(), serde_json::json!(has_mention));
        } else {
            // extra 不是对象，创建新对象
            extra_json = serde_json::json!({
                "has_mention": has_mention
            });
        }
        
        let extra_str = serde_json::to_string(&extra_json)
            .map_err(|e| crate::error::PrivchatSDKError::Database(format!("序列化 extra 失败: {}", e)))?;
        
        // 更新消息的 extra 字段
        conn.execute(
            "UPDATE message SET extra = ?1 WHERE message_id = ?2",
            params![extra_str, message_id],
        )?;
        
        Ok(())
    }
    
    /// 设置 RPC 限流器（由 SDK 注入）
    pub fn set_rpc_rate_limiter(&mut self, limiter: Arc<crate::rate_limiter::RpcRateLimiter>) {
        self.rpc_rate_limiter = Some(limiter);
    }
    
    /// 获取 RPC 限流器（内部使用）
    pub(crate) fn get_rpc_rate_limiter(&self) -> Option<&Arc<crate::rate_limiter::RpcRateLimiter>> {
        self.rpc_rate_limiter.as_ref()
    }
}

impl Drop for PrivchatClient {
    fn drop(&mut self) {
        tracing::debug!("PrivchatClient 正在清理资源");
    }
} 