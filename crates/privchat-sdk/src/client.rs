use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::collections::HashMap;
use tokio::sync::RwLock;
use rusqlite::Connection;
use uuid::Uuid;
use bytes::Bytes;
use serde::{Serialize, Deserialize};
use serde::de::DeserializeOwned;
use msgtrans::{
    transport::client::TransportClientBuilder,
    protocol::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig},
    transport::TransportOptions,
};
use crate::error::{PrivchatSDKError, Result};
use privchat_protocol::{
    encode_message, decode_message, MessageType,
    ConnectRequest, ConnectResponse, DisconnectRequest, DisconnectResponse,
    SubscribeRequest, SubscribeResponse,
    SendRequest, SendResponse, RecvRequest, RecvResponse,
    RecvBatchRequest, RecvBatchResponse, PublishRequest, PublishResponse,
    AuthType, ClientInfo, DeviceInfo, DeviceType, DisconnectReason,
    MessageSetting,
};

// ========== RPC 调用相关类型定义 ==========

/// RPC 调用结果类型
pub type RpcResult<T> = std::result::Result<T, RpcError>;

/// RPC 错误信息结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[code={}] {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

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

/// 用户会话信息
#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: String,
    pub token: String,
    pub device_id: String,
    pub login_time: chrono::DateTime<chrono::Utc>,
    pub server_key: Option<String>,
    pub node_id: Option<String>,
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
    user_id: Option<String>,
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
}

impl PrivchatClient {
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
        })
    }
    
    /// 连接并认证到服务器，支持多协议降级
    pub async fn connect(&mut self, phone: &str, token: &str) -> Result<UserSession> {
        tracing::info!("正在连接到服务器，手机号: {}", phone);
        
        // 按优先级尝试连接到不同的服务器端点
        let mut last_error = None;
        let endpoints = self.server_endpoints.clone(); // 克隆端点列表避免借用冲突
        for endpoint in &endpoints {
            tracing::info!("尝试连接到 {:?} 服务器: {}:{}", endpoint.protocol, endpoint.host, endpoint.port);
            
            match self.try_connect_to_endpoint(endpoint).await {
                Ok(mut transport) => {
                    // 连接成功，执行认证
                    match self.authenticate_with_transport(&mut transport, phone, token).await {
                        Ok(session) => {
                            self.transport = Some(transport);
                            self.session = Some(session.clone());
                            
                            // 初始化用户环境
                            self.initialize_user_environment(&session).await?;
                            
                            *self.connected.write().await = true;
                            tracing::info!("认证成功，用户ID: {}", session.user_id);
                            return Ok(session);
                        }
                        Err(e) => {
                            tracing::warn!("认证失败: {}", e);
                            last_error = Some(e);
                            continue;
                        }
                    }
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
    
    /// 尝试连接到指定端点
    async fn try_connect_to_endpoint(&self, endpoint: &ServerEndpoint) -> Result<msgtrans::transport::client::TransportClient> {
        let mut client = match endpoint.protocol {
            TransportProtocol::Quic => {
                let config = QuicClientConfig::new(&format!("{}:{}", endpoint.host, endpoint.port))
                    .map_err(|e| PrivchatSDKError::Transport(format!("创建 QUIC 配置失败: {}", e)))?;
                
                TransportClientBuilder::new()
                    .with_protocol(config)
                    .connect_timeout(self.connection_timeout)
                    .build()
                    .await
                    .map_err(|e| PrivchatSDKError::Transport(format!("构建 QUIC 客户端失败: {}", e)))?
            }
            TransportProtocol::Tcp => {
                let config = TcpClientConfig::new(&format!("{}:{}", endpoint.host, endpoint.port))
                    .map_err(|e| PrivchatSDKError::Transport(format!("创建 TCP 配置失败: {}", e)))?
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
        transport: &mut msgtrans::transport::client::TransportClient,
        phone: &str, 
        token: &str
    ) -> Result<UserSession> {
        // 1. 构建认证请求
        let connect_request = ConnectRequest {
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
            device_info: DeviceInfo {
                device_id: self.generate_device_id(),
                device_name: format!("{}-device", phone),
                device_type: DeviceType::Desktop,
                push_token: None,
                device_fingerprint: Some(format!("fingerprint_{}", Uuid::new_v4())),
            },
            protocol_version: "1.0".to_string(),
            properties: {
                let mut props = HashMap::new();
                props.insert("user_id".to_string(), phone.to_string());
                props.insert("client_timestamp".to_string(), chrono::Utc::now().timestamp_millis().to_string());
                props
            },
        };
        
        let request_data = encode_message(&connect_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码连接请求失败: {}", e)))?;
        
        // 2. 发送请求 - 使用 request_with_options 并设置正确的 biz_type
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::ConnectRequest as u8)
            .with_timeout(self.connection_timeout);
        
        let response_data = transport.request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("发送认证请求失败: {}", e)))?;
        
        // 3. 解析响应
        let connect_response: ConnectResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码连接响应失败: {}", e)))?;
        
        if !connect_response.success {
            let error_code = connect_response.error_code.unwrap_or_else(|| "UNKNOWN".to_string());
            let error_message = connect_response.error_message.unwrap_or_else(|| "认证失败".to_string());
            return Err(PrivchatSDKError::Auth(format!("认证失败，错误码: {}, 消息: {}", error_code, error_message)));
        }
        
        // 4. 创建用户会话
        let user_id = connect_response.user_id.unwrap_or_else(|| format!("u_{}", Uuid::new_v4()));
        let session = UserSession {
            user_id: user_id.clone(),
            token: token.to_string(),
            device_id: connect_request.device_info.device_id.clone(),
            login_time: chrono::Utc::now(),
            server_key: connect_response.session_id.clone(),
            node_id: connect_response.connection_id.clone(),
        };
        
        tracing::info!("认证成功，用户ID: {}, 会话ID: {:?}", user_id, session.server_key);
        
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
        if let Some(mut transport) = self.transport.take() {
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
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
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
        
        let ping_request = privchat_protocol::message::PingRequest {
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
        
        let _ping_response: privchat_protocol::message::PongResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码心跳响应失败: {}", e)))?;
        
        tracing::debug!("心跳检测成功");
        Ok(())
    }
    
    /// 订阅频道
    pub async fn subscribe_channel(&mut self, channel_id: &str) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let subscribe_request = SubscribeRequest {
            setting: 1,
            client_msg_no: format!("sub_{}", uuid::Uuid::new_v4()),
            channel_id: channel_id.to_string(),
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
    pub async fn send_message(&mut self, channel_id: &str, content: &str, message_type: u8) -> Result<String> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let client_msg_no = format!("msg_{}", Uuid::new_v4());
        let from_uid = self.user_id.as_ref().unwrap_or(&"unknown".to_string()).clone();
        
        let send_request = SendRequest {
            setting: MessageSetting {
                need_receipt: true,
                signal: 0,
            },
            client_seq: 1,
            client_msg_no: client_msg_no.clone(),
            stream_no: format!("stream_{}", Uuid::new_v4()),
            channel_id: channel_id.to_string(),
            channel_type: 1, // 1: 个人聊天, 2: 群聊
            expire: 3600,
            from_uid,
            topic: "chat".to_string(),
            payload: content.as_bytes().to_vec(),
        };
        
        let request_data = encode_message(&send_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码发送请求失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::SendRequest as u8)
            .with_timeout(Duration::from_secs(10));
        
        let response_data = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(request_data), transport_options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("发送消息请求失败: {}", e)))?;
        
        let send_response: SendResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码发送响应失败: {}", e)))?;
        
        if send_response.reason_code != 0 {
            return Err(PrivchatSDKError::Transport(format!("发送消息失败，错误码: {}", send_response.reason_code)));
        }
        
        tracing::info!("成功发送消息: {} -> {}", client_msg_no, channel_id);
        Ok(client_msg_no)
    }
    
    /// 处理接收到的消息并发送确认
    pub async fn handle_received_message(&mut self, recv_request: RecvRequest) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::info!(
            "收到消息: {} 来自: {} 频道: {} 内容: {}",
            recv_request.client_msg_no,
            recv_request.from_uid,
            recv_request.channel_id,
            String::from_utf8_lossy(&recv_request.payload)
        );
        
        // 发送接收确认
        let recv_response = RecvResponse {
            succeed: true,
            message: Some("消息接收成功".to_string()),
        };
        
        let response_data = encode_message(&recv_response)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码接收响应失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::RecvResponse as u8)
            .with_timeout(Duration::from_secs(5));
        
        let _ = self.transport.as_mut().unwrap()
            .request_with_options(Bytes::from(response_data), transport_options).await;
        
        Ok(())
    }
    
    /// 处理批量接收消息
    pub async fn handle_batch_messages(&mut self, batch_request: RecvBatchRequest) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        tracing::info!("收到批量消息，数量: {}", batch_request.message_count());
        
        // 处理每条消息
        for message in &batch_request.messages {
            tracing::info!(
                "批量消息: {} 来自: {} 频道: {}",
                message.client_msg_no,
                message.from_uid,
                message.channel_id
            );
        }
        
        // 发送批量确认
        let batch_response = RecvBatchResponse {
            succeed: true,
            message: Some(format!("成功处理 {} 条消息", batch_request.message_count())),
        };
        
        let response_data = encode_message(&batch_response)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码批量响应失败: {}", e)))?;
        
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::RecvBatchResponse as u8)
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
            return Err(RpcError {
                code: -1,
                message: "客户端未连接到服务器".to_string(),
            });
        }
        
        // 构建 RPC 请求
        let request = RPCMessageRequest {
            route: route.to_string(),
            body,
        };
        
        tracing::debug!("🚀 发送 RPC 请求: route={}, body={}", route, request.body);
        
        // 序列化请求
        let request_data = serde_json::to_vec(&request)
            .map_err(|e| RpcError {
                code: -2,
                message: format!("请求序列化失败: {}", e),
            })?;
        
        // 设置传输选项 - 使用 RPCRequest 消息类型
        let transport_options = TransportOptions::new()
            .with_biz_type(MessageType::RPCRequest as u8)
            .with_timeout(self.connection_timeout);
        
        // 发送请求并等待响应
        let response_data = self.transport.as_mut()
            .ok_or_else(|| RpcError {
                code: -1,
                message: "传输层客户端未初始化".to_string(),
            })?
            .request_with_options(Bytes::from(request_data), transport_options)
            .await
            .map_err(|e| RpcError {
                code: -3,
                message: format!("传输层错误: {}", e),
            })?;
        
        // 反序列化响应
        let rpc_response: RPCMessageResponse = serde_json::from_slice(&response_data)
            .map_err(|e| RpcError {
                code: -4,
                message: format!("响应反序列化失败: {}", e),
            })?;
        
        tracing::debug!("📥 收到 RPC 响应: route={}, code={}, message={}", 
                       route, rpc_response.code, rpc_response.message);
        
        // 检查响应状态码 (0 表示成功)
        if rpc_response.code != 0 {
            return Err(RpcError {
                code: rpc_response.code,
                message: rpc_response.message,
            });
        }
        
        // 提取数据并反序列化为目标类型
        let data = rpc_response.data.ok_or_else(|| RpcError {
            code: -5,
            message: "成功响应中缺少数据字段".to_string(),
        })?;
        
        serde_json::from_value(data).map_err(|e| RpcError {
            code: -6,
            message: format!("数据反序列化失败: {}", e),
        })
    }
    
    /// 初始化用户环境
    async fn initialize_user_environment(&mut self, session: &UserSession) -> Result<()> {
        // 创建用户目录
        let user_dir = self.work_dir.join(&session.user_id);
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
        self.user_id = Some(session.user_id.clone());
        
        tracing::info!("用户环境初始化完成，用户ID: {}", session.user_id);
        Ok(())
    }
    
    /// 生成设备ID
    fn generate_device_id(&self) -> String {
        format!("privchat_device_{}", Uuid::new_v4())
    }
    
    /// 派生加密密钥
    pub fn derive_encryption_key(user_id: &str) -> String {
        format!("encryption_key_{}", user_id)
    }
    
    /// 创建数据库表结构
    pub fn create_database_tables(conn: &Connection) -> Result<()> {
        // 创建基础表结构
        let create_tables_sql = r#"
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT UNIQUE NOT NULL,
                conversation_id TEXT NOT NULL,
                sender_id TEXT NOT NULL,
                content TEXT NOT NULL,
                message_type INTEGER NOT NULL,
                timestamp INTEGER NOT NULL,
                is_read INTEGER DEFAULT 0,
                is_deleted INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            
            CREATE TABLE IF NOT EXISTS conversations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                conversation_id TEXT UNIQUE NOT NULL,
                title TEXT,
                conversation_type INTEGER NOT NULL,
                participants TEXT NOT NULL,
                last_message_id TEXT,
                last_message_time INTEGER,
                unread_count INTEGER DEFAULT 0,
                is_archived INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            
            CREATE INDEX IF NOT EXISTS idx_messages_conversation_id ON messages(conversation_id);
            CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
            CREATE INDEX IF NOT EXISTS idx_conversations_last_message_time ON conversations(last_message_time);
        "#;
        
        conn.execute_batch(create_tables_sql)
            .map_err(|e| PrivchatSDKError::Database(format!("创建数据库表失败: {}", e)))?;
        
        Ok(())
    }
}

impl Drop for PrivchatClient {
    fn drop(&mut self) {
        tracing::debug!("PrivchatClient 正在清理资源");
    }
} 