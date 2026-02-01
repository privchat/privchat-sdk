use crate::storage::queue::MessageData;
use crate::error::{PrivchatSDKError, Result};

use std::time::{SystemTime, UNIX_EPOCH};
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// 接收任务状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiveTaskStatus {
    /// 待处理
    Pending,
    /// 处理中
    Processing,
    /// 已完成
    Completed,
    /// 失败
    Failed,
    /// 已跳过（重复消息）
    Skipped,
}

impl Default for ReceiveTaskStatus {
    fn default() -> Self {
        ReceiveTaskStatus::Pending
    }
}

/// 消息来源类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageSource {
    /// 实时推送
    RealTime,
    /// 历史同步
    Historical,
    /// 离线拉取
    Offline,
    /// 重连后补偿
    Reconnect,
}

impl Default for MessageSource {
    fn default() -> Self {
        MessageSource::RealTime
    }
}

/// 接收任务
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiveTask {
    /// 任务ID（用于去重和跟踪）
    pub task_id: String,
    
    /// 消息数据
    pub message_data: MessageData,
    
    /// 消息序列号（用于去重）
    pub sequence_id: u64,
    
    /// 服务端消息ID
    pub server_msg_id: u64,  // u64，与服务端一致
    
    /// 消息来源
    pub source: MessageSource,
    
    /// 任务状态
    pub status: ReceiveTaskStatus,
    
    /// 创建时间
    pub created_at: u64,
    
    /// 处理时间
    pub processed_at: Option<u64>,
    
    /// 重试次数
    pub retry_count: u32,
    
    /// 失败原因
    pub failure_reason: Option<String>,
    
    /// 是否需要发送已读回执
    pub need_read_receipt: bool,
    
    /// 批次ID（用于批量处理）
    pub batch_id: Option<String>,
}

impl ReceiveTask {
    /// 创建新的接收任务
    pub fn new(
        message_data: MessageData,
        sequence_id: u64,
        server_msg_id: u64,
        source: MessageSource,
    ) -> Self {
        let task_id = Self::generate_task_id(server_msg_id, sequence_id);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            task_id,
            message_data,
            sequence_id,
            server_msg_id,
            source,
            status: ReceiveTaskStatus::Pending,
            created_at: now,
            processed_at: None,
            retry_count: 0,
            failure_reason: None,
            need_read_receipt: true,
            batch_id: None,
        }
    }

    /// 生成任务ID
    fn generate_task_id(server_msg_id: u64, sequence_id: u64) -> String {
        format!("recv_{}_{}", server_msg_id, sequence_id)
    }

    /// 获取去重键
    pub fn dedup_key(&self) -> String {
        format!("{}_{}", self.server_msg_id, self.sequence_id)
    }

    /// 获取会话键（用于批量处理）
    pub fn channel_key(&self) -> String {
        format!("{}_{}", 
            self.message_data.channel_id, 
            self.message_data.channel_type
        )
    }

    /// 标记为处理中
    pub fn mark_processing(&mut self) {
        self.status = ReceiveTaskStatus::Processing;
        debug!("Task {} marked as processing", self.task_id);
    }

    /// 标记为已完成
    pub fn mark_completed(&mut self) {
        self.status = ReceiveTaskStatus::Completed;
        self.processed_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );
        info!("Task {} completed successfully", self.task_id);
    }

    /// 标记为失败
    pub fn mark_failed(&mut self, reason: String) {
        self.status = ReceiveTaskStatus::Failed;
        self.failure_reason = Some(reason.clone());
        self.processed_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );
        warn!("Task {} failed: {}", self.task_id, reason);
    }

    /// 标记为跳过（重复消息）
    pub fn mark_skipped(&mut self) {
        self.status = ReceiveTaskStatus::Skipped;
        self.processed_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );
        debug!("Task {} skipped (duplicate)", self.task_id);
    }

    /// 增加重试次数
    pub fn increment_retry(&mut self) {
        self.retry_count += 1;
        debug!("Task {} retry count increased to {}", self.task_id, self.retry_count);
    }

    /// 设置批次ID
    pub fn set_batch_id(&mut self, batch_id: String) {
        self.batch_id = Some(batch_id);
    }

    /// 是否可以重试
    pub fn can_retry(&self, max_retries: u32) -> bool {
        self.retry_count < max_retries && 
        matches!(self.status, ReceiveTaskStatus::Failed)
    }

    /// 获取处理延迟（毫秒）
    pub fn processing_delay_ms(&self) -> Option<u64> {
        self.processed_at.map(|processed| {
            (processed - self.created_at) * 1000
        })
    }

    /// 是否是历史消息
    pub fn is_historical(&self) -> bool {
        matches!(self.source, MessageSource::Historical | MessageSource::Offline)
    }

    /// 是否需要实时处理
    pub fn needs_realtime_processing(&self) -> bool {
        matches!(self.source, MessageSource::RealTime | MessageSource::Reconnect)
    }
}

/// 接收任务批次
#[derive(Debug, Clone)]
pub struct ReceiveTaskBatch {
    /// 批次ID
    pub batch_id: String,
    
    /// 会话键
    pub channel_key: String,
    
    /// 批次中的任务
    pub tasks: Vec<ReceiveTask>,
    
    /// 批次创建时间
    pub created_at: u64,
    
    /// 批次大小限制
    pub max_size: usize,
    
    /// 批次超时时间（秒）
    pub timeout_seconds: u64,
}

impl ReceiveTaskBatch {
    /// 创建新批次
    pub fn new(channel_key: String, max_size: usize, timeout_seconds: u64) -> Self {
        let batch_id = format!("batch_{}_{}", 
            channel_key,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            batch_id,
            channel_key,
            tasks: Vec::new(),
            created_at: now,
            max_size,
            timeout_seconds,
        }
    }

    /// 添加任务到批次
    pub fn add_task(&mut self, mut task: ReceiveTask) -> Result<()> {
        if self.is_full() {
            return Err(PrivchatSDKError::QueueFull("Batch is full".to_string()));
        }

        if task.channel_key() != self.channel_key {
            return Err(PrivchatSDKError::InvalidInput(
                "Task channel key doesn't match batch".to_string()
            ));
        }

        task.set_batch_id(self.batch_id.clone());
        self.tasks.push(task);
        Ok(())
    }

    /// 批次是否已满
    pub fn is_full(&self) -> bool {
        self.tasks.len() >= self.max_size
    }

    /// 批次是否超时
    pub fn is_timeout(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        (now - self.created_at) >= self.timeout_seconds
    }

    /// 批次是否准备好处理
    pub fn is_ready(&self) -> bool {
        !self.tasks.is_empty() && (self.is_full() || self.is_timeout())
    }

    /// 获取批次大小
    pub fn size(&self) -> usize {
        self.tasks.len()
    }

    /// 获取批次年龄（秒）
    pub fn age_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        now - self.created_at
    }

    /// 按序列号排序任务
    pub fn sort_by_sequence(&mut self) {
        self.tasks.sort_by_key(|task| task.sequence_id);
    }
}

/// 去重管理器
#[derive(Debug)]
pub struct DeduplicationManager {
    /// 已处理的消息键
    processed_keys: HashMap<String, u64>,
    
    /// 最大缓存大小
    max_cache_size: usize,
    
    /// 清理阈值
    cleanup_threshold: usize,
}

impl DeduplicationManager {
    /// 创建新的去重管理器
    pub fn new(max_cache_size: usize) -> Self {
        Self {
            processed_keys: HashMap::new(),
            max_cache_size,
            cleanup_threshold: max_cache_size * 3 / 4, // 75% 阈值
        }
    }

    /// 检查消息是否重复
    pub fn is_duplicate(&self, task: &ReceiveTask) -> bool {
        let key = task.dedup_key();
        self.processed_keys.contains_key(&key)
    }

    /// 标记消息为已处理
    pub fn mark_processed(&mut self, task: &ReceiveTask) {
        let key = task.dedup_key();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        self.processed_keys.insert(key, now);
        
        // 检查是否需要清理
        if self.processed_keys.len() > self.max_cache_size {
            self.cleanup_old_entries();
        }
    }

    /// 清理旧的条目
    fn cleanup_old_entries(&mut self) {
        if self.processed_keys.len() <= self.cleanup_threshold {
            return;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // 保留最近1小时的记录
        let cutoff_time = now.saturating_sub(3600);
        
        self.processed_keys.retain(|_, &mut timestamp| {
            timestamp >= cutoff_time
        });

        info!("Cleaned up deduplication cache, {} entries remaining", 
              self.processed_keys.len());
    }

    /// 获取缓存统计
    pub fn stats(&self) -> (usize, usize) {
        (self.processed_keys.len(), self.max_cache_size)
    }

    /// 清空缓存
    pub fn clear(&mut self) {
        self.processed_keys.clear();
        info!("Deduplication cache cleared");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_message_data() -> MessageData {
        MessageData {
            id: 123,
            channel_id: 456,
            channel_type: 1,
            from_uid: 123,
            message_type: 1,
            content: "Hello World".to_string(),
            extra: HashMap::new(),
            created_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            expires_at: None,
            local_message_id: 0,
        }
    }

    #[test]
    fn test_receive_task_creation() {
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data.clone(),
            12345,
            789,
            MessageSource::RealTime,
        );

        assert_eq!(task.sequence_id, 12345);
        assert_eq!(task.server_msg_id, 789);
        assert_eq!(task.source, MessageSource::RealTime);
        assert_eq!(task.status, ReceiveTaskStatus::Pending);
        assert_eq!(task.retry_count, 0);
        assert!(task.need_read_receipt);
    }

    #[test]
    fn test_task_dedup_key() {
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        assert_eq!(task.dedup_key(), "789_12345");
    }

    #[test]
    fn test_task_channel_key() {
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        assert_eq!(task.channel_key(), "456_1");
    }

    #[test]
    fn test_task_status_transitions() {
        let message_data = create_test_message_data();
        let mut task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        // 初始状态
        assert_eq!(task.status, ReceiveTaskStatus::Pending);

        // 标记为处理中
        task.mark_processing();
        assert_eq!(task.status, ReceiveTaskStatus::Processing);

        // 标记为完成
        task.mark_completed();
        assert_eq!(task.status, ReceiveTaskStatus::Completed);
        assert!(task.processed_at.is_some());
    }

    #[test]
    fn test_receive_task_batch() {
        let mut batch = ReceiveTaskBatch::new(
            "456_1".to_string(),
            5,
            30,
        );

        assert_eq!(batch.channel_key, "456_1");
        assert_eq!(batch.max_size, 5);
        assert_eq!(batch.size(), 0);
        assert!(!batch.is_full());
        assert!(!batch.is_ready());

        // 添加任务
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        batch.add_task(task).unwrap();
        assert_eq!(batch.size(), 1);
        assert!(!batch.is_full());
    }

    #[test]
    fn test_deduplication_manager() {
        let mut dedup = DeduplicationManager::new(100);
        
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        // 首次检查，不重复
        assert!(!dedup.is_duplicate(&task));

        // 标记为已处理
        dedup.mark_processed(&task);

        // 再次检查，重复
        assert!(dedup.is_duplicate(&task));

        let (cache_size, max_size) = dedup.stats();
        assert_eq!(cache_size, 1);
        assert_eq!(max_size, 100);
    }

    #[test]
    fn test_task_retry_logic() {
        let message_data = create_test_message_data();
        let mut task = ReceiveTask::new(
            message_data,
            12345,
            789,
            MessageSource::RealTime,
        );

        // 初始状态不能重试
        assert!(!task.can_retry(3));

        // 标记失败后可以重试
        task.mark_failed("Test failure".to_string());
        assert!(task.can_retry(3));

        // 增加重试次数
        task.increment_retry();
        assert_eq!(task.retry_count, 1);
        assert!(task.can_retry(3));

        // 达到最大重试次数
        task.retry_count = 3;
        assert!(!task.can_retry(3));
    }

    #[test]
    fn test_message_source_helpers() {
        let message_data = create_test_message_data();
        
        let realtime_task = ReceiveTask::new(
            message_data.clone(),
            12345,
            789,
            MessageSource::RealTime,
        );
        assert!(realtime_task.needs_realtime_processing());
        assert!(!realtime_task.is_historical());

        let historical_task = ReceiveTask::new(
            message_data,
            12346,
            790,
            MessageSource::Historical,
        );
        assert!(!historical_task.needs_realtime_processing());
        assert!(historical_task.is_historical());
    }
} 