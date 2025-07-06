use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use crate::error::Result;
use crate::storage::queue::MessageData;

/// 网络发送响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResponse {
    /// 服务端消息ID
    pub server_msg_id: String,
    /// 发送时间戳
    pub sent_at: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息（如果失败）
    pub error_message: Option<String>,
}

/// 网络发送器trait
#[async_trait]
pub trait NetworkSender: Send + Sync + std::fmt::Debug {
    /// 发送消息
    async fn send_message(&self, message: &MessageData) -> Result<SendResponse>;
    
    /// 撤回消息
    async fn revoke_message(&self, message_id: &str) -> Result<()>;
    
    /// 检查网络连接状态
    async fn check_connection(&self) -> bool;
}

/// 网络状态
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkStatus {
    /// 在线
    Online,
    /// 离线
    Offline,
    /// 连接中
    Connecting,
    /// 网络受限
    Limited,
}

/// 网络状态变化事件
#[derive(Debug, Clone)]
pub struct NetworkStatusEvent {
    pub old_status: NetworkStatus,
    pub new_status: NetworkStatus,
    pub timestamp: u64,
}

/// 网络状态监听器trait
#[async_trait]
pub trait NetworkStatusListener: Send + Sync + std::fmt::Debug {
    /// 获取当前网络状态
    async fn get_current_status(&self) -> NetworkStatus;
    
    /// 开始监听网络状态变化
    async fn start_monitoring(&self) -> Result<broadcast::Receiver<NetworkStatusEvent>>;
    
    /// 停止监听
    async fn stop_monitoring(&self);
}

/// 网络监控管理器
#[derive(Debug)]
pub struct NetworkMonitor {
    listener: Arc<dyn NetworkStatusListener>,
    sender: Arc<dyn NetworkSender>,
    status_sender: broadcast::Sender<NetworkStatusEvent>,
    current_status: Arc<tokio::sync::RwLock<NetworkStatus>>,
}

impl NetworkMonitor {
    pub fn new(
        listener: Arc<dyn NetworkStatusListener>,
        sender: Arc<dyn NetworkSender>,
    ) -> Self {
        let (status_sender, _) = broadcast::channel(100);
        
        Self {
            listener,
            sender,
            status_sender,
            current_status: Arc::new(tokio::sync::RwLock::new(NetworkStatus::Offline)),
        }
    }

    /// 启动网络监控
    pub async fn start(&self) -> Result<()> {
        let mut receiver = self.listener.start_monitoring().await?;
        let status_sender = self.status_sender.clone();
        let current_status = self.current_status.clone();
        
        // 启动监听任务
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                // 更新当前状态
                {
                    let mut status = current_status.write().await;
                    *status = event.new_status.clone();
                }
                
                // 广播状态变化
                let _ = status_sender.send(event);
            }
        });
        
        Ok(())
    }

    /// 获取当前网络状态
    pub async fn get_status(&self) -> NetworkStatus {
        self.current_status.read().await.clone()
    }

    /// 订阅网络状态变化
    pub fn subscribe(&self) -> broadcast::Receiver<NetworkStatusEvent> {
        self.status_sender.subscribe()
    }

    /// 检查网络连接
    pub async fn check_connection(&self) -> bool {
        self.sender.check_connection().await
    }
}

/// 虚拟网络发送器（用于测试）
#[derive(Debug, Clone)]
pub struct DummyNetworkSender {
    pub should_fail: bool,
    pub delay_ms: u64,
}

impl Default for DummyNetworkSender {
    fn default() -> Self {
        Self {
            should_fail: false,
            delay_ms: 100,
        }
    }
}

#[async_trait]
impl NetworkSender for DummyNetworkSender {
    async fn send_message(&self, message: &MessageData) -> Result<SendResponse> {
        // 模拟网络延迟
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        
        if self.should_fail {
            return Err(crate::error::PrivchatSDKError::Transport("Network error".to_string()));
        }
        
        Ok(SendResponse {
            server_msg_id: format!("server_{}", message.client_msg_no),
            sent_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            success: true,
            error_message: None,
        })
    }

    async fn revoke_message(&self, _message_id: &str) -> Result<()> {
        tokio::time::sleep(tokio::time::Duration::from_millis(self.delay_ms)).await;
        
        if self.should_fail {
            return Err(crate::error::PrivchatSDKError::Transport("Network error".to_string()));
        }
        
        Ok(())
    }

    async fn check_connection(&self) -> bool {
        !self.should_fail
    }
}

/// 虚拟网络状态监听器（用于测试）
#[derive(Debug)]
pub struct DummyNetworkStatusListener {
    status: Arc<tokio::sync::RwLock<NetworkStatus>>,
    sender: Arc<tokio::sync::RwLock<Option<broadcast::Sender<NetworkStatusEvent>>>>,
}

impl Default for DummyNetworkStatusListener {
    fn default() -> Self {
        Self {
            status: Arc::new(tokio::sync::RwLock::new(NetworkStatus::Online)),
            sender: Arc::new(tokio::sync::RwLock::new(None)),
        }
    }
}

impl DummyNetworkStatusListener {
    /// 模拟网络状态变化
    pub async fn simulate_status_change(&self, new_status: NetworkStatus) {
        let old_status = {
            let mut status = self.status.write().await;
            let old = status.clone();
            *status = new_status.clone();
            old
        };
        
        if let Some(sender) = self.sender.read().await.as_ref() {
            let event = NetworkStatusEvent {
                old_status,
                new_status,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            };
            
            let _ = sender.send(event);
        }
    }
}

#[async_trait]
impl NetworkStatusListener for DummyNetworkStatusListener {
    async fn get_current_status(&self) -> NetworkStatus {
        self.status.read().await.clone()
    }

    async fn start_monitoring(&self) -> Result<broadcast::Receiver<NetworkStatusEvent>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::MessageData;

    #[tokio::test]
    async fn test_dummy_network_sender() {
        let sender = DummyNetworkSender::default();
        
        let message = MessageData::new(
            "test_123".to_string(),
            "channel_1".to_string(),
            1, // channel_type
            "user_1".to_string(), // from_uid
            "Hello".to_string(),
            1, // message_type
        );
        
        let response = sender.send_message(&message).await;
        assert!(response.is_ok());
        
        let response = response.unwrap();
        assert!(response.success);
        assert_eq!(response.server_msg_id, "server_test_123");
    }

    #[tokio::test]
    async fn test_dummy_network_sender_failure() {
        let sender = DummyNetworkSender {
            should_fail: true,
            delay_ms: 10,
        };
        
        let message = MessageData::new(
            "test_123".to_string(),
            "channel_1".to_string(),
            1, // channel_type
            "user_1".to_string(), // from_uid
            "Hello".to_string(),
            1, // message_type
        );
        
        let response = sender.send_message(&message).await;
        assert!(response.is_err());
    }

    #[tokio::test]
    async fn test_network_status_listener() {
        let listener = DummyNetworkStatusListener::default();
        
        let mut receiver = listener.start_monitoring().await.unwrap();
        
        // 模拟状态变化
        listener.simulate_status_change(NetworkStatus::Offline).await;
        
        // 接收状态变化事件
        let event = receiver.recv().await.unwrap();
        assert_eq!(event.old_status, NetworkStatus::Online);
        assert_eq!(event.new_status, NetworkStatus::Offline);
        
        listener.stop_monitoring().await;
    }

    #[tokio::test]
    async fn test_network_monitor() {
        let listener = Arc::new(DummyNetworkStatusListener::default());
        let sender = Arc::new(DummyNetworkSender::default());
        
        let monitor = NetworkMonitor::new(listener.clone(), sender);
        
        // 启动监控
        monitor.start().await.unwrap();
        
        // 订阅状态变化
        let mut receiver = monitor.subscribe();
        
        // 模拟状态变化
        listener.simulate_status_change(NetworkStatus::Offline).await;
        
        // 接收状态变化事件
        let event = receiver.recv().await.unwrap();
        assert_eq!(event.new_status, NetworkStatus::Offline);
        
        // 检查监控器的状态
        let status = monitor.get_status().await;
        assert_eq!(status, NetworkStatus::Offline);
    }
} 