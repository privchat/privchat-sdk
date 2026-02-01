use crate::storage::queue::receive_task::ReceiveTask;
use crate::storage::queue::receive_queue::ReceiveQueueManager;
use crate::storage::message_state::MessageStateManager;
use crate::storage::dao::message::MessageDao;
use crate::storage::entities::{Message, MessageStatus};
use crate::error::{PrivchatSDKError, Result};

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, Notify};
use tokio::time::{sleep, timeout};
use tokio::select;
use tracing::{debug, error, info, warn, instrument};
use serde::{Deserialize, Serialize};
use rusqlite::Connection;

/// 处理单个任务的结果
#[allow(dead_code)]
enum ProcessResult {
    Success(Message),
    Skipped,
}

/// 接收消费者配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiveConsumerConfig {
    /// 工作线程数量
    pub worker_count: usize,
    
    /// 批量拉取任务数量
    pub batch_size: usize,
    
    /// 拉取任务间隔（毫秒）
    pub poll_interval_ms: u64,
    
    /// 数据库批量写入大小
    pub db_batch_size: usize,
    
    /// 数据库写入超时时间（秒）
    pub db_timeout_seconds: u64,
    
    /// 最大重试次数
    pub max_retries: u32,
    
    /// 重试延迟（毫秒）
    pub retry_delay_ms: u64,
    
    /// 超时批次检查间隔（秒）
    pub timeout_check_interval_seconds: u64,
}

impl Default for ReceiveConsumerConfig {
    fn default() -> Self {
        Self {
            worker_count: 2,
            batch_size: 20,
            poll_interval_ms: 100,
            db_batch_size: 50,
            db_timeout_seconds: 30,
            max_retries: 3,
            retry_delay_ms: 1000,
            timeout_check_interval_seconds: 5,
        }
    }
}

/// 接收消费者统计
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReceiveConsumerStats {
    /// 处理的任务总数
    pub tasks_processed: u64,
    
    /// 处理成功数
    pub tasks_success: u64,
    
    /// 处理失败数
    pub tasks_failed: u64,
    
    /// 跳过的重复任务数
    pub tasks_skipped: u64,
    
    /// 数据库写入次数
    pub db_writes: u64,
    
    /// 数据库写入失败次数
    pub db_write_failures: u64,
    
    /// 平均处理时间（毫秒）
    pub average_processing_time_ms: f64,
    
    /// 平均批次大小
    pub average_batch_size: f64,
    
    /// 当前活跃工作线程数
    pub active_workers: usize,
}

impl ReceiveConsumerStats {
    /// 计算成功率
    pub fn success_rate(&self) -> f64 {
        if self.tasks_processed == 0 {
            0.0
        } else {
            self.tasks_success as f64 / self.tasks_processed as f64
        }
    }

    /// 更新平均值
    pub fn update_averages(&mut self, processing_time_ms: u64, batch_size: usize) {
        // 简单的移动平均
        self.average_processing_time_ms = (self.average_processing_time_ms * 0.9) + (processing_time_ms as f64 * 0.1);
        self.average_batch_size = (self.average_batch_size * 0.9) + (batch_size as f64 * 0.1);
    }
}

/// 接收消费者运行器
pub struct ReceiveConsumerRunner {
    config: ReceiveConsumerConfig,
    queue_manager: Arc<ReceiveQueueManager>,
    state_manager: Arc<MessageStateManager>,
    db_connection: Arc<Mutex<Connection>>,
    
    /// 统计信息
    stats: Arc<RwLock<ReceiveConsumerStats>>,
    
    /// 控制信号
    shutdown_signal: Arc<Notify>,
    is_running: Arc<RwLock<bool>>,
}

impl ReceiveConsumerRunner {
    /// 创建新的接收消费者
    pub fn new(
        config: ReceiveConsumerConfig,
        queue_manager: Arc<ReceiveQueueManager>,
        state_manager: Arc<MessageStateManager>,
        db_connection: Arc<Mutex<Connection>>,
    ) -> Self {
        Self {
            config,
            queue_manager,
            state_manager,
            db_connection,
            stats: Arc::new(RwLock::new(ReceiveConsumerStats::default())),
            shutdown_signal: Arc::new(Notify::new()),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// 启动消费者
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        let mut is_running = self.is_running.write().await;
        if *is_running {
            return Err(PrivchatSDKError::InvalidOperation(
                "Consumer is already running".to_string()
            ));
        }

        *is_running = true;
        drop(is_running);

        info!("Starting receive consumer with {} workers", self.config.worker_count);

        // 启动工作线程
        for worker_id in 0..self.config.worker_count {
            self.start_worker(worker_id).await;
        }

        // 启动超时批次处理器
        self.start_timeout_processor().await;

        info!("Receive consumer started successfully");
        Ok(())
    }

    /// 停止消费者
    #[instrument(skip(self))]
    pub async fn stop(&self) -> Result<()> {
        let mut is_running = self.is_running.write().await;
        if !*is_running {
            return Ok(());
        }

        info!("Stopping receive consumer...");
        
        *is_running = false;
        self.shutdown_signal.notify_waiters();
        
        // 等待一段时间让工作线程完成当前任务
        sleep(Duration::from_millis(500)).await;
        
        info!("Receive consumer stopped");
        Ok(())
    }

    /// 启动工作线程
    async fn start_worker(&self, worker_id: usize) {
        let queue_manager = self.queue_manager.clone();
        let state_manager = self.state_manager.clone();
        let db_connection = self.db_connection.clone();
        let stats = self.stats.clone();
        let shutdown_signal = self.shutdown_signal.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            info!("Worker {} started", worker_id);

            loop {
                select! {
                    _ = shutdown_signal.notified() => {
                        info!("Worker {} received shutdown signal", worker_id);
                        break;
                    }
                    _ = sleep(Duration::from_millis(config.poll_interval_ms)) => {
                        if !*is_running.read().await {
                            break;
                        }

                        if let Err(e) = Self::process_tasks(
                            worker_id,
                            &queue_manager,
                            &state_manager,
                            &db_connection,
                            &stats,
                            &config,
                        ).await {
                            error!("Worker {} error: {}", worker_id, e);
                        }
                    }
                }
            }

            info!("Worker {} stopped", worker_id);
        });
    }

    /// 处理任务
    async fn process_tasks(
        worker_id: usize,
        queue_manager: &ReceiveQueueManager,
        state_manager: &MessageStateManager,
        db_connection: &Arc<Mutex<Connection>>,
        stats: &Arc<RwLock<ReceiveConsumerStats>>,
        config: &ReceiveConsumerConfig,
    ) -> Result<()> {
        // 批量获取任务
        let tasks = queue_manager.batch_dequeue(config.batch_size).await?;
        if tasks.is_empty() {
            return Ok(());
        }

        let start_time = SystemTime::now();
        let batch_size = tasks.len();

        debug!("Worker {} processing {} tasks", worker_id, batch_size);

        // 更新活跃工作线程数
        {
            let mut s = stats.write().await;
            s.active_workers += 1;
        }

        let result = Self::process_task_batch(
            worker_id,
            tasks,
            state_manager,
            db_connection,
            config,
        ).await;

        // 更新统计
        {
            let mut s = stats.write().await;
            s.active_workers = s.active_workers.saturating_sub(1);
            
            if let Ok((success_count, failed_count, skipped_count)) = &result {
                s.tasks_processed += batch_size as u64;
                s.tasks_success += success_count;
                s.tasks_failed += failed_count;
                s.tasks_skipped += skipped_count;
                
                if *success_count > 0 {
                    s.db_writes += 1;
                }
            }

            let processing_time = start_time.elapsed().unwrap_or_default().as_millis() as u64;
            s.update_averages(processing_time, batch_size);
        }

        result.map(|_| ())
    }

    /// 处理任务批次
    async fn process_task_batch(
        worker_id: usize,
        tasks: Vec<ReceiveTask>,
        state_manager: &MessageStateManager,
        db_connection: &Arc<Mutex<Connection>>,
        config: &ReceiveConsumerConfig,
    ) -> Result<(u64, u64, u64)> {
        let mut success_count = 0u64;
        let mut failed_count = 0u64;
        let mut skipped_count = 0u64;
        let mut messages_to_save = Vec::new();

        // 处理每个任务
        for mut task in tasks {
            match Self::process_single_task(&mut task, state_manager).await {
                Ok(ProcessResult::Success(message)) => {
                    messages_to_save.push(message);
                    success_count += 1;
                }
                Ok(ProcessResult::Skipped) => {
                    skipped_count += 1;
                }
                Err(e) => {
                    warn!("Worker {} failed to process task {}: {}", 
                          worker_id, task.task_id, e);
                    failed_count += 1;
                    
                    // 重试逻辑
                    if task.can_retry(config.max_retries) {
                        task.increment_retry();
                        task.mark_failed(format!("Processing failed: {}", e));
                        
                        // 这里可以重新入队重试任务
                        // 为简化，暂时跳过重试机制
                    }
                }
            }
        }

        // 批量保存消息到数据库
        if !messages_to_save.is_empty() {
            if let Err(e) = Self::batch_save_messages(
                worker_id,
                messages_to_save,
                db_connection,
                config,
            ).await {
                error!("Worker {} failed to save messages: {}", worker_id, e);
                // 将成功计数转为失败
                failed_count += success_count;
                success_count = 0;
            }
        }

        Ok((success_count, failed_count, skipped_count))
    }



    /// 处理单个任务
    async fn process_single_task(
        task: &mut ReceiveTask,
        state_manager: &MessageStateManager,
    ) -> Result<ProcessResult> {
        task.mark_processing();

        // 检查消息是否已存在 - 暂时跳过这个检查，因为方法不存在
        // 在实际实现中，这里应该查询数据库检查消息是否已存在

        // 转换为消息实体
        let message = Self::convert_task_to_message(task)?;

        // 更新消息状态
        state_manager.update_message_state(
            task.message_data.id as u64,
            MessageStatus::Received as i32,
        )?;

        task.mark_completed();
        Ok(ProcessResult::Success(message))
    }

    /// 将任务转换为消息实体
    fn convert_task_to_message(task: &ReceiveTask) -> Result<Message> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        Ok(Message {
            id: Some(task.message_data.id),  // 只使用 message.id
            server_message_id: Some(task.server_msg_id),
            pts: task.sequence_id as i64,  // ⭐ message_seq -> pts
            channel_id: task.message_data.channel_id,
            channel_type: task.message_data.channel_type,
            timestamp: Some(now),
            from_uid: task.message_data.from_uid,
            message_type: task.message_data.message_type,
            content: task.message_data.content.clone(),
            status: MessageStatus::Received as i32,
            voice_status: 0,
            created_at: now,
            updated_at: now,
            searchable_word: String::new(),
            local_message_id: 0,
            is_deleted: 0,
            setting: 0,
            order_seq: task.sequence_id as i64,
            extra: serde_json::to_string(&task.message_data.extra).unwrap_or_default(),
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: String::new(),
            expire_time: None,
            expire_timestamp: None,
            revoked: 0,
            revoked_at: 0,
            revoked_by: None,
        })
    }

    /// 批量保存消息
    async fn batch_save_messages(
        worker_id: usize,
        messages: Vec<Message>,
        db_connection: &Arc<Mutex<Connection>>,
        config: &ReceiveConsumerConfig,
    ) -> Result<()> {
        let message_count = messages.len();
        
        debug!("Worker {} saving {} messages to database", worker_id, message_count);

        let save_result = timeout(
            Duration::from_secs(config.db_timeout_seconds),
            async {
                let conn = db_connection.lock().await;
                let dao = MessageDao::new(&*conn);
                
                for message in messages {
                    // 按 (channel_id, server_message_id) 去重：已存在则跳过，避免重复插入
                    if let Some(server_msg_id) = message.server_message_id {
                        if dao.get_id_by_channel_and_server_message_id(message.channel_id, server_msg_id)?.is_some() {
                            debug!("Worker {} skip duplicate message: channel_id={}, server_message_id={}", worker_id, message.channel_id, server_msg_id);
                            continue;
                        }
                    }
                    dao.insert(&message)?;
                }
                
                Ok::<(), PrivchatSDKError>(())
            }
        ).await;

        match save_result {
            Ok(Ok(())) => {
                info!("Worker {} successfully saved {} messages", worker_id, message_count);
                Ok(())
            }
            Ok(Err(e)) => {
                error!("Worker {} database error: {}", worker_id, e);
                Err(e)
            }
            Err(_) => {
                error!("Worker {} database save timeout", worker_id);
                Err(PrivchatSDKError::Timeout("Database save timeout".to_string()))
            }
        }
    }

    /// 启动超时批次处理器
    async fn start_timeout_processor(&self) {
        let queue_manager = self.queue_manager.clone();
        let shutdown_signal = self.shutdown_signal.clone();
        let is_running = self.is_running.clone();
        let interval = self.config.timeout_check_interval_seconds;

        tokio::spawn(async move {
            info!("Timeout batch processor started");

            loop {
                select! {
                    _ = shutdown_signal.notified() => {
                        info!("Timeout processor received shutdown signal");
                        break;
                    }
                    _ = sleep(Duration::from_secs(interval)) => {
                        if !*is_running.read().await {
                            break;
                        }

                        match queue_manager.process_timeout_batches().await {
                            Ok(count) => {
                                if count > 0 {
                                    debug!("Processed {} timeout batches", count);
                                }
                            }
                            Err(e) => {
                                error!("Error processing timeout batches: {}", e);
                            }
                        }
                    }
                }
            }

            info!("Timeout batch processor stopped");
        });
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> ReceiveConsumerStats {
        self.stats.read().await.clone()
    }

    /// 清除统计信息
    pub async fn clear_stats(&self) {
        let mut stats = self.stats.write().await;
        *stats = ReceiveConsumerStats::default();
    }

    /// 检查是否正在运行
    pub async fn is_running(&self) -> bool {
        *self.is_running.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::{MessageData, receive_task::MessageSource};
    use tempfile::TempDir;

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

    async fn create_test_consumer() -> (ReceiveConsumerRunner, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let state_manager = Arc::new(MessageStateManager::new(conn));
        
        let queue_config = ReceiveQueueConfig::default();
        let queue_manager = Arc::new(ReceiveQueueManager::new(queue_config));
        
        let consumer_config = ReceiveConsumerConfig::default();
        let db_conn = Arc::new(Mutex::new(rusqlite::Connection::open(&db_path).unwrap()));
        
        let consumer = ReceiveConsumerRunner::new(
            consumer_config,
            queue_manager,
            state_manager,
            db_conn,
        );
        
        (consumer, temp_dir)
    }

    #[tokio::test]
    async fn test_consumer_lifecycle() {
        let (consumer, _temp_dir) = create_test_consumer().await;
        
        // 启动消费者
        consumer.start().await.unwrap();
        assert!(consumer.is_running().await);
        
        // 停止消费者
        consumer.stop().await.unwrap();
        assert!(!consumer.is_running().await);
    }

    #[tokio::test]
    async fn test_consumer_stats() {
        let (consumer, _temp_dir) = create_test_consumer().await;
        
        let stats = consumer.get_stats().await;
        assert_eq!(stats.tasks_processed, 0);
        assert_eq!(stats.success_rate(), 0.0);
        
        // 清除统计
        consumer.clear_stats().await;
        let stats = consumer.get_stats().await;
        assert_eq!(stats.tasks_processed, 0);
    }

    #[tokio::test]
    async fn test_task_to_message_conversion() {
        let message_data = create_test_message_data();
        let task = ReceiveTask::new(
            message_data.clone(),
            12345,
            789,
            MessageSource::RealTime,
        );

        let message = ReceiveConsumerRunner::convert_task_to_message(&task).unwrap();
        
        assert_eq!(message.id, Some(task.message_data.id));
        assert_eq!(message.server_message_id, Some(task.server_msg_id));
        assert_eq!(message.channel_id, task.message_data.channel_id);
        assert_eq!(message.pts, task.sequence_id as i64);
        assert_eq!(message.status, MessageStatus::Received as i32);
    }
} 