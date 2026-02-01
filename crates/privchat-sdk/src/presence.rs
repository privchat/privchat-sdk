//! 在线状态管理模块
//!
//! 功能包括：
//! - 订阅/取消订阅用户在线状态
//! - 缓存在线状态信息
//! - 处理服务端推送的状态变化
//! - 批量查询在线状态

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use serde::{Deserialize, Serialize};

use privchat_protocol::presence::*;
use crate::events::{EventManager, event_builders};

/// 在线状态管理器
pub struct PresenceManager {
    /// 在线状态缓存
    status_cache: Arc<RwLock<HashMap<u64, OnlineStatusInfo>>>,
    
    /// 当前订阅的用户列表
    subscribed_users: Arc<RwLock<HashSet<u64>>>,
    
    /// 事件管理器（用于发布状态变化事件）
    event_manager: Arc<EventManager>,
    
    /// 缓存配置
    config: PresenceCacheConfig,
}

/// 缓存配置
#[derive(Debug, Clone)]
pub struct PresenceCacheConfig {
    /// 缓存过期时间（秒）
    pub cache_ttl_secs: u64,
    
    /// 最大缓存条目数
    pub max_cache_size: usize,
    
    /// 是否启用自动清理
    pub enable_auto_cleanup: bool,
}

impl Default for PresenceCacheConfig {
    fn default() -> Self {
        Self {
            cache_ttl_secs: 300,      // 5分钟
            max_cache_size: 10000,     // 最多缓存10000个用户
            enable_auto_cleanup: true,
        }
    }
}

impl PresenceManager {
    /// 创建新的在线状态管理器
    pub fn new(event_manager: Arc<EventManager>) -> Self {
        Self::with_config(event_manager, PresenceCacheConfig::default())
    }
    
    /// 使用自定义配置创建管理器
    pub fn with_config(event_manager: Arc<EventManager>, config: PresenceCacheConfig) -> Self {
        let manager = Self {
            status_cache: Arc::new(RwLock::new(HashMap::new())),
            subscribed_users: Arc::new(RwLock::new(HashSet::new())),
            event_manager,
            config,
        };
        
        // 启动自动清理任务
        if manager.config.enable_auto_cleanup {
            manager.start_cleanup_task();
        }
        
        manager
    }
    
    /// 订阅用户在线状态（内部方法，不直接调用RPC）
    ///
    /// 这个方法只管理本地订阅状态，实际的RPC调用在SDK层完成
    pub async fn add_subscription(&self, user_ids: Vec<u64>) {
        let mut subscribed = self.subscribed_users.write().await;
        for user_id in user_ids {
            subscribed.insert(user_id);
        }
        debug!("Added {} subscriptions, total: {}", subscribed.len(), subscribed.len());
    }
    
    /// 取消订阅（内部方法）
    pub async fn remove_subscription(&self, user_ids: Vec<u64>) {
        let mut subscribed = self.subscribed_users.write().await;
        for user_id in &user_ids {
            subscribed.remove(user_id);
        }
        debug!("Removed {} subscriptions, remaining: {}", user_ids.len(), subscribed.len());
    }
    
    /// 更新在线状态缓存
    pub async fn update_status(&self, user_id: u64, status_info: OnlineStatusInfo) {
        let mut cache = self.status_cache.write().await;
        
        // 检查缓存大小限制
        if cache.len() >= self.config.max_cache_size && !cache.contains_key(&user_id) {
            warn!("Cache size limit reached ({}), removing oldest entry", self.config.max_cache_size);
            // 简单策略：移除第一个元素
            if let Some((&first_key, _)) = cache.iter().next() {
                cache.remove(&first_key);
            }
        }
        
        cache.insert(user_id, status_info);
        debug!("Updated status for user {}, cache size: {}", user_id, cache.len());
    }
    
    /// 批量更新在线状态
    pub async fn batch_update_status(&self, statuses: HashMap<u64, OnlineStatusInfo>) {
        let mut cache = self.status_cache.write().await;
        
        for (user_id, status_info) in statuses {
            // 检查缓存大小
            if cache.len() >= self.config.max_cache_size && !cache.contains_key(&user_id) {
                break; // 达到最大缓存，停止添加新条目
            }
            cache.insert(user_id, status_info);
        }
        
        info!("Batch updated {} statuses, cache size: {}", cache.len(), cache.len());
    }
    
    /// 获取用户在线状态（从缓存）
    pub async fn get_status(&self, user_id: u64) -> Option<OnlineStatusInfo> {
        let cache = self.status_cache.read().await;
        cache.get(&user_id).cloned()
    }
    
    /// 批量获取在线状态（从缓存）
    pub async fn batch_get_status(&self, user_ids: &[u64]) -> HashMap<u64, OnlineStatusInfo> {
        let cache = self.status_cache.read().await;
        let mut result = HashMap::new();
        
        for user_id in user_ids {
            if let Some(status) = cache.get(user_id) {
                result.insert(*user_id, status.clone());
            }
        }
        
        result
    }
    
    /// 处理服务端推送的在线状态变化
    pub async fn handle_status_change(&self, notification: OnlineStatusChangeNotification) {
        debug!(
            "Handling status change for user {}: {:?} -> {:?}",
            notification.user_id, notification.old_status, notification.new_status
        );
        
        // 更新缓存
        let status_info = OnlineStatusInfo {
            user_id: notification.user_id,
            status: notification.new_status.clone(),
            last_seen: notification.last_seen,
            online_devices: vec![], // 服务端推送时不包含设备列表
        };
        self.update_status(notification.user_id, status_info).await;
        
        // 发布事件
        let is_online = matches!(notification.new_status, OnlineStatus::Online);
        let last_seen = if notification.last_seen > 0 {
            Some(notification.last_seen as u64)
        } else {
            None
        };
        
        let event = event_builders::user_presence_changed(
            notification.user_id,
            is_online,
            last_seen,
        );
        
        self.event_manager.emit(event).await;
        
        info!(
            "✅ User {} presence changed: {:?} -> {:?}",
            notification.user_id, notification.old_status, notification.new_status
        );
    }
    
    /// 检查用户是否被订阅
    pub async fn is_subscribed(&self, user_id: u64) -> bool {
        let subscribed = self.subscribed_users.read().await;
        subscribed.contains(&user_id)
    }
    
    /// 获取所有订阅的用户ID
    pub async fn get_subscribed_users(&self) -> Vec<u64> {
        let subscribed = self.subscribed_users.read().await;
        subscribed.iter().copied().collect()
    }
    
    /// 清空缓存
    pub async fn clear_cache(&self) {
        let mut cache = self.status_cache.write().await;
        cache.clear();
        info!("Cleared presence cache");
    }
    
    /// 清空订阅列表
    pub async fn clear_subscriptions(&self) {
        let mut subscribed = self.subscribed_users.write().await;
        subscribed.clear();
        info!("Cleared all subscriptions");
    }
    
    /// 获取缓存统计信息
    pub async fn get_cache_stats(&self) -> PresenceCacheStats {
        let cache = self.status_cache.read().await;
        let subscribed = self.subscribed_users.read().await;
        
        PresenceCacheStats {
            cached_users: cache.len(),
            subscribed_users: subscribed.len(),
            max_cache_size: self.config.max_cache_size,
            cache_ttl_secs: self.config.cache_ttl_secs,
        }
    }
    
    /// 启动自动清理任务
    fn start_cleanup_task(&self) {
        let cache = self.status_cache.clone();
        let cleanup_interval = Duration::from_secs(self.config.cache_ttl_secs);
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(cleanup_interval);
            loop {
                interval.tick().await;
                
                // 简单清理策略：清空整个缓存
                // 实际应用中应该根据时间戳清理过期条目
                let mut cache_guard = cache.write().await;
                if !cache_guard.is_empty() {
                    cache_guard.clear();
                    debug!("Auto-cleaned presence cache");
                }
            }
        });
    }
}

/// 缓存统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceCacheStats {
    /// 已缓存的用户数
    pub cached_users: usize,
    
    /// 已订阅的用户数
    pub subscribed_users: usize,
    
    /// 最大缓存大小
    pub max_cache_size: usize,
    
    /// 缓存TTL（秒）
    pub cache_ttl_secs: u64,
}

/// 辅助函数：从时间戳计算在线状态
pub fn calculate_online_status(last_seen_timestamp: i64) -> OnlineStatus {
    use chrono::Utc;
    
    let now = Utc::now().timestamp();
    let elapsed = now - last_seen_timestamp;
    
    OnlineStatus::from_elapsed_seconds(elapsed)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_presence_manager_basic() {
        let event_manager = Arc::new(EventManager::new(100));
        let manager = PresenceManager::new(event_manager);
        
        // 测试订阅
        manager.add_subscription(vec![1, 2, 3]).await;
        assert!(manager.is_subscribed(1).await);
        assert!(manager.is_subscribed(2).await);
        assert!(!manager.is_subscribed(999).await);
        
        // 测试更新状态
        let status = OnlineStatusInfo {
            user_id: 1,
            status: OnlineStatus::Online,
            last_seen: 1234567890,
            online_devices: vec![],
        };
        manager.update_status(1, status.clone()).await;
        
        // 测试获取状态
        let cached = manager.get_status(1).await;
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().user_id, 1);
        
        // 测试取消订阅
        manager.remove_subscription(vec![1]).await;
        assert!(!manager.is_subscribed(1).await);
    }
    
    #[tokio::test]
    async fn test_batch_operations() {
        let event_manager = Arc::new(EventManager::new(100));
        let manager = PresenceManager::new(event_manager);
        
        // 批量更新
        let mut statuses = HashMap::new();
        for i in 1..=10 {
            statuses.insert(i, OnlineStatusInfo {
                user_id: i,
                status: OnlineStatus::Online,
                last_seen: 1234567890,
                online_devices: vec![],
            });
        }
        manager.batch_update_status(statuses).await;
        
        // 批量获取
        let user_ids: Vec<u64> = (1..=10).collect();
        let result = manager.batch_get_status(&user_ids).await;
        assert_eq!(result.len(), 10);
    }
    
    #[test]
    fn test_calculate_online_status() {
        use chrono::Utc;
        
        let now = Utc::now().timestamp();
        
        // 在线（3分钟内）
        assert_eq!(calculate_online_status(now - 60), OnlineStatus::Online);
        
        // 最近在线（1小时内）
        assert_eq!(calculate_online_status(now - 3600), OnlineStatus::Recently);
        
        // 上周在线（7天内）
        assert_eq!(calculate_online_status(now - 86400 * 3), OnlineStatus::LastWeek);
    }
}
