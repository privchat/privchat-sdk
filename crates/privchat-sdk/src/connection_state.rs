//! 连接状态管理
//! 
//! 提供完整的连接状态信息，包括：
//! - 连接协议和服务器信息
//! - 用户和设备信息
//! - 性能统计
//! - 实时状态

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 连接协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionProtocol {
    /// QUIC 协议
    Quic,
    /// TCP 协议
    Tcp,
    /// WebSocket 协议
    WebSocket,
}

impl std::fmt::Display for ConnectionProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionProtocol::Quic => write!(f, "QUIC"),
            ConnectionProtocol::Tcp => write!(f, "TCP"),
            ConnectionProtocol::WebSocket => write!(f, "WebSocket"),
        }
    }
}

/// 连接状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    /// 未连接
    Disconnected,
    /// 连接中
    Connecting,
    /// 已连接
    Connected,
    /// 认证中
    Authenticating,
    /// 已认证
    Authenticated,
    /// 重连中
    Reconnecting,
    /// 连接失败
    Failed,
}

impl std::fmt::Display for ConnectionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionStatus::Disconnected => write!(f, "未连接"),
            ConnectionStatus::Connecting => write!(f, "连接中"),
            ConnectionStatus::Connected => write!(f, "已连接"),
            ConnectionStatus::Authenticating => write!(f, "认证中"),
            ConnectionStatus::Authenticated => write!(f, "已认证"),
            ConnectionStatus::Reconnecting => write!(f, "重连中"),
            ConnectionStatus::Failed => write!(f, "连接失败"),
        }
    }
}

/// 服务器信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// 服务器地址（IP:端口）
    pub address: String,
    /// 服务器版本
    pub version: Option<String>,
    /// 服务器名称
    pub name: Option<String>,
    /// 服务器区域（如：中国、美国等）
    pub region: Option<String>,
}

/// 用户信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    /// 用户ID
    pub user_id: String,
    /// 设备ID
    pub device_id: String,
    /// 会话ID
    pub session_id: Option<String>,
    /// 认证时间（UTC毫秒时间戳）
    pub auth_time: Option<i64>,
}

/// 性能统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PerformanceStats {
    /// 已发送消息数
    pub messages_sent: u64,
    /// 已接收消息数
    pub messages_received: u64,
    /// 已发送字节数
    pub bytes_sent: u64,
    /// 已接收字节数
    pub bytes_received: u64,
    /// 最后活动时间（UTC毫秒时间戳）
    pub last_activity_time: Option<i64>,
    /// 平均网络延迟（毫秒）
    pub avg_latency_ms: Option<u64>,
}

/// SDK 连接状态（完整信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionState {
    /// 连接协议
    pub protocol: ConnectionProtocol,
    /// 连接状态
    pub status: ConnectionStatus,
    /// 服务器信息
    pub server: ServerInfo,
    /// 用户信息
    pub user: Option<UserInfo>,
    /// 性能统计
    pub stats: PerformanceStats,
    /// 连接建立时间（UTC毫秒时间戳）
    pub connected_at: Option<i64>,
    /// 是否使用TLS加密
    pub use_tls: bool,
    /// SDK版本
    pub sdk_version: String,
    /// 客户端平台信息
    pub platform: String,
    /// 备注信息（可用于存储额外的调试信息）
    pub notes: Option<String>,
}

impl ConnectionState {
    /// 创建新的连接状态（未连接）
    pub fn new(platform: String) -> Self {
        Self {
            protocol: ConnectionProtocol::Tcp, // 默认值
            status: ConnectionStatus::Disconnected,
            server: ServerInfo {
                address: String::new(),
                version: None,
                name: None,
                region: None,
            },
            user: None,
            stats: PerformanceStats::default(),
            connected_at: None,
            use_tls: false,
            sdk_version: crate::version::SDK_VERSION.to_string(),
            platform,
            notes: None,
        }
    }
    
    /// 获取连接持续时间（秒）
    pub fn connection_duration_secs(&self) -> Option<i64> {
        self.connected_at.map(|connected_at| {
            let now = Utc::now().timestamp_millis();
            (now - connected_at) / 1000
        })
    }
    
    /// 格式化连接持续时间为可读字符串
    pub fn format_connection_duration(&self) -> String {
        match self.connection_duration_secs() {
            Some(secs) => {
                let hours = secs / 3600;
                let minutes = (secs % 3600) / 60;
                let seconds = secs % 60;
                
                if hours > 0 {
                    format!("{}小时{}分{}秒", hours, minutes, seconds)
                } else if minutes > 0 {
                    format!("{}分{}秒", minutes, seconds)
                } else {
                    format!("{}秒", seconds)
                }
            }
            None => "未连接".to_string(),
        }
    }
    
    /// 生成状态摘要（用于日志打印）
    pub fn summary(&self) -> String {
        let user_info = self.user.as_ref()
            .map(|u| format!("用户: {}, 设备: {}", u.user_id, u.device_id))
            .unwrap_or_else(|| "未认证".to_string());
        
        let duration = self.format_connection_duration();
        
        format!(
            "【连接状态】\n\
             协议: {}\n\
             状态: {}\n\
             服务器: {}\n\
             {}\n\
             已连接: {}\n\
             统计: 发送{}条/接收{}条\n\
             SDK版本: {}\n\
             平台: {}",
            self.protocol,
            self.status,
            self.server.address,
            user_info,
            duration,
            self.stats.messages_sent,
            self.stats.messages_received,
            self.sdk_version,
            self.platform
        )
    }
    
    /// 生成详细的JSON格式状态信息
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// 连接状态管理器（线程安全）
#[derive(Debug, Clone)]
pub struct ConnectionStateManager {
    state: Arc<RwLock<ConnectionState>>,
}

impl ConnectionStateManager {
    /// 创建新的状态管理器
    pub fn new(platform: String) -> Self {
        Self {
            state: Arc::new(RwLock::new(ConnectionState::new(platform))),
        }
    }
    
    /// 更新协议
    pub async fn set_protocol(&self, protocol: ConnectionProtocol) {
        let mut state = self.state.write().await;
        state.protocol = protocol;
    }
    
    /// 更新状态
    pub async fn set_status(&self, status: ConnectionStatus) {
        let mut state = self.state.write().await;
        state.status = status;
    }
    
    /// 更新服务器信息
    pub async fn set_server_info(&self, address: String, use_tls: bool) {
        let mut state = self.state.write().await;
        state.server.address = address;
        state.use_tls = use_tls;
        
        if state.status == ConnectionStatus::Disconnected {
            state.status = ConnectionStatus::Connecting;
        }
    }
    
    /// 设置服务器版本和名称
    pub async fn set_server_metadata(&self, version: Option<String>, name: Option<String>, region: Option<String>) {
        let mut state = self.state.write().await;
        state.server.version = version;
        state.server.name = name;
        state.server.region = region;
    }
    
    /// 标记连接成功
    pub async fn mark_connected(&self) {
        let mut state = self.state.write().await;
        state.status = ConnectionStatus::Connected;
        state.connected_at = Some(Utc::now().timestamp_millis());
    }
    
    /// 设置用户信息（认证成功后）
    pub async fn set_user_info(&self, user_id: String, device_id: String, session_id: Option<String>) {
        let mut state = self.state.write().await;
        state.user = Some(UserInfo {
            user_id,
            device_id,
            session_id,
            auth_time: Some(Utc::now().timestamp_millis()),
        });
        state.status = ConnectionStatus::Authenticated;
    }
    
    /// 标记断开连接
    pub async fn mark_disconnected(&self) {
        let mut state = self.state.write().await;
        state.status = ConnectionStatus::Disconnected;
        state.connected_at = None;
    }
    
    /// 增加发送消息计数
    pub async fn increment_sent(&self, byte_count: u64) {
        let mut state = self.state.write().await;
        state.stats.messages_sent += 1;
        state.stats.bytes_sent += byte_count;
        state.stats.last_activity_time = Some(Utc::now().timestamp_millis());
    }
    
    /// 增加接收消息计数
    pub async fn increment_received(&self, byte_count: u64) {
        let mut state = self.state.write().await;
        state.stats.messages_received += 1;
        state.stats.bytes_received += byte_count;
        state.stats.last_activity_time = Some(Utc::now().timestamp_millis());
    }
    
    /// 更新网络延迟
    pub async fn update_latency(&self, latency_ms: u64) {
        let mut state = self.state.write().await;
        state.stats.avg_latency_ms = Some(latency_ms);
    }
    
    /// 添加备注
    pub async fn set_notes(&self, notes: String) {
        let mut state = self.state.write().await;
        state.notes = Some(notes);
    }
    
    /// 获取当前状态快照
    pub async fn get_state(&self) -> ConnectionState {
        self.state.read().await.clone()
    }
    
    /// 获取状态摘要
    pub async fn get_summary(&self) -> String {
        let state = self.state.read().await;
        state.summary()
    }
    
    /// 打印状态到日志
    pub async fn log_state(&self) {
        let summary = self.get_summary().await;
        tracing::info!("\n{}", summary);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_connection_state_manager() {
        let manager = ConnectionStateManager::new("macOS 14.0".to_string());
        
        // 设置连接信息
        manager.set_protocol(ConnectionProtocol::Quic).await;
        manager.set_server_info("127.0.0.1:8082".to_string(), true).await;
        manager.mark_connected().await;
        
        // 设置用户信息
        manager.set_user_info(
            "1001".to_string(),
            "device_123".to_string(),
            Some("session_456".to_string())
        ).await;
        
        // 模拟消息发送
        manager.increment_sent(1024).await;
        manager.increment_received(2048).await;
        
        // 获取状态
        let state = manager.get_state().await;
        assert_eq!(state.protocol, ConnectionProtocol::Quic);
        assert_eq!(state.status, ConnectionStatus::Authenticated);
        assert_eq!(state.stats.messages_sent, 1);
        assert_eq!(state.stats.messages_received, 1);
        
        // 打印摘要
        let summary = manager.get_summary().await;
        println!("\n{}", summary);
    }
}
