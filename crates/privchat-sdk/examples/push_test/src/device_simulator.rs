//! 设备模拟器
//! 
//! 模拟设备的行为：连接、断开、前台/后台切换等

use privchat_sdk::{PrivchatSDK, PrivchatConfig, ServerEndpoint, TransportProtocol, ServerConfig};
use privchat_sdk::error::Result;
use privchat_protocol::protocol::DeviceInfo;
use privchat_protocol::protocol::DeviceType;
use tracing::{info, debug, warn};
use std::time::Duration;
use std::path::PathBuf;
use std::sync::Arc;

/// 设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Online,      // 在线（已连接）
    Offline,     // 离线（未连接）
    Background,  // 后台（已连接但 apns_armed=true）
}

/// 模拟设备
pub struct DeviceSimulator {
    pub device_id: String,
    pub sdk: Option<Arc<PrivchatSDK>>,
    pub state: DeviceState,
    pub apns_armed: bool,
    pub user_id: Option<u64>,
    pub token: Option<String>,
}

impl DeviceSimulator {
    pub fn new(device_id: String) -> Self {
        Self {
            device_id,
            sdk: None,
            state: DeviceState::Offline,
            apns_armed: false,
            user_id: None,
            token: None,
        }
    }
    
    /// 注册并连接设备
    pub async fn register_and_connect(
        &mut self,
        server_url: &str,
        username: &str,
        password: &str,
    ) -> Result<()> {
        info!("[设备 {}] 正在注册并连接...", self.device_id);
        
        // 1. 创建 SDK 配置
        let config = PrivchatConfig {
            data_dir: PathBuf::from(format!("/tmp/push_test_{}", self.device_id)),
            server_config: ServerConfig {
                endpoints: vec![
                    ServerEndpoint {
                        protocol: TransportProtocol::WebSocket,
                        host: server_url.replace("ws://", "").replace("http://", "").split(':').next().unwrap_or("127.0.0.1").to_string(),
                        port: server_url.split(':').last().unwrap_or("8081").parse().unwrap_or(8081),
                        path: Some("/".to_string()),
                        use_tls: false,
                    },
                ],
            },
            ..Default::default()
        };
        
        // 2. 初始化 SDK 实例
        let sdk = PrivchatSDK::initialize(config).await?;
        
        // 3. 建立网络连接
        sdk.connect().await?;
        info!("[设备 {}] 网络连接已建立", self.device_id);
        
        // 4. 注册用户
        let device_info = DeviceInfo {
            device_id: self.device_id.clone(),
            device_type: DeviceType::Web,
            app_id: "push_test".to_string(),
            push_token: Some(format!("mock_push_token_{}", self.device_id)),
            push_channel: Some("fcm".to_string()),
            device_name: format!("Push Test Device {}", self.device_id),
            device_model: Some("Test Device".to_string()),
            os_version: Some("Test OS".to_string()),
            app_version: Some("1.0.0".to_string()),
            manufacturer: None,
            device_fingerprint: None,
        };
        
        let (user_id, token) = sdk.register(
            username.to_string(),
            password.to_string(),
            self.device_id.clone(),
            Some(device_info.clone()),
        ).await?;
        
        info!("[设备 {}] 用户注册成功: user_id={}", self.device_id, user_id);
        
        // 5. 认证
        sdk.authenticate(user_id, &token, device_info).await?;
        info!("[设备 {}] 认证成功", self.device_id);
        
        self.sdk = Some(sdk);
        self.user_id = Some(user_id);
        self.token = Some(token);
        self.state = DeviceState::Online;
        self.apns_armed = false;
        
        info!("[设备 {}] ✅ 已连接", self.device_id);
        Ok(())
    }
    
    /// 登录并连接设备（如果用户已存在）
    pub async fn login_and_connect(
        &mut self,
        server_url: &str,
        username: &str,
        password: &str,
    ) -> Result<()> {
        info!("[设备 {}] 正在登录并连接...", self.device_id);
        
        // 1. 创建 SDK 配置
        let config = PrivchatConfig {
            data_dir: PathBuf::from(format!("/tmp/push_test_{}", self.device_id)),
            server_config: ServerConfig {
                endpoints: vec![
                    ServerEndpoint {
                        protocol: TransportProtocol::WebSocket,
                        host: server_url.replace("ws://", "").replace("http://", "").split(':').next().unwrap_or("127.0.0.1").to_string(),
                        port: server_url.split(':').last().unwrap_or("8081").parse().unwrap_or(8081),
                        path: Some("/".to_string()),
                        use_tls: false,
                    },
                ],
            },
            ..Default::default()
        };
        
        // 2. 初始化 SDK 实例
        let sdk = PrivchatSDK::initialize(config).await?;
        
        // 3. 建立网络连接
        sdk.connect().await?;
        info!("[设备 {}] 网络连接已建立", self.device_id);
        
        // 4. 登录用户
        let device_info = DeviceInfo {
            device_id: self.device_id.clone(),
            device_type: DeviceType::Web,
            app_id: "push_test".to_string(),
            push_token: Some(format!("mock_push_token_{}", self.device_id)),
            push_channel: Some("fcm".to_string()),
            device_name: format!("Push Test Device {}", self.device_id),
            device_model: Some("Test Device".to_string()),
            os_version: Some("Test OS".to_string()),
            app_version: Some("1.0.0".to_string()),
            manufacturer: None,
            device_fingerprint: None,
        };
        
        let (user_id, token) = sdk.login(
            username.to_string(),
            password.to_string(),
            self.device_id.clone(),
            Some(device_info.clone()),
        ).await?;
        
        info!("[设备 {}] 用户登录成功: user_id={}", self.device_id, user_id);
        
        // 5. 认证
        sdk.authenticate(user_id, &token, device_info).await?;
        info!("[设备 {}] 认证成功", self.device_id);
        
        self.sdk = Some(sdk);
        self.user_id = Some(user_id);
        self.token = Some(token);
        self.state = DeviceState::Online;
        self.apns_armed = false;
        
        info!("[设备 {}] ✅ 已连接", self.device_id);
        Ok(())
    }
    
    /// 断开设备（模拟下线）
    pub async fn disconnect(&mut self) -> Result<()> {
        if let Some(_sdk) = &self.sdk {
            info!("[设备 {}] 正在断开...", self.device_id);
            // TODO: 实现断开逻辑（SDK 可能没有 disconnect 方法）
            self.sdk = None;
            self.state = DeviceState::Offline;
            info!("[设备 {}] ✅ 已断开", self.device_id);
        }
        Ok(())
    }
    
    /// 切换到后台（模拟前台→后台）
    /// 
    /// 使用 SDK 的标准生命周期方法
    pub async fn switch_to_background(&mut self) -> Result<()> {
        if self.state != DeviceState::Online {
            warn!("[设备 {}] 设备不在线，无法切换到后台", self.device_id);
            return Ok(());
        }
        
        info!("[设备 {}] 切换到后台...", self.device_id);
        
        // 使用 SDK 的标准生命周期方法（会触发所有注册的 Hook）
        if let Some(sdk) = &self.sdk {
            sdk.on_app_background().await?;
        }
        
        self.apns_armed = true;
        self.state = DeviceState::Background;
        
        info!("[设备 {}] ✅ 已切换到后台 (apns_armed=true)", self.device_id);
        Ok(())
    }
    
    /// 切换到前台（模拟后台→前台）
    /// 
    /// 使用 SDK 的标准生命周期方法
    pub async fn switch_to_foreground(&mut self) -> Result<()> {
        if self.state != DeviceState::Background {
            warn!("[设备 {}] 设备不在后台，无法切换到前台", self.device_id);
            return Ok(());
        }
        
        info!("[设备 {}] 切换到前台...", self.device_id);
        
        // 使用 SDK 的标准生命周期方法（会触发所有注册的 Hook）
        if let Some(sdk) = &self.sdk {
            sdk.on_app_foreground().await?;
        }
        
        self.apns_armed = false;
        self.state = DeviceState::Online;
        
        info!("[设备 {}] ✅ 已切换到前台 (apns_armed=false)", self.device_id);
        Ok(())
    }
    
    /// 发送消息
    pub async fn send_message(&self, channel_id: u64, content: &str) -> Result<u64> {
        if let Some(sdk) = &self.sdk {
            info!("[设备 {}] 发送消息到频道 {}: {}", self.device_id, channel_id, content);
            
            // 使用 SDK 发送消息
            let local_message_id = sdk.send_message(channel_id, content).await?;
            
            info!("[设备 {}] ✅ 消息发送成功: local_message_id={}", self.device_id, local_message_id);
            Ok(local_message_id)
        } else {
            Err(privchat_sdk::error::PrivchatSDKError::NotConnected)
        }
    }
    
    /// 撤销消息
    pub async fn revoke_message(&self, channel_id: u64, message_id: u64) -> Result<()> {
        if let Some(sdk) = &self.sdk {
            info!("[设备 {}] 撤销消息: channel_id={}, message_id={}", self.device_id, channel_id, message_id);
            
            // 使用 RPC 撤销消息
            use privchat_protocol::rpc::routes;
            use privchat_protocol::rpc::message::revoke::MessageRevokeRequest;
            
            let request = MessageRevokeRequest {
                message_id,
                channel_id,
                user_id: self.user_id.unwrap_or(0),
            };
            
            let request_json = serde_json::to_value(request)?;
            let response: serde_json::Value = sdk.rpc_call(routes::message::REVOKE, request_json).await?;
            info!("[设备 {}] 撤销消息 RPC 响应: {:?}", self.device_id, response);
            
            info!("[设备 {}] ✅ 消息撤销成功", self.device_id);
            Ok(())
        } else {
            Err(privchat_sdk::error::PrivchatSDKError::NotConnected)
        }
    }
    
    /// 获取私聊频道ID（通过创建或查找）
    pub async fn get_or_create_direct_channel(&self, other_user_id: u64) -> Result<u64> {
        if let Some(sdk) = &self.sdk {
            // 尝试从存储中查找已有的频道
            if let Ok(Some(channel_id)) = sdk.storage().find_channel_id_by_user(other_user_id).await {
                info!("[设备 {}] 找到已有频道: channel_id={}", self.device_id, channel_id);
                return Ok(channel_id);
            }
            
            // 如果没有找到，尝试通过发送消息自动创建（SDK 会自动创建私聊频道）
            // 或者使用 RPC 创建频道
            // 暂时返回一个基于 user_id 的 channel_id（实际应该通过 RPC 创建）
            warn!("[设备 {}] 未找到频道，使用临时 channel_id (实际应该通过 RPC 创建)", self.device_id);
            Ok(1000 + other_user_id)
        } else {
            Err(privchat_sdk::error::PrivchatSDKError::NotConnected)
        }
    }
    
    /// 等待一段时间（用于观察日志）
    pub async fn wait(&self, seconds: u64) {
        tokio::time::sleep(Duration::from_secs(seconds)).await;
    }
}
