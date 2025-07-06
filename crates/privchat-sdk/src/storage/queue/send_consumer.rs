use crate::storage::queue::{SendQueueManager, TaskQueueTrait};
use crate::storage::queue::send_task::{SendTask, TaskStatus};
use crate::storage::queue::retry_policy::{RetryManager, SendFailureReason};
use crate::storage::message_state::MessageStateManager;
use crate::network::{NetworkSender, NetworkMonitor, NetworkStatus, NetworkStatusEvent};
use crate::error::{PrivchatSDKError, Result};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio::time::{sleep, timeout, Instant as TokioInstant};
use tokio_util::time::DelayQueue;
use tokio::select;
use futures::StreamExt;
use tracing::{debug, error, info, warn, instrument};
use serde::{Deserialize, Serialize};

/// 频道键，用于控制串行发送
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ChannelKey {
    pub channel_id: String,
    pub channel_type: i32,
}

impl ChannelKey {
    pub fn new(channel_id: String, channel_type: i32) -> Self {
        Self { channel_id, channel_type }
    }
    
    pub fn from_task(task: &SendTask) -> Self {
        Self::new(
            task.message_data.channel_id.clone(),
            task.message_data.channel_type,
        )
    }
}

/// 发送消费者配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendConsumerConfig {
    /// 工作线程数量
    pub worker_count: usize,
    /// 批量拉取任务数量
    pub batch_size: usize,
    /// 拉取任务间隔（毫秒）
    pub poll_interval_ms: u64,
    /// 发送超时时间（秒）
    pub send_timeout_seconds: u64,
    /// 最大并发频道数
    pub max_concurrent_channels: usize,
    /// 延迟队列容量
    pub delay_queue_capacity: usize,
}

impl Default for SendConsumerConfig {
    fn default() -> Self {
        Self {
            worker_count: 4,
            batch_size: 10,
            poll_interval_ms: 100,
            send_timeout_seconds: 30,
            max_concurrent_channels: 100,
            delay_queue_capacity: 1000,
        }
    }
}

/// 发送统计信息
#[derive(Debug, Clone, Default)]
pub struct SendMetrics {
    pub send_attempt_total: u64,
    pub send_success_total: u64,
    pub send_failure_total: u64,
    pub retry_count_total: u64,
    pub channel_serial_violations: u64,
    pub average_retry_count: f64,
}

impl SendMetrics {
    pub fn success_rate(&self) -> f64 {
        if self.send_attempt_total == 0 {
            0.0
        } else {
            self.send_success_total as f64 / self.send_attempt_total as f64
        }
    }
    
    pub fn update_retry_average(&mut self) {
        if self.send_attempt_total > 0 {
            self.average_retry_count = self.retry_count_total as f64 / self.send_attempt_total as f64;
        }
    }
}

/// 延迟重试任务
#[derive(Debug)]
struct DelayedTask {
    task: SendTask,
    retry_at: TokioInstant,
}

/// 发送消费者运行器
pub struct SendConsumerRunner {
    config: SendConsumerConfig,
    task_queue: Arc<dyn TaskQueueTrait>,
    state_manager: Arc<MessageStateManager>,
    network_sender: Arc<dyn NetworkSender>,
    network_monitor: Arc<NetworkMonitor>,
    retry_manager: Arc<RetryManager>,
    
    // 频道级串行控制
    channel_locks: Arc<RwLock<HashMap<ChannelKey, Arc<Mutex<()>>>>>,
    
    // 延迟重试队列
    delay_queue: Arc<Mutex<DelayQueue<DelayedTask>>>,
    
    // 统计信息
    metrics: Arc<RwLock<SendMetrics>>,
    
    // 控制信号
    shutdown_signal: Arc<tokio::sync::Notify>,
    is_running: Arc<RwLock<bool>>,
}

impl SendConsumerRunner {
    pub fn new(
        config: SendConsumerConfig,
        task_queue: Arc<dyn TaskQueueTrait>,
        state_manager: Arc<MessageStateManager>,
        network_sender: Arc<dyn NetworkSender>,
        network_monitor: Arc<NetworkMonitor>,
        retry_manager: Arc<RetryManager>,
    ) -> Self {
        Self {
            config,
            task_queue,
            state_manager,
            network_sender,
            network_monitor,
            retry_manager,
            channel_locks: Arc::new(RwLock::new(HashMap::new())),
            delay_queue: Arc::new(Mutex::new(DelayQueue::with_capacity(1000))),
            metrics: Arc::new(RwLock::new(SendMetrics::default())),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// 启动消费者
    #[instrument(skip(self))]
    pub async fn start(&self) -> Result<()> {
        {
            let mut running = self.is_running.write().await;
            if *running {
                return Err(PrivchatSDKError::Other("Consumer already running".to_string()));
            }
            *running = true;
        }

        info!("Starting SendConsumer with {} workers", self.config.worker_count);

        // 启动网络状态监听
        let mut network_events = self.network_monitor.subscribe();
        let shutdown_clone = self.shutdown_signal.clone();
        tokio::spawn(async move {
            loop {
                select! {
                    event = network_events.recv() => {
                        match event {
                            Ok(NetworkStatusEvent { new_status, .. }) => {
                                info!("Network status changed to: {:?}", new_status);
                            }
                            Err(_) => break,
                        }
                    }
                    _ = shutdown_clone.notified() => break,
                }
            }
        });

        // 启动延迟队列处理器
        self.start_delay_queue_processor().await;

        // 启动工作线程
        for worker_id in 0..self.config.worker_count {
            self.start_worker(worker_id).await;
        }

        Ok(())
    }

    /// 停止消费者
    #[instrument(skip(self))]
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping SendConsumer");
        
        {
            let mut running = self.is_running.write().await;
            *running = false;
        }

        self.shutdown_signal.notify_waiters();
        
        // 等待一段时间让工作线程优雅退出
        sleep(Duration::from_millis(500)).await;
        
        info!("SendConsumer stopped");
        Ok(())
    }

    /// 启动工作线程
    async fn start_worker(&self, worker_id: usize) {
        let task_queue = self.task_queue.clone();
        let state_manager = self.state_manager.clone();
        let network_sender = self.network_sender.clone();
        let network_monitor = self.network_monitor.clone();
        let retry_manager = self.retry_manager.clone();
        let channel_locks = self.channel_locks.clone();
        let delay_queue = self.delay_queue.clone();
        let metrics = self.metrics.clone();
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

                        // 检查网络状态
                        let network_status = network_monitor.get_status().await;
                        if network_status != NetworkStatus::Online {
                            debug!("Worker {} skipping due to network status: {:?}", worker_id, network_status);
                            continue;
                        }

                        // 拉取任务
                        match Self::pull_and_process_tasks(
                            worker_id,
                            &*task_queue,
                            &*state_manager,
                            &*network_sender,
                            &*retry_manager,
                            &channel_locks,
                            &delay_queue,
                            &metrics,
                            &config,
                        ).await {
                            Ok(processed_count) => {
                                if processed_count > 0 {
                                    debug!("Worker {} processed {} tasks", worker_id, processed_count);
                                }
                            }
                            Err(e) => {
                                error!("Worker {} error: {}", worker_id, e);
                            }
                        }
                    }
                }
            }

            info!("Worker {} stopped", worker_id);
        });
    }

    /// 拉取并处理任务
    async fn pull_and_process_tasks(
        worker_id: usize,
        task_queue: &dyn TaskQueueTrait,
        state_manager: &MessageStateManager,
        network_sender: &dyn NetworkSender,
        retry_manager: &RetryManager,
        channel_locks: &Arc<RwLock<HashMap<ChannelKey, Arc<Mutex<()>>>>>,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
        config: &SendConsumerConfig,
    ) -> Result<usize> {
        // 拉取任务
        let tasks = task_queue.dequeue_batch(config.batch_size).await?;
        if tasks.is_empty() {
            return Ok(0);
        }

        let mut processed_count = 0;

        for mut task in tasks {
            // 检查任务是否过期
            if task.is_expired() {
                warn!("Task {} expired, marking as failed", task.client_msg_no);
                task.mark_expired();
                state_manager.update_message_state(&task.client_msg_no, crate::storage::entities::MessageStatus::Failed as i32)?;
                continue;
            }

            // 获取频道锁
            let channel_key = ChannelKey::from_task(&task);
            let channel_lock = Self::get_channel_lock(channel_locks, channel_key).await;
            
            // 在锁保护下处理任务
            let _guard = channel_lock.lock().await;
            
            match Self::process_single_task(
                worker_id,
                task,
                state_manager,
                network_sender,
                retry_manager,
                delay_queue,
                metrics,
                config,
            ).await {
                Ok(_) => processed_count += 1,
                Err(e) => {
                    error!("Worker {} failed to process task: {}", worker_id, e);
                }
            }
        }

        Ok(processed_count)
    }

    /// 处理单个任务
    async fn process_single_task(
        worker_id: usize,
        mut task: SendTask,
        state_manager: &MessageStateManager,
        network_sender: &dyn NetworkSender,
        retry_manager: &RetryManager,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
        config: &SendConsumerConfig,
    ) -> Result<()> {
        debug!("Worker {} processing task: {}", worker_id, task.client_msg_no);

        // 更新统计
        {
            let mut m = metrics.write().await;
            m.send_attempt_total += 1;
            m.retry_count_total += task.retry_count as u64;
            m.update_retry_average();
        }

        // 标记任务为处理中
        task.mark_processing();
        state_manager.update_message_state(&task.client_msg_no, crate::storage::entities::MessageStatus::Sending as i32)?;

        // 执行发送
        let send_result = timeout(
            Duration::from_secs(config.send_timeout_seconds),
            network_sender.send_message(&task.message_data)
        ).await;

        match send_result {
            Ok(Ok(response)) => {
                // 发送成功
                info!("Worker {} successfully sent message: {} -> {}", 
                     worker_id, task.client_msg_no, response.server_msg_id);
                
                task.mark_completed();
                state_manager.update_message_state(&task.client_msg_no, crate::storage::entities::MessageStatus::Sent as i32)?;
                
                // 更新统计
                {
                    let mut m = metrics.write().await;
                    m.send_success_total += 1;
                }
            }
            Ok(Err(send_error)) => {
                // 发送失败，处理重试
                Self::handle_send_failure(
                    worker_id,
                    task,
                    send_error,
                    state_manager,
                    retry_manager,
                    delay_queue,
                    metrics,
                ).await?;
            }
            Err(_) => {
                // 超时
                warn!("Worker {} timeout sending message: {}", worker_id, task.client_msg_no);
                let timeout_error = PrivchatSDKError::Transport("Send timeout".to_string());
                Self::handle_send_failure(
                    worker_id,
                    task,
                    timeout_error,
                    state_manager,
                    retry_manager,
                    delay_queue,
                    metrics,
                ).await?;
            }
        }

        Ok(())
    }

    /// 处理发送失败
    async fn handle_send_failure(
        worker_id: usize,
        mut task: SendTask,
        error: PrivchatSDKError,
        state_manager: &MessageStateManager,
        retry_manager: &RetryManager,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
    ) -> Result<()> {
        let failure_reason: SendFailureReason = error.into();
        
        warn!("Worker {} send failed for {}: {:?}", 
             worker_id, task.client_msg_no, failure_reason);

        // 更新统计
        {
            let mut m = metrics.write().await;
            m.send_failure_total += 1;
        }

        // 检查是否可以重试
        match retry_manager.handle_send_failure(task.retry_count, failure_reason.clone())? {
            Some(next_retry_time) => {
                // 可以重试，加入延迟队列
                task.increment_retry();
                task.next_retry_at = Some(next_retry_time);
                task.mark_failed(format!("{:?}", failure_reason), Some(failure_reason));
                
                info!("Worker {} scheduling retry for {} at {}", 
                     worker_id, task.client_msg_no, next_retry_time);

                let retry_delay = Duration::from_secs(
                    next_retry_time.saturating_sub(
                        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
                    )
                );

                let delayed_task = DelayedTask {
                    task,
                    retry_at: TokioInstant::now() + retry_delay,
                };

                delay_queue.lock().await.insert(delayed_task, retry_delay);
            }
            None => {
                // 不能重试，标记为最终失败
                task.mark_failed(format!("{:?}", failure_reason), Some(failure_reason));
                state_manager.update_message_state(&task.client_msg_no, crate::storage::entities::MessageStatus::Failed as i32)?;
                
                error!("Worker {} final failure for {}: max retries exceeded", 
                      worker_id, task.client_msg_no);
            }
        }

        Ok(())
    }

    /// 获取频道锁
    async fn get_channel_lock(
        channel_locks: &Arc<RwLock<HashMap<ChannelKey, Arc<Mutex<()>>>>>,
        channel_key: ChannelKey,
    ) -> Arc<Mutex<()>> {
        // 先尝试读锁
        {
            let locks = channel_locks.read().await;
            if let Some(lock) = locks.get(&channel_key) {
                return lock.clone();
            }
        }

        // 需要创建新锁，获取写锁
        let mut locks = channel_locks.write().await;
        locks.entry(channel_key).or_insert_with(|| Arc::new(Mutex::new(()))).clone()
    }

    /// 启动延迟队列处理器
    async fn start_delay_queue_processor(&self) {
        let delay_queue = self.delay_queue.clone();
        let task_queue = self.task_queue.clone();
        let shutdown_signal = self.shutdown_signal.clone();
        let is_running = self.is_running.clone();

        tokio::spawn(async move {
            info!("Delay queue processor started");

            loop {
                select! {
                    _ = shutdown_signal.notified() => {
                        info!("Delay queue processor received shutdown signal");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if !*is_running.read().await {
                            break;
                        }

                        // 检查延迟队列中是否有到期的任务
                        let mut expired_tasks = Vec::new();
                        {
                            let mut queue = delay_queue.lock().await;
                            while let Some(expired_item) = queue.next().await {
                                expired_tasks.push(expired_item.into_inner());
                            }
                        }

                        // 处理到期的任务
                        for delayed_task in expired_tasks {
                            info!("Retry time reached for task: {}", delayed_task.task.client_msg_no);
                            
                            // 重新入队
                            match task_queue.enqueue(delayed_task.task).await {
                                Ok(_) => {
                                    debug!("Successfully re-enqueued retry task");
                                }
                                Err(e) => {
                                    error!("Failed to re-enqueue retry task: {}", e);
                                }
                            }
                        }
                    }
                }
            }

            info!("Delay queue processor stopped");
        });
    }

    /// 获取统计信息
    pub async fn get_metrics(&self) -> SendMetrics {
        self.metrics.read().await.clone()
    }

    /// 清除统计信息
    pub async fn clear_metrics(&self) {
        let mut metrics = self.metrics.write().await;
        *metrics = SendMetrics::default();
    }

    /// 检查是否正在运行
    pub async fn is_running(&self) -> bool {
        *self.is_running.read().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::{MemoryTaskQueue, MessageData};
    use crate::storage::queue::priority::QueuePriority;
    use crate::storage::queue::retry_policy::RetryPolicy;
    use crate::network::{DummyNetworkSender, DummyNetworkStatusListener};
    use tempfile::TempDir;
    use rusqlite;

    async fn create_test_consumer() -> (SendConsumerRunner, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let state_manager = Arc::new(MessageStateManager::new(conn));
        let task_queue = Arc::new(MemoryTaskQueue::new());
        let network_sender = Arc::new(DummyNetworkSender::default());
        let network_listener = Arc::new(DummyNetworkStatusListener::default());
        let network_monitor = Arc::new(NetworkMonitor::new(network_listener, network_sender.clone()));
        let retry_manager = Arc::new(RetryManager::new(RetryPolicy::default()));
        
        let consumer = SendConsumerRunner::new(
            SendConsumerConfig::default(),
            task_queue,
            state_manager,
            network_sender,
            network_monitor,
            retry_manager,
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
    async fn test_channel_key() {
        let key1 = ChannelKey::new("channel1".to_string(), 1);
        let key2 = ChannelKey::new("channel1".to_string(), 1);
        let key3 = ChannelKey::new("channel2".to_string(), 1);
        
        assert_eq!(key1, key2);
        assert_ne!(key1, key3);
    }

    #[tokio::test]
    async fn test_send_metrics() {
        let mut metrics = SendMetrics::default();
        
        metrics.send_attempt_total = 10;
        metrics.send_success_total = 8;
        metrics.retry_count_total = 15;
        
        metrics.update_retry_average();
        
        assert_eq!(metrics.success_rate(), 0.8);
        assert_eq!(metrics.average_retry_count, 1.5);
    }
} 