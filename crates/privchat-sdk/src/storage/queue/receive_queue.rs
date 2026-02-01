use crate::storage::queue::receive_task::{
    ReceiveTask, ReceiveTaskBatch, DeduplicationManager
};
use crate::storage::queue::{TaskQueueTrait, QueueStats};
use crate::error::{PrivchatSDKError, Result};

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn, instrument};
use serde::{Deserialize, Serialize};

/// 接收队列配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiveQueueConfig {
    /// 批次最大大小
    pub batch_max_size: usize,
    
    /// 批次超时时间（秒）
    pub batch_timeout_seconds: u64,
    
    /// 最大并发批次数
    pub max_concurrent_batches: usize,
    
    /// 去重缓存大小
    pub dedup_cache_size: usize,
    
    /// 队列最大大小
    pub max_queue_size: usize,
    
    /// 实时消息优先级
    pub realtime_priority: bool,
    
    /// 批量处理延迟（毫秒）
    pub batch_process_delay_ms: u64,
}

impl Default for ReceiveQueueConfig {
    fn default() -> Self {
        Self {
            batch_max_size: 50,
            batch_timeout_seconds: 5,
            max_concurrent_batches: 10,
            dedup_cache_size: 10000,
            max_queue_size: 50000,
            realtime_priority: true,
            batch_process_delay_ms: 100,
        }
    }
}

/// 接收队列统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiveQueueStats {
    /// 总接收任务数
    pub total_received: u64,
    
    /// 处理成功数
    pub processed_success: u64,
    
    /// 处理失败数
    pub processed_failed: u64,
    
    /// 跳过的重复消息数
    pub duplicates_skipped: u64,
    
    /// 当前队列大小
    pub current_queue_size: usize,
    
    /// 当前批次数
    pub current_batches: usize,
    
    /// 平均批次大小
    pub average_batch_size: f64,
    
    /// 平均处理延迟（毫秒）
    pub average_processing_delay_ms: f64,
    
    /// 去重缓存命中率
    pub dedup_hit_rate: f64,
}

impl ReceiveQueueStats {
    /// 计算成功率
    pub fn success_rate(&self) -> f64 {
        let total_processed = self.processed_success + self.processed_failed;
        if total_processed == 0 {
            0.0
        } else {
            self.processed_success as f64 / total_processed as f64
        }
    }

    /// 更新平均值
    pub fn update_averages(&mut self, batch_size: usize, processing_delay_ms: u64) {
        // 简单的移动平均
        self.average_batch_size = (self.average_batch_size * 0.9) + (batch_size as f64 * 0.1);
        self.average_processing_delay_ms = (self.average_processing_delay_ms * 0.9) + (processing_delay_ms as f64 * 0.1);
    }
}

/// 接收队列管理器
#[derive(Debug)]
pub struct ReceiveQueueManager {
    config: ReceiveQueueConfig,
    
    /// 待处理的任务队列
    pending_tasks: Arc<Mutex<VecDeque<ReceiveTask>>>,
    
    /// 按会话分组的批次
    channel_batches: Arc<RwLock<HashMap<String, ReceiveTaskBatch>>>,
    
    /// 准备好的批次队列
    ready_batches: Arc<Mutex<VecDeque<ReceiveTaskBatch>>>,
    
    /// 去重管理器
    dedup_manager: Arc<Mutex<DeduplicationManager>>,
    
    /// 统计信息
    stats: Arc<RwLock<ReceiveQueueStats>>,
}

impl ReceiveQueueManager {
    /// 创建新的接收队列管理器
    pub fn new(config: ReceiveQueueConfig) -> Self {
        let dedup_manager = DeduplicationManager::new(config.dedup_cache_size);
        
        Self {
            config,
            pending_tasks: Arc::new(Mutex::new(VecDeque::new())),
            channel_batches: Arc::new(RwLock::new(HashMap::new())),
            ready_batches: Arc::new(Mutex::new(VecDeque::new())),
            dedup_manager: Arc::new(Mutex::new(dedup_manager)),
            stats: Arc::new(RwLock::new(ReceiveQueueStats::default())),
        }
    }

    /// 添加接收任务
    #[instrument(skip(self, task), fields(task_id = %task.task_id))]
    pub async fn enqueue(&self, task: ReceiveTask) -> Result<()> {
        // 检查队列大小限制
        {
            let pending = self.pending_tasks.lock().await;
            if pending.len() >= self.config.max_queue_size {
                return Err(PrivchatSDKError::QueueFull(
                    format!("Receive queue full: {}", self.config.max_queue_size)
                ));
            }
        }

        // 检查重复
        {
            let mut dedup = self.dedup_manager.lock().await;
            if dedup.is_duplicate(&task) {
                self.handle_duplicate_task(task).await;
                return Ok(());
            }
            // 标记为已处理（防止重复）
            dedup.mark_processed(&task);
        }

        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_received += 1;
        }

        // 根据优先级决定处理方式
        if self.config.realtime_priority && task.needs_realtime_processing() {
            self.handle_realtime_task(task).await?;
        } else {
            self.handle_batch_task(task).await?;
        }

        Ok(())
    }

    /// 处理实时任务
    async fn handle_realtime_task(&self, task: ReceiveTask) -> Result<()> {
        debug!("Handling realtime task: {}", task.task_id);
        
        // 实时任务直接加入待处理队列
        let mut pending = self.pending_tasks.lock().await;
        pending.push_front(task); // 实时任务优先处理
        
        Ok(())
    }

    /// 处理批量任务
    async fn handle_batch_task(&self, task: ReceiveTask) -> Result<()> {
        debug!("Handling batch task: {}", task.task_id);
        
        let channel_key = task.channel_key();
        
        // 第一阶段：尝试添加任务到现有批次
        let (task_added, ready_batch_opt) = {
            let mut batches = self.channel_batches.write().await;
            
            // 获取或创建批次
            let batch = batches.entry(channel_key.clone()).or_insert_with(|| {
                ReceiveTaskBatch::new(
                    channel_key.clone(),
                    self.config.batch_max_size,
                    self.config.batch_timeout_seconds,
                )
            });

            // 尝试添加任务到批次
            match batch.add_task(task.clone()) {
                Ok(()) => {
                    // 任务添加成功，检查批次是否准备好
                    if batch.is_ready() {
                        let ready_batch = batches.remove(&channel_key).unwrap();
                        (true, Some(ready_batch))
                    } else {
                        (true, None)
                    }
                }
                Err(_) => {
                    // 批次已满，需要移除并处理
                    let full_batch = batches.remove(&channel_key).unwrap();
                    (false, Some(full_batch))
                }
            }
        };
        
        // 第二阶段：如果任务未添加，创建新批次
        if !task_added {
            let mut batches = self.channel_batches.write().await;
            let mut new_batch = ReceiveTaskBatch::new(
                channel_key.clone(),
                self.config.batch_max_size,
                self.config.batch_timeout_seconds,
            );
            
            new_batch.add_task(task)?;
            batches.insert(channel_key, new_batch);
        }
        
        // 第三阶段：处理准备好的批次
        if let Some(ready_batch) = ready_batch_opt {
            self.move_batch_to_ready(ready_batch).await;
        }

        Ok(())
    }

    /// 将批次移动到准备队列
    async fn move_batch_to_ready(&self, mut batch: ReceiveTaskBatch) {
        // 按序列号排序
        batch.sort_by_sequence();
        
        info!("Batch {} ready for processing with {} tasks", 
              batch.batch_id, batch.size());
        
        let mut ready = self.ready_batches.lock().await;
        ready.push_back(batch);
    }

    /// 处理重复任务
    async fn handle_duplicate_task(&self, mut task: ReceiveTask) {
        warn!("Duplicate task detected: {}", task.task_id);
        
        task.mark_skipped();
        
        // 更新统计
        let mut stats = self.stats.write().await;
        stats.duplicates_skipped += 1;
        stats.total_received += 1;
    }

    /// 获取下一个待处理任务
    pub async fn dequeue(&self) -> Result<Option<ReceiveTask>> {
        // 优先处理实时任务
        {
            let mut pending = self.pending_tasks.lock().await;
            if let Some(task) = pending.pop_front() {
                return Ok(Some(task));
            }
        }

        // 处理批次任务
        {
            let mut ready = self.ready_batches.lock().await;
            if let Some(mut batch) = ready.pop_front() {
                if let Some(task) = batch.tasks.pop() {
                    // 如果批次还有任务，放回队列
                    if !batch.tasks.is_empty() {
                        ready.push_front(batch);
                    }
                    return Ok(Some(task));
                }
            }
        }

        Ok(None)
    }

    /// 批量获取任务
    pub async fn batch_dequeue(&self, limit: usize) -> Result<Vec<ReceiveTask>> {
        let mut tasks = Vec::new();
        
        // 优先获取实时任务
        {
            let mut pending = self.pending_tasks.lock().await;
            while tasks.len() < limit && !pending.is_empty() {
                if let Some(task) = pending.pop_front() {
                    tasks.push(task);
                }
            }
        }

        // 获取批次任务
        if tasks.len() < limit {
            let mut ready = self.ready_batches.lock().await;
            while tasks.len() < limit && !ready.is_empty() {
                if let Some(mut batch) = ready.pop_front() {
                    while tasks.len() < limit && !batch.tasks.is_empty() {
                        if let Some(task) = batch.tasks.pop() {
                            tasks.push(task);
                        }
                    }
                    
                    // 如果批次还有任务，放回队列
                    if !batch.tasks.is_empty() {
                        ready.push_front(batch);
                    }
                }
            }
        }

        Ok(tasks)
    }

    /// 标记任务完成
    pub async fn mark_completed(&self, task: &ReceiveTask) -> Result<()> {
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.processed_success += 1;
            
            if let Some(delay) = task.processing_delay_ms() {
                stats.update_averages(1, delay);
            }
        }

        Ok(())
    }

    /// 标记任务失败
    pub async fn mark_failed(&self, _task: &ReceiveTask) -> Result<()> {
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.processed_failed += 1;
        }

        Ok(())
    }

    /// 检查并处理超时批次
    pub async fn process_timeout_batches(&self) -> Result<usize> {
        let mut processed_count = 0;
        let mut timeout_batches = Vec::new();
        
        // 检查超时批次
        {
            let mut batches = self.channel_batches.write().await;
            let keys_to_remove: Vec<String> = batches
                .iter()
                .filter(|(_, batch)| batch.is_timeout())
                .map(|(key, _)| key.clone())
                .collect();
            
            for key in keys_to_remove {
                if let Some(batch) = batches.remove(&key) {
                    timeout_batches.push(batch);
                }
            }
        }

        // 处理超时批次
        for batch in timeout_batches {
            info!("Processing timeout batch: {} with {} tasks", 
                  batch.batch_id, batch.size());
            
            self.move_batch_to_ready(batch).await;
            processed_count += 1;
        }

        Ok(processed_count)
    }

    /// 获取队列统计
    pub async fn stats(&self) -> ReceiveQueueStats {
        let mut stats = self.stats.read().await.clone();
        
        // 更新当前状态
        stats.current_queue_size = self.pending_tasks.lock().await.len();
        stats.current_batches = self.channel_batches.read().await.len();
        
        // 计算去重命中率
        let (_cache_size, _) = self.dedup_manager.lock().await.stats();
        if stats.total_received > 0 {
            stats.dedup_hit_rate = stats.duplicates_skipped as f64 / stats.total_received as f64;
        }

        stats
    }

    /// 清空队列
    pub async fn clear(&self) -> Result<()> {
        self.pending_tasks.lock().await.clear();
        self.channel_batches.write().await.clear();
        self.ready_batches.lock().await.clear();
        self.dedup_manager.lock().await.clear();
        
        // 重置统计
        {
            let mut stats = self.stats.write().await;
            *stats = ReceiveQueueStats::default();
        }

        info!("Receive queue cleared");
        Ok(())
    }

    /// 获取队列大小
    pub async fn size(&self) -> usize {
        let pending_size = self.pending_tasks.lock().await.len();
        let batch_size: usize = self.channel_batches.read().await
            .values()
            .map(|batch| batch.size())
            .sum();
        let ready_size: usize = self.ready_batches.lock().await
            .iter()
            .map(|batch| batch.size())
            .sum();
        
        pending_size + batch_size + ready_size
    }

    /// 检查队列是否为空
    pub async fn is_empty(&self) -> bool {
        self.size().await == 0
    }
}

#[async_trait::async_trait]
impl TaskQueueTrait for ReceiveQueueManager {
    async fn push(&self, _task: crate::storage::queue::send_task::SendTask) -> Result<()> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support SendTask".to_string()
        ))
    }

    async fn pop(&self) -> Result<Option<crate::storage::queue::send_task::SendTask>> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support SendTask".to_string()
        ))
    }

    async fn batch_push(&self, _tasks: Vec<crate::storage::queue::send_task::SendTask>) -> Result<()> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support SendTask".to_string()
        ))
    }

    async fn batch_pop(&self, _limit: usize) -> Result<Vec<crate::storage::queue::send_task::SendTask>> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support SendTask".to_string()
        ))
    }

    async fn remove(&self, _task_id: &str) -> Result<()> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support task removal by ID".to_string()
        ))
    }

    async fn clear(&self) -> Result<()> {
        self.clear().await
    }

    async fn size(&self) -> Result<usize> {
        Ok(self.size().await)
    }

    async fn is_empty(&self) -> Result<bool> {
        Ok(self.is_empty().await)
    }

    async fn stats(&self) -> Result<QueueStats> {
        let receive_stats = self.stats().await;
        
        Ok(QueueStats {
            total_tasks: receive_stats.total_received as usize,
            pending_tasks: receive_stats.current_queue_size,
            processing_tasks: 0,
            completed_tasks: receive_stats.processed_success as usize,
            failed_tasks: receive_stats.processed_failed as usize,
            expired_tasks: 0,
            avg_processing_time_ms: receive_stats.average_processing_delay_ms,
            priority_distribution: HashMap::new(),
        })
    }

    async fn enqueue(&self, _task: crate::storage::queue::send_task::SendTask) -> Result<()> {
        Err(PrivchatSDKError::InvalidOperation(
            "ReceiveQueueManager doesn't support SendTask enqueue".to_string()
        ))
    }


}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::{MessageData, receive_task::MessageSource};

    fn create_test_message_data(channel_id: u64) -> MessageData {
        MessageData {
            id: channel_id as i64,
            channel_id,
            channel_type: 1,
            from_uid: 123,
            message_type: 1,
            content: "Test message".to_string(),
            extra: HashMap::new(),
            created_at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            expires_at: None,
            local_message_id: 0,
        }
    }

    fn create_test_task(channel_id: u64, seq: u64, server_msg_id: u64) -> ReceiveTask {
        let message_data = create_test_message_data(channel_id);
        ReceiveTask::new(
            message_data,
            seq,
            server_msg_id,
            MessageSource::RealTime,
        )
    }

    #[tokio::test]
    async fn test_receive_queue_basic_operations() {
        let config = ReceiveQueueConfig::default();
        let queue = ReceiveQueueManager::new(config);

        // 测试入队
        let task = create_test_task(1001, 1, 2001);
        queue.enqueue(task).await.unwrap();

        // 测试出队
        let dequeued = queue.dequeue().await.unwrap();
        assert!(dequeued.is_some());
        
        let task = dequeued.unwrap();
        assert_eq!(task.sequence_id, 1);
        assert_eq!(task.server_msg_id, 2001);
    }

    #[tokio::test]
    async fn test_deduplication() {
        let config = ReceiveQueueConfig::default();
        let queue = ReceiveQueueManager::new(config);

        // 添加第一个任务
        let task1 = create_test_task(1001, 1, 2001);
        queue.enqueue(task1).await.unwrap();

        // 添加重复任务
        let task2 = create_test_task(1001, 1, 2001);
        queue.enqueue(task2).await.unwrap();

        // 应该只能出队一个任务
        let dequeued1 = queue.dequeue().await.unwrap();
        assert!(dequeued1.is_some());

        let dequeued2 = queue.dequeue().await.unwrap();
        assert!(dequeued2.is_none());

        // 检查统计
        let stats = queue.stats().await;
        assert_eq!(stats.total_received, 2);
        assert_eq!(stats.duplicates_skipped, 1);
    }

    #[tokio::test]
    async fn test_batch_processing() {
        let mut config = ReceiveQueueConfig::default();
        config.batch_max_size = 3;
        config.realtime_priority = false; // 禁用实时优先级
        
        let queue = ReceiveQueueManager::new(config);

        // 添加同一会话的多个任务
        for i in 1..=5 {
            let mut task = create_test_task(1001, i, 2000 + i);
            task.source = MessageSource::Historical; // 历史消息，会被批量处理
            queue.enqueue(task).await.unwrap();
        }

        // 检查批次处理
        let stats = queue.stats().await;
        assert_eq!(stats.total_received, 5);

        // 应该能够批量出队
        let tasks = queue.batch_dequeue(10).await.unwrap();
        assert!(!tasks.is_empty());
    }

    #[tokio::test]
    async fn test_realtime_priority() {
        let config = ReceiveQueueConfig::default();
        let queue = ReceiveQueueManager::new(config);

        // 添加历史消息
        let mut historical_task = create_test_task(1001, 1, 2001);
        historical_task.source = MessageSource::Historical;
        queue.enqueue(historical_task).await.unwrap();

        // 添加实时消息
        let realtime_task = create_test_task(1001, 2, 2002);
        queue.enqueue(realtime_task).await.unwrap();

        // 实时消息应该优先出队
        let dequeued = queue.dequeue().await.unwrap().unwrap();
        assert_eq!(dequeued.sequence_id, 2); // 实时消息
        assert_eq!(dequeued.source, MessageSource::RealTime);
    }

    #[tokio::test]
    async fn test_queue_stats() {
        let config = ReceiveQueueConfig::default();
        let queue = ReceiveQueueManager::new(config);

        // 添加一些任务
        for i in 1..=3 {
            let task = create_test_task(1001, i, 2000 + i);
            queue.enqueue(task).await.unwrap();
        }

        let stats = queue.stats().await;
        assert_eq!(stats.total_received, 3);
        assert!(stats.current_queue_size > 0);
    }

    #[tokio::test]
    async fn test_queue_clear() {
        let config = ReceiveQueueConfig::default();
        let queue = ReceiveQueueManager::new(config);

        // 添加任务
        let task = create_test_task(1001, 1, 2001);
        queue.enqueue(task).await.unwrap();

        assert!(!queue.is_empty().await);

        // 清空队列
        queue.clear().await.unwrap();
        assert!(queue.is_empty().await);

        let stats = queue.stats().await;
        assert_eq!(stats.total_received, 0);
    }
} 