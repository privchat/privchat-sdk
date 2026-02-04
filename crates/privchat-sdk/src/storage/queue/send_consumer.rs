use crate::storage::queue::TaskQueueTrait;
use crate::storage::queue::send_task::SendTask;
use crate::storage::queue::retry_policy::{RetryManager, SendFailureReason};
use crate::storage::StorageManager;
use crate::network::{NetworkMonitor, NetworkStatus, NetworkStatusEvent};
use crate::client::PrivchatClient;
use crate::error::{PrivchatSDKError, Result};
use crate::rate_limiter::MessageRateLimiter;
use crate::events::{EventManager, SDKEvent, SendStatusState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};
use tokio::time::{sleep, timeout, Instant as TokioInstant};
use tokio_util::time::DelayQueue;
use tokio::select;
use futures::StreamExt;
use tracing::{debug, error, info, warn, instrument};

/// 消息发送结果（仅用于消息队列内部）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResult {
    /// 服务端消息ID（发送成功时才有值）
    pub server_msg_id: Option<u64>,
    /// 发送时间戳
    pub sent_at: u64,
    /// 是否成功
    pub success: bool,
    /// 错误信息（如果失败）
    pub error_message: Option<String>,
}

/// 频道键，用于控制串行发送
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ChannelKey {
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i32,
}

impl ChannelKey {
    pub fn new(channel_id: u64, channel_type: i32) -> Self {
        Self { channel_id, channel_type }
    }
    
    pub fn from_task(task: &SendTask) -> Self {
        Self::new(
            task.message_data.channel_id,  // u64，直接使用
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
#[allow(dead_code)]
struct DelayedTask {
    task: SendTask,
    retry_at: TokioInstant,
}

/// 发送消费者运行器
pub struct SendConsumerRunner {
    config: SendConsumerConfig,
    task_queue: Arc<dyn TaskQueueTrait>,
    storage_manager: Arc<StorageManager>,
    client: Arc<tokio::sync::RwLock<Option<PrivchatClient>>>,
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
    
    // 消息发送限流器
    message_rate_limiter: Arc<MessageRateLimiter>,
    
    // 事件管理器（用于发送状态通知）
    event_manager: Arc<EventManager>,
}

impl SendConsumerRunner {
    pub fn new(
        config: SendConsumerConfig,
        task_queue: Arc<dyn TaskQueueTrait>,
        storage_manager: Arc<StorageManager>,
        client: Arc<tokio::sync::RwLock<Option<PrivchatClient>>>,
        network_monitor: Arc<NetworkMonitor>,
        retry_manager: Arc<RetryManager>,
        message_rate_limiter: Arc<MessageRateLimiter>,
        event_manager: Arc<EventManager>,
    ) -> Self {
        Self {
            config,
            task_queue,
            storage_manager,
            client,
            network_monitor,
            retry_manager,
            channel_locks: Arc::new(RwLock::new(HashMap::new())),
            delay_queue: Arc::new(Mutex::new(DelayQueue::with_capacity(1000))),
            metrics: Arc::new(RwLock::new(SendMetrics::default())),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            is_running: Arc::new(RwLock::new(false)),
            message_rate_limiter,
            event_manager,
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
        let storage_manager = self.storage_manager.clone();
        let client = self.client.clone();
        let network_monitor = self.network_monitor.clone();
        let retry_manager = self.retry_manager.clone();
        let channel_locks = self.channel_locks.clone();
        let delay_queue = self.delay_queue.clone();
        let metrics = self.metrics.clone();
        let shutdown_signal = self.shutdown_signal.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();
        let message_rate_limiter = self.message_rate_limiter.clone();
        let event_manager = self.event_manager.clone();

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
                            info!("Worker {} skipping due to network status: {:?}", worker_id, network_status);
                            continue;
                        }

                        info!("Worker {} attempting to pull tasks...", worker_id);

                        // 拉取任务
                        match Self::pull_and_process_tasks(
                            worker_id,
                            &*task_queue,
                            &*storage_manager,
                            &client,
                            &*retry_manager,
                            &channel_locks,
                            &delay_queue,
                            &metrics,
                            &config,
                            &*message_rate_limiter,
                            &*event_manager,
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
        storage_manager: &StorageManager,
        client: &Arc<tokio::sync::RwLock<Option<PrivchatClient>>>,
        retry_manager: &RetryManager,
        channel_locks: &Arc<RwLock<HashMap<ChannelKey, Arc<Mutex<()>>>>>,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
        config: &SendConsumerConfig,
        message_rate_limiter: &MessageRateLimiter,
        event_manager: &EventManager,
    ) -> Result<usize> {
        // 拉取任务
        let tasks = task_queue.dequeue_batch(config.batch_size).await?;
        if tasks.is_empty() {
            debug!("Worker {} found no tasks in queue", worker_id);
            return Ok(0);
        }
        
        info!("Worker {} pulled {} tasks from queue", worker_id, tasks.len());

        let mut processed_count = 0;

        for mut task in tasks {
            // 检查任务是否过期
            if task.is_expired() {
                warn!("Task {} expired, marking as failed", task.id);
                task.mark_expired();
                if let Err(e) = storage_manager.update_message_status(task.id, crate::storage::entities::MessageStatus::Failed as i32).await {
                    error!("Failed to update expired message status: {}", e);
                }
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
                storage_manager,
                client,
                retry_manager,
                delay_queue,
                metrics,
                config,
                message_rate_limiter,
                event_manager,
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
        storage_manager: &StorageManager,
        client: &Arc<tokio::sync::RwLock<Option<PrivchatClient>>>,
        retry_manager: &RetryManager,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
        config: &SendConsumerConfig,
        message_rate_limiter: &MessageRateLimiter,
        event_manager: &EventManager,
    ) -> Result<()> {
        debug!("Worker {} processing task: {}", worker_id, task.id);

        // 更新统计
        {
            let mut m = metrics.write().await;
            m.send_attempt_total += 1;
            m.retry_count_total += task.retry_count as u64;
            m.update_retry_average();
        }

        // 标记任务为处理中
        task.mark_processing();
        storage_manager.update_message_status(task.id, crate::storage::entities::MessageStatus::Sending as i32).await?;
        
        // 通知发送状态：Sending
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        event_manager.emit(SDKEvent::SendStatusUpdate {
            channel_id: task.message_data.channel_id,
            id: task.id as u64,
            state: SendStatusState::Sending,
            attempts: task.retry_count,
            error: None,
            timestamp,
        }).await;

        // 🔥 检查消息发送限流
        let is_group = task.message_data.channel_type != 0; // 假设 0 = 私聊，其他 = 群聊
        if let Err(wait_duration) = message_rate_limiter.check_send(is_group) {
            debug!(
                "Worker {} 消息发送受限（{}），等待 {}ms",
                worker_id,
                if is_group { "群聊" } else { "私聊" },
                wait_duration.as_millis()
            );
            tokio::time::sleep(wait_duration).await;
        }

        // 执行发送（直接调用 PrivchatClient）
        let send_result = {
            let mut client_guard = client.write().await;
            let client_ref = client_guard.as_mut()
                .ok_or_else(|| PrivchatSDKError::NotConnected)?;
            
            // 构造消息内容；local_message_id 由 sdk 入队时雪花生成，兼容旧任务用 id 回退
            let message = &task.message_data;
            let content = &message.content;
            let channel_id = message.channel_id;
            let local_message_id = if message.local_message_id != 0 {
                message.local_message_id
            } else {
                task.id as u64
            };
            
            timeout(
                Duration::from_secs(config.send_timeout_seconds),
                async {
                    // 从 extra 中提取 reply_to_message_id（如果有）
                    let metadata = if let Some(reply_id_str) = message.extra.get("reply_to_message_id") {
                        if let Ok(reply_id) = reply_id_str.parse::<u64>() {
                            Some(serde_json::json!({
                                "reply_to_message_id": reply_id
                            }))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    
                    // 直接调用 client 的 send_message_internal；local_message_id 用于服务端去重
                    let (_ignored, server_message_id) = client_ref.send_message_internal(
                        channel_id,
                        content,
                        "text",
                        metadata,
                        local_message_id,
                    ).await?;
                    
                    // 返回 SendResult 格式
                    Ok(SendResult {
                        success: true,
                        server_msg_id: Some(server_message_id),
                        sent_at: chrono::Utc::now().timestamp_millis() as u64,
                        error_message: None,
                    })
                }
            ).await
        };

        match send_result {
            Ok(Ok(response)) => {
                // 发送成功
                info!("Worker {} successfully sent message: {} -> {:?}", 
                     worker_id, task.id, response.server_msg_id);
                
                task.mark_completed();
                storage_manager.update_message_status(task.id, crate::storage::entities::MessageStatus::Sent as i32).await?;
                
                // 更新服务端消息ID（用于撤回等操作）
                if let Some(server_message_id) = response.server_msg_id {
                    info!("🔍 [DEBUG] Worker {} 准备更新 message_id: local_message_id={}, server_message_id={}", 
                         worker_id, task.id, server_message_id);
                    
                    if let Err(e) = storage_manager.update_message_server_id(task.id, server_message_id).await {
                        warn!("❌ Worker {} failed to update server message_id: {}", worker_id, e);
                    } else {
                        info!("✅ Worker {} 成功更新 message_id: local_message_id={} -> message_id={}", 
                             worker_id, task.id, server_message_id);
                    }
                } else {
                    warn!("⚠️ Worker {} 发送成功但未返回 server_msg_id: local_message_id={}", 
                         worker_id, task.id);
                }
                
                // 更新统计
                {
                    let mut m = metrics.write().await;
                    m.send_success_total += 1;
                }
                
                // 通知发送状态：Sent
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                event_manager.emit(SDKEvent::SendStatusUpdate {
                    channel_id: task.message_data.channel_id,
                    id: task.id as u64,
                    state: SendStatusState::Sent,
                    attempts: task.retry_count,
                    error: None,
                    timestamp,
                }).await;
            }
            Ok(Err(send_error)) => {
                // 发送失败，处理重试
                Self::handle_send_failure(
                    worker_id,
                    task,
                    send_error,
                    storage_manager,
                    retry_manager,
                    delay_queue,
                    metrics,
                    event_manager,
                ).await?;
            }
            Err(_) => {
                // 超时
                warn!("Worker {} timeout sending message: {}", worker_id, task.id);
                let timeout_error = PrivchatSDKError::Transport("Send timeout".to_string());
                Self::handle_send_failure(
                    worker_id,
                    task,
                    timeout_error,
                    storage_manager,
                    retry_manager,
                    delay_queue,
                    metrics,
                    event_manager,
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
        storage_manager: &StorageManager,
        retry_manager: &RetryManager,
        delay_queue: &Arc<Mutex<DelayQueue<DelayedTask>>>,
        metrics: &Arc<RwLock<SendMetrics>>,
        event_manager: &EventManager,
    ) -> Result<()> {
        let failure_reason: SendFailureReason = error.into();
        
        warn!("Worker {} send failed for {}: {:?}", 
             worker_id, task.id, failure_reason);

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
                task.mark_failed(format!("{:?}", failure_reason), Some(failure_reason.clone()));
                
                info!("Worker {} scheduling retry for {} at {}", 
                     worker_id, task.id, next_retry_time);
                
                // 通知发送状态：Retrying
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                event_manager.emit(SDKEvent::SendStatusUpdate {
                    channel_id: task.message_data.channel_id,
                    id: task.id as u64,
                    state: SendStatusState::Retrying,
                    attempts: task.retry_count,
                    error: Some(format!("{:?}", failure_reason)),
                    timestamp,
                }).await;

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
                task.mark_failed(format!("{:?}", failure_reason), Some(failure_reason.clone()));
                storage_manager.update_message_status(task.id, crate::storage::entities::MessageStatus::Failed as i32).await?;
                
                error!("Worker {} final failure for {}: max retries exceeded", 
                      worker_id, task.id);
                
                // 通知发送状态：Failed
                let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                event_manager.emit(SDKEvent::SendStatusUpdate {
                    channel_id: task.message_data.channel_id,
                    id: task.id as u64,
                    state: SendStatusState::Failed,
                    attempts: task.retry_count,
                    error: Some(format!("{:?}", failure_reason)),
                    timestamp,
                }).await;
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
                            info!("Retry time reached for task: {}", delayed_task.task.id);
                            
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
    use crate::storage::queue::{MemoryTaskQueue, TaskQueueTrait};
    use crate::storage::queue::retry_policy::RetryPolicy;
    use crate::network::DummyNetworkStatusListener;
    use crate::events::EventManager;
    use tempfile::TempDir;

    async fn create_test_consumer() -> (SendConsumerRunner, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let base_path = temp_dir.path();

        let storage_manager = StorageManager::new_simple(base_path).await.unwrap();
        storage_manager.init_user("test_uid").await.unwrap();
        storage_manager.switch_user("test_uid").await.unwrap();

        let task_queue = Arc::new(MemoryTaskQueue::new());
        let client = Arc::new(tokio::sync::RwLock::new(None));
        let network_listener = Arc::new(DummyNetworkStatusListener::default());
        let network_monitor = Arc::new(NetworkMonitor::new(network_listener));
        let retry_manager = Arc::new(RetryManager::new(RetryPolicy::default()));
        let message_rate_limiter = Arc::new(MessageRateLimiter::new(
            crate::rate_limiter::MessageRateLimiterConfig::default(),
        ));
        let event_manager = Arc::new(EventManager::new(100));

        let consumer = SendConsumerRunner::new(
            SendConsumerConfig::default(),
            task_queue,
            Arc::new(storage_manager),
            client,
            network_monitor,
            retry_manager,
            message_rate_limiter,
            event_manager,
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
        let key1 = ChannelKey::new(1001, 1);
        let key2 = ChannelKey::new(1001, 1);
        let key3 = ChannelKey::new(1002, 1);
        
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