use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// 消息去重管理器
/// 
/// 基于 message_id 实现消息去重，防止重复消息处理
pub struct DeduplicationManager {
    /// 已处理的消息集合 (message_id -> timestamp)
    processed_messages: Arc<Mutex<HashMap<String, Instant>>>,
    
    /// 消息保留时间（秒）
    message_retention: Duration,
    
    /// 最大缓存大小
    max_cache_size: usize,
    
    /// 清理阈值（当缓存大小超过此值时触发清理）
    cleanup_threshold: usize,
}

impl DeduplicationManager {
    /// 创建新的消息去重管理器
    pub fn new() -> Self {
        Self {
            processed_messages: Arc::new(Mutex::new(HashMap::new())),
            message_retention: Duration::from_secs(3600), // 保留1小时
            max_cache_size: 10000, // 最大缓存10000条消息
            cleanup_threshold: 8000, // 超过8000条时触发清理
        }
    }
    
    /// 使用自定义配置创建消息去重管理器
    pub fn with_config(
        message_retention: Duration,
        max_cache_size: usize,
    ) -> Self {
        Self {
            processed_messages: Arc::new(Mutex::new(HashMap::new())),
            message_retention,
            max_cache_size,
            cleanup_threshold: max_cache_size * 4 / 5, // 80% 阈值
        }
    }
    
    /// 检查消息是否已处理（去重检查）
    /// 
    /// 返回 true 如果消息已处理过（重复消息），false 如果未处理过
    pub fn is_duplicate(&self, message_id: u64) -> bool {
        let processed = self.processed_messages.lock().unwrap();
        
        // 检查是否存在
        let message_id_str = message_id.to_string();
        if processed.contains_key(&message_id_str) {
            debug!("🔄 检测到重复消息: message_id={}", message_id);
            return true;
        }
        
        false
    }
    
    /// 标记消息为已处理
    pub fn mark_as_processed(&self, message_id: u64) {
        let mut processed = self.processed_messages.lock().unwrap();
        processed.insert(message_id.to_string(), Instant::now());
        
        // 检查是否需要清理
        if processed.len() > self.cleanup_threshold {
            self.cleanup_expired_internal(&mut processed);
        }
        
        debug!("✅ 标记消息为已处理: message_id={}", message_id);
    }
    
    /// 内部清理方法（需要已持有锁）
    fn cleanup_expired_internal(&self, processed: &mut HashMap<String, Instant>) {
        let now = Instant::now();
        let initial_count = processed.len();
        
        // 移除过期的记录
        processed.retain(|_, timestamp| {
            now.duration_since(*timestamp) <= self.message_retention
        });
        
        let removed_count = initial_count - processed.len();
        if removed_count > 0 {
            info!("🧹 清理过期消息记录: 移除了 {} 条记录，剩余 {} 条", 
                  removed_count, processed.len());
        }
    }
    
    /// 清理过期的消息记录（外部调用）
    pub fn cleanup_expired(&self) {
        let mut processed = self.processed_messages.lock().unwrap();
        self.cleanup_expired_internal(&mut processed);
    }
    
    /// 获取统计信息
    pub fn get_stats(&self) -> (usize, usize) {
        let processed = self.processed_messages.lock().unwrap();
        (processed.len(), self.max_cache_size)
    }
    
    /// 清空所有记录
    pub fn clear(&self) {
        let mut processed = self.processed_messages.lock().unwrap();
        processed.clear();
        info!("消息去重缓存已清空");
    }
}

impl Default for DeduplicationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration as StdDuration;
    
    #[test]
    fn test_message_dedup() {
        let manager = DeduplicationManager::new();
        
        // 第一次检查应该返回 false（未处理过）
        assert!(!manager.is_duplicate("msg1"));
        
        // 标记为已处理
        manager.mark_as_processed("msg1");
        
        // 再次检查应该返回 true（已处理过）
        assert!(manager.is_duplicate("msg1"));
        
        // 不同的消息应该返回 false
        assert!(!manager.is_duplicate("msg2"));
    }
    
    #[test]
    fn test_cleanup_expired() {
        let manager = DeduplicationManager::with_config(
            Duration::from_secs(1), // 1秒保留时间
            100, // 最大100条
        );
        
        // 标记一些消息
        manager.mark_as_processed("msg1");
        manager.mark_as_processed("msg2");
        
        let (count_before, _) = manager.get_stats();
        assert!(count_before >= 2);
        
        // 等待超过保留时间
        thread::sleep(StdDuration::from_secs(2));
        
        // 清理过期记录
        manager.cleanup_expired();
        
        let (count_after, _) = manager.get_stats();
        assert_eq!(count_after, 0);
    }
} 