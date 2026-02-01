use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;
use crate::error::Result;

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

/// 网络状态监听器trait（由平台层实现，如 Android/iOS）
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
    status_sender: broadcast::Sender<NetworkStatusEvent>,
    current_status: Arc<tokio::sync::RwLock<NetworkStatus>>,
}

impl NetworkMonitor {
    pub fn new(listener: Arc<dyn NetworkStatusListener>) -> Self {
        let (status_sender, _) = broadcast::channel(100);
        
        Self {
            listener,
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
    
    /// 手动设置网络状态（用于连接成功后更新状态）
    pub async fn set_status(&self, new_status: NetworkStatus) {
        let old_status = {
            let mut status = self.current_status.write().await;
            let old = status.clone();
            *status = new_status.clone();
            old
        };
        
        // 广播状态变化
        let event = NetworkStatusEvent {
            old_status,
            new_status,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        };
        let _ = self.status_sender.send(event);
    }

    /// 订阅网络状态变化
    pub fn subscribe(&self) -> broadcast::Receiver<NetworkStatusEvent> {
        self.status_sender.subscribe()
    }

    /// 检查网络连接（通过网络状态判断）
    pub async fn check_connection(&self) -> bool {
        let status = self.get_status().await;
        matches!(status, NetworkStatus::Online | NetworkStatus::Limited)
    }
}

#[cfg(test)]
pub mod test_helpers {
    use super::*;

    /// 测试用：始终返回在线的网络状态监听器
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

    #[async_trait::async_trait]
    impl NetworkStatusListener for DummyNetworkStatusListener {
        async fn get_current_status(&self) -> NetworkStatus {
            self.status.read().await.clone()
        }

        async fn start_monitoring(&self) -> Result<broadcast::Receiver<NetworkStatusEvent>> {
            let (tx, rx) = broadcast::channel(16);
            *self.sender.write().await = Some(tx);
            Ok(rx)
        }

        async fn stop_monitoring(&self) {
            *self.sender.write().await = None;
        }
    }
}

#[cfg(test)]
pub use test_helpers::DummyNetworkStatusListener;
