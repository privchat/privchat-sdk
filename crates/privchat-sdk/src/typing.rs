//! 输入状态管理模块
//!
//! 功能包括：
//! - 发送输入状态通知
//! - 接收并处理其他用户的输入状态
//! - 防抖处理（避免频繁发送）

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::debug;

use privchat_protocol::presence::*;
use crate::events::{EventManager, SDKEvent};
use crate::storage::typing::TypingEvent;

/// 输入状态管理器
pub struct TypingManager {
    /// 当前正在输入的会话
    active_typing: Arc<RwLock<HashMap<u64, TypingState>>>,
    
    /// 事件管理器
    event_manager: Arc<EventManager>,
    
    /// 配置
    config: TypingConfig,
}

/// 输入状态
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TypingState {
    /// 会话ID
    channel_id: u64,
    
    /// 会话类型
    channel_type: u8,
    
    /// 输入动作类型
    action_type: TypingActionType,
    
    /// 开始时间
    started_at: Instant,
    
    /// 最后发送时间
    last_sent_at: Instant,
}

/// 输入状态配置
#[derive(Debug, Clone)]
pub struct TypingConfig {
    /// 防抖间隔（秒）- 多久发送一次输入状态
    pub debounce_interval_secs: u64,
    
    /// 自动清除时间（秒）- 超过多久自动认为停止输入
    pub auto_clear_secs: u64,
}

impl Default for TypingConfig {
    fn default() -> Self {
        Self {
            debounce_interval_secs: 3,  // 3秒发送一次
            auto_clear_secs: 5,          // 5秒后自动清除
        }
    }
}

impl TypingManager {
    /// 创建新的输入状态管理器
    pub fn new(event_manager: Arc<EventManager>) -> Self {
        Self::with_config(event_manager, TypingConfig::default())
    }
    
    /// 使用自定义配置创建
    pub fn with_config(event_manager: Arc<EventManager>, config: TypingConfig) -> Self {
        let manager = Self {
            active_typing: Arc::new(RwLock::new(HashMap::new())),
            event_manager,
            config,
        };
        
        // 启动自动清理任务
        manager.start_cleanup_task();
        
        manager
    }
    
    /// 记录开始输入
    /// 
    /// 返回 true 表示需要发送通知，false 表示在防抖间隔内，不需要发送
    pub async fn start_typing(
        &self,
        channel_id: u64,
        channel_type: u8,
        action_type: TypingActionType,
    ) -> bool {
        let now = Instant::now();
        let mut active = self.active_typing.write().await;
        
        if let Some(state) = active.get(&channel_id) {
            // 检查是否在防抖间隔内
            let elapsed = now.duration_since(state.last_sent_at);
            if elapsed.as_secs() < self.config.debounce_interval_secs {
                debug!("Typing debounced for channel {}", channel_id);
                return false; // 防抖，不发送
            }
            
            // 更新状态
            let mut updated_state = state.clone();
            updated_state.last_sent_at = now;
            updated_state.action_type = action_type;
            active.insert(channel_id, updated_state);
            
            return true; // 需要发送
        } else {
            // 新的输入状态
            let state = TypingState {
                channel_id,
                channel_type,
                action_type,
                started_at: now,
                last_sent_at: now,
            };
            active.insert(channel_id, state);
            
            return true; // 需要发送
        }
    }
    
    /// 停止输入
    pub async fn stop_typing(&self, channel_id: u64) {
        let mut active = self.active_typing.write().await;
        active.remove(&channel_id);
        debug!("Stopped typing for channel {}", channel_id);
    }
    
    /// 处理接收到的输入状态通知
    pub async fn handle_typing_notification(&self, notification: TypingStatusNotification) {
        debug!(
            "📥 Received typing notification: user {} in channel {} is_typing={}",
            notification.user_id, notification.channel_id, notification.is_typing
        );
        
        // 发布事件
        let event = SDKEvent::TypingIndicator(TypingEvent {
            user_id: notification.user_id,
            channel_id: notification.channel_id,
            channel_type: notification.channel_type as i32,
            is_typing: notification.is_typing,
            timestamp: notification.timestamp as u64,
            session_id: None,
        });
        
        self.event_manager.emit(event).await;
    }
    
    /// 启动自动清理任务
    fn start_cleanup_task(&self) {
        let active_typing = self.active_typing.clone();
        let auto_clear_secs = self.config.auto_clear_secs;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            
            loop {
                interval.tick().await;
                
                let now = Instant::now();
                let mut active = active_typing.write().await;
                
                // 移除超时的输入状态
                active.retain(|channel_id, state| {
                    let elapsed = now.duration_since(state.started_at);
                    if elapsed.as_secs() >= auto_clear_secs {
                        debug!("Auto-cleared typing status for channel {}", channel_id);
                        false
                    } else {
                        true
                    }
                });
            }
        });
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> TypingStats {
        let active = self.active_typing.read().await;
        TypingStats {
            active_typing_count: active.len(),
        }
    }
}

/// 输入状态统计
#[derive(Debug, Clone)]
pub struct TypingStats {
    pub active_typing_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;
    
    #[tokio::test]
    async fn test_typing_debounce() {
        let event_manager = Arc::new(EventManager::new(100));
        let manager = TypingManager::new(event_manager);
        
        // 第一次发送
        let should_send1 = manager.start_typing(123, 1, TypingActionType::Typing).await;
        assert!(should_send1); // 应该发送
        
        // 立即再次发送（在防抖间隔内）
        let should_send2 = manager.start_typing(123, 1, TypingActionType::Typing).await;
        assert!(!should_send2); // 不应该发送
        
        // 等待防抖间隔
        sleep(Duration::from_secs(4)).await;
        
        // 再次发送
        let should_send3 = manager.start_typing(123, 1, TypingActionType::Typing).await;
        assert!(should_send3); // 应该发送
    }
    
    #[tokio::test]
    async fn test_stop_typing() {
        let event_manager = Arc::new(EventManager::new(100));
        let manager = TypingManager::new(event_manager);
        
        // 开始输入
        manager.start_typing(123, 1, TypingActionType::Typing).await;
        
        let stats = manager.get_stats().await;
        assert_eq!(stats.active_typing_count, 1);
        
        // 停止输入
        manager.stop_typing(123).await;
        
        let stats = manager.get_stats().await;
        assert_eq!(stats.active_typing_count, 0);
    }
}
