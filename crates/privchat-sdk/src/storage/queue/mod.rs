use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info};
use std::fmt::Debug;

use crate::error::Result;
use crate::error::PrivchatSDKError;
use crate::storage::kv::KvStore;
use crossbeam_channel::{unbounded, Sender, Receiver};
use sled::Db;

pub mod priority;
pub mod send_task;
pub mod retry_policy;
pub mod send_consumer;
pub mod file_send_task;
pub mod file_send_queue;
pub mod file_send_consumer;

// 接收相关模块
pub mod receive_task;
pub mod receive_queue;
pub mod receive_consumer;

// 重新导出核心类型
pub use priority::{QueuePriority, PriorityComparator};
pub use send_task::{SendTask, MessageData, TaskStatus, TaskFilter};
pub use file_send_task::FileSendTask;
pub use file_send_queue::FileSendQueue;
pub use file_send_consumer::{FileConsumerRunner, FileConsumerConfig};

/// 队列统计信息
#[derive(Debug, Clone)]
pub struct QueueStats {
    pub total_tasks: usize,
    pub pending_tasks: usize,
    pub processing_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
    pub expired_tasks: usize,
    pub avg_processing_time_ms: f64,
    pub priority_distribution: HashMap<QueuePriority, usize>,
}

#[async_trait::async_trait]
pub trait TaskQueueTrait: Debug + Send + Sync {
    async fn push(&self, task: SendTask) -> Result<()>;
    async fn pop(&self) -> Result<Option<SendTask>>;
    async fn batch_push(&self, tasks: Vec<SendTask>) -> Result<()>;
    async fn batch_pop(&self, limit: usize) -> Result<Vec<SendTask>>;
    async fn remove(&self, task_id: &str) -> Result<()>;
    async fn clear(&self) -> Result<()>;
    async fn size(&self) -> Result<usize>;
    async fn is_empty(&self) -> Result<bool>;
    async fn stats(&self) -> Result<QueueStats>;
    
    // SendConsumer需要的方法
    async fn enqueue(&self, task: SendTask) -> Result<()> {
        self.push(task).await
    }
    
    async fn dequeue_batch(&self, limit: usize) -> Result<Vec<SendTask>> {
        self.batch_pop(limit).await
    }
}

/// 任务队列枚举 - 支持内存队列和持久化队列
#[derive(Debug)]
pub enum TaskQueue {
    Memory(MemoryTaskQueue),
    Persistent(PersistentTaskQueue),
}

#[async_trait::async_trait]
impl TaskQueueTrait for TaskQueue {
    async fn push(&self, task: SendTask) -> Result<()> {
        match self {
            TaskQueue::Memory(q) => q.push(task).await,
            TaskQueue::Persistent(q) => q.push(task).await,
        }
    }
    
    async fn pop(&self) -> Result<Option<SendTask>> {
        match self {
            TaskQueue::Memory(q) => q.pop().await,
            TaskQueue::Persistent(q) => q.pop().await,
        }
    }
    
    async fn batch_push(&self, tasks: Vec<SendTask>) -> Result<()> {
        match self {
            TaskQueue::Memory(q) => q.batch_push(tasks).await,
            TaskQueue::Persistent(q) => q.batch_push(tasks).await,
        }
    }
    
    async fn batch_pop(&self, limit: usize) -> Result<Vec<SendTask>> {
        match self {
            TaskQueue::Memory(q) => q.batch_pop(limit).await,
            TaskQueue::Persistent(q) => q.batch_pop(limit).await,
        }
    }

    async fn remove(&self, task_id: &str) -> Result<()> {
        match self {
            TaskQueue::Memory(q) => q.remove(task_id).await,
            TaskQueue::Persistent(q) => q.remove(task_id).await,
        }
    }
    
    async fn clear(&self) -> Result<()> {
        match self {
            TaskQueue::Memory(q) => q.clear().await,
            TaskQueue::Persistent(q) => q.clear().await,
        }
    }
    
    async fn size(&self) -> Result<usize> {
        match self {
            TaskQueue::Memory(q) => q.size().await,
            TaskQueue::Persistent(q) => q.size().await,
        }
    }

    async fn is_empty(&self) -> Result<bool> {
        match self {
            TaskQueue::Memory(q) => q.is_empty().await,
            TaskQueue::Persistent(q) => q.is_empty().await,
        }
    }
    
    async fn stats(&self) -> Result<QueueStats> {
        match self {
            TaskQueue::Memory(q) => q.stats().await,
            TaskQueue::Persistent(q) => q.stats().await,
        }
    }
}

/// 基于内存的简单任务队列实现
#[derive(Debug)]
pub struct MemoryTaskQueue {
    tasks: Arc<RwLock<Vec<SendTask>>>,
    stats: Arc<RwLock<QueueStats>>,
}

impl MemoryTaskQueue {
    /// 创建新的内存任务队列
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(RwLock::new(Vec::new())),
            stats: Arc::new(RwLock::new(QueueStats {
                total_tasks: 0,
                pending_tasks: 0,
                processing_tasks: 0,
                completed_tasks: 0,
                failed_tasks: 0,
                expired_tasks: 0,
                avg_processing_time_ms: 0.0,
                priority_distribution: HashMap::new(),
            })),
        }
    }
    
    /// 更新统计信息
    async fn update_stats(&self) {
        let tasks = self.tasks.read().await;
        let mut stats = self.stats.write().await;
        
        stats.total_tasks = tasks.len();
        stats.pending_tasks = tasks.iter().filter(|t| t.status == TaskStatus::Pending).count();
        stats.processing_tasks = tasks.iter().filter(|t| t.status == TaskStatus::Processing).count();
        stats.completed_tasks = tasks.iter().filter(|t| t.status == TaskStatus::Completed).count();
        stats.failed_tasks = tasks.iter().filter(|t| t.status == TaskStatus::Failed).count();
        stats.expired_tasks = tasks.iter().filter(|t| t.status == TaskStatus::Expired).count();
        
        // 计算优先级分布
        stats.priority_distribution.clear();
        for task in tasks.iter() {
            *stats.priority_distribution.entry(task.priority).or_insert(0) += 1;
        }
        
        // 计算平均处理时间
        let completed_tasks: Vec<_> = tasks.iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .collect();
        
        if !completed_tasks.is_empty() {
            let total_time: u64 = completed_tasks.iter()
                .map(|t| t.age_ms())
                .sum();
            stats.avg_processing_time_ms = total_time as f64 / completed_tasks.len() as f64;
        }
    }
}

#[async_trait::async_trait]
impl TaskQueueTrait for MemoryTaskQueue {
    async fn push(&self, task: SendTask) -> Result<()> {
        {
            let mut tasks = self.tasks.write().await;
            tasks.push(task);
            
            // 按优先级排序
            tasks.sort();
            
            debug!("任务已推送到队列，当前队列大小: {}", tasks.len());
        } // 释放写锁
        
        self.update_stats().await;
        
        Ok(())
    }
    
    async fn pop(&self) -> Result<Option<SendTask>> {
        let task = {
            let mut tasks = self.tasks.write().await;
            
            // 寻找第一个可处理的任务
            let mut task_index = None;
            for (i, task) in tasks.iter().enumerate() {
                if task.status == TaskStatus::Pending && !task.is_expired() {
                    task_index = Some(i);
                    break;
                }
            }
            
            if let Some(index) = task_index {
                Some(tasks.remove(index))
            } else {
                None
            }
        }; // 释放写锁
        
        if task.is_some() {
            self.update_stats().await;
        }
        
        Ok(task)
    }
    
    async fn batch_push(&self, new_tasks: Vec<SendTask>) -> Result<()> {
        {
            let mut tasks = self.tasks.write().await;
            tasks.extend(new_tasks);
            
            // 按优先级排序
            tasks.sort();
            
            debug!("批量推送任务到队列，当前队列大小: {}", tasks.len());
        } // 释放写锁
        
        self.update_stats().await;
        
        Ok(())
    }
    
    async fn batch_pop(&self, limit: usize) -> Result<Vec<SendTask>> {
        let result = {
            let mut tasks = self.tasks.write().await;
            let mut result = Vec::new();
            
            // 寻找可处理的任务
            let mut i = 0;
            while i < tasks.len() && result.len() < limit {
                if tasks[i].status == TaskStatus::Pending && !tasks[i].is_expired() {
                    result.push(tasks.remove(i));
                } else {
                    i += 1;
                }
            }
            
            result
        }; // 释放写锁
        
        if !result.is_empty() {
            self.update_stats().await;
        }
        
        Ok(result)
    }

    async fn remove(&self, task_id: &str) -> Result<()> {
        {
            let mut tasks = self.tasks.write().await;
            tasks.retain(|task| task.task_id != task_id);
        } // 释放写锁
        
        self.update_stats().await;
        Ok(())
    }
    
    async fn clear(&self) -> Result<()> {
        {
            let mut tasks = self.tasks.write().await;
            tasks.clear();
        } // 释放写锁
        
        self.update_stats().await;
        Ok(())
    }
    
    async fn size(&self) -> Result<usize> {
        let tasks = self.tasks.read().await;
        Ok(tasks.len())
    }

    async fn is_empty(&self) -> Result<bool> {
        let tasks = self.tasks.read().await;
        Ok(tasks.is_empty())
    }
    
    async fn stats(&self) -> Result<QueueStats> {
        let stats = self.stats.read().await;
        Ok(stats.clone())
    }
}

impl Default for MemoryTaskQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// 持久化任务队列实现
#[derive(Debug)]
#[allow(dead_code)]
pub struct PersistentTaskQueue {
    kv_store: Arc<KvStore>,
    user_id: String,
    queue_key: String,
}

impl PersistentTaskQueue {
    pub fn new(kv_store: Arc<KvStore>, user_id: String) -> Self {
        let queue_key = format!("queue:{}:tasks", user_id);
        Self {
            kv_store,
            user_id,
            queue_key,
        }
    }

    fn task_key(&self, task_id: &str) -> String {
        format!("{}:{}", self.queue_key, task_id)
    }

    fn serialize_task(&self, task: &SendTask) -> Result<Vec<u8>> {
        serde_json::to_vec(task).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))
    }

    fn deserialize_task(&self, data: &[u8]) -> Result<SendTask> {
        serde_json::from_slice(data).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))
    }

    async fn get_all_tasks(&self) -> Result<Vec<SendTask>> {
        let mut tasks = Vec::new();
        let prefix = format!("{}:", self.queue_key);
        let items = self.kv_store.scan_prefix::<Vec<u8>>(prefix.as_bytes()).await?;
        
        for (_, data) in items {
            if let Ok(task) = self.deserialize_task(&data) {
                tasks.push(task);
            }
        }
        
        // 按优先级排序
        tasks.sort();
        Ok(tasks)
    }
}

#[async_trait::async_trait]
impl TaskQueueTrait for PersistentTaskQueue {
    async fn push(&self, task: SendTask) -> Result<()> {
        let data = self.serialize_task(&task)?;
        self.kv_store.set(&self.task_key(&task.task_id), &data).await?;
        Ok(())
    }

    async fn pop(&self) -> Result<Option<SendTask>> {
        let tasks = self.get_all_tasks().await?;
        if let Some(task) = tasks.into_iter().next() {
            self.kv_store.delete(&self.task_key(&task.task_id)).await?;
            Ok(Some(task))
        } else {
            Ok(None)
        }
    }

    async fn batch_push(&self, tasks: Vec<SendTask>) -> Result<()> {
        for task in tasks {
            self.push(task).await?;
        }
        Ok(())
    }

    async fn batch_pop(&self, limit: usize) -> Result<Vec<SendTask>> {
        let mut tasks = self.get_all_tasks().await?;
        tasks.truncate(limit);
        for task in &tasks {
            self.kv_store.delete(&self.task_key(&task.task_id)).await?;
        }
        Ok(tasks)
    }

    async fn remove(&self, task_id: &str) -> Result<()> {
        self.kv_store.delete(&self.task_key(task_id)).await?;
        Ok(())
    }

    async fn clear(&self) -> Result<()> {
        let tasks = self.get_all_tasks().await?;
        for task in tasks {
            self.kv_store.delete(&self.task_key(&task.task_id)).await?;
        }
        Ok(())
    }

    async fn size(&self) -> Result<usize> {
        let tasks = self.get_all_tasks().await?;
        Ok(tasks.len())
    }

    async fn is_empty(&self) -> Result<bool> {
        let tasks = self.get_all_tasks().await?;
        Ok(tasks.is_empty())
    }

    async fn stats(&self) -> Result<QueueStats> {
        let tasks = self.get_all_tasks().await?;
        let mut stats = QueueStats {
            total_tasks: tasks.len(),
            pending_tasks: 0,
            processing_tasks: 0,
            completed_tasks: 0,
            failed_tasks: 0,
            expired_tasks: 0,
            avg_processing_time_ms: 0.0,
            priority_distribution: HashMap::new(),
        };

        for task in &tasks {
            match task.status {
                TaskStatus::Pending => stats.pending_tasks += 1,
                TaskStatus::Processing => stats.processing_tasks += 1,
                TaskStatus::Completed => stats.completed_tasks += 1,
                TaskStatus::Failed => stats.failed_tasks += 1,
                TaskStatus::Expired => stats.expired_tasks += 1,
                TaskStatus::Cancelled => {}, // 已取消的任务不计入统计
            }
            *stats.priority_distribution.entry(task.priority).or_insert(0) += 1;
        }

        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::send_task::MessageData;
    
    #[tokio::test]
    async fn test_memory_queue_basic_operations() {
        let queue = MemoryTaskQueue::new();
        
        // 创建测试任务（id = message.id，无值时用 0）
        let message_data = MessageData::new(
            123,
            456,
            1,
            789,
            "Hello, world!".to_string(),
            1,
            0,
        );
        
        let task = SendTask::from_message_data(message_data);
        
        // 测试推送
        assert!(queue.push(task.clone()).await.is_ok());
        assert_eq!(queue.size().await.unwrap(), 1);
        
        // 测试弹出
        let popped_task = queue.pop().await.unwrap();
        assert!(popped_task.is_some());
        assert_eq!(popped_task.unwrap().task_id, task.task_id);
        assert_eq!(queue.size().await.unwrap(), 0);
    }
    
    #[tokio::test]
    async fn test_priority_ordering() {
        let queue = MemoryTaskQueue::new();
        
        // 创建不同优先级的任务（id = message.id）
        let high_priority_data = MessageData::new(
            1,
            1,
            1,
            1,
            "High priority".to_string(),
            1, // 高优先级
            0,
        );
        
        let low_priority_data = MessageData::new(
            2,
            2,
            1,
            2,
            "Low priority".to_string(),
            5, // 低优先级
            0,
        );
        
        let high_task = SendTask::from_message_data(high_priority_data);
        let low_task = SendTask::from_message_data(low_priority_data);
        
        // 先推送低优先级任务
        queue.push(low_task).await.unwrap();
        // 再推送高优先级任务
        queue.push(high_task).await.unwrap();
        
        // 弹出的应该是高优先级任务
        let popped_task = queue.pop().await.unwrap().unwrap();
        assert_eq!(popped_task.id, 1);
        assert_eq!(popped_task.priority, QueuePriority::High);
    }
} 

#[derive(Debug)]
pub struct SendQueueManager {
    task_sender: Sender<SendTask>,
    task_receiver: Receiver<SendTask>,
    db: Arc<Db>,
}

impl SendQueueManager {
    pub fn new(db: Arc<Db>) -> Self {
        let (task_sender, task_receiver) = unbounded();
        Self {
            task_sender,
            task_receiver,
            db,
        }
    }
    
    pub fn enqueue_task(&self, task: SendTask) {
        info!("🔍 enqueue_task: 加入队列 id={}", task.id);
        if let Err(e) = self.task_sender.send(task) {
            error!("发送任务到队列失败: {}", e);
        } else {
            info!("✅ enqueue_task: 任务已加入内存队列");
        }
    }
    
    pub fn dequeue_task(&self) -> Option<SendTask> {
        let result = self.task_receiver.try_recv().ok();
        if result.is_some() {
            info!("✅ dequeue_task: 成功取出任务");
        }
        result
    }
    
    pub fn persist_task(&self, task: &SendTask) -> Result<()> {
        let key = format!("task:{}", task.id);
        let data = bincode::serialize(task)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化任务失败: {}", e)))?;
        
        self.db.insert(key.as_bytes(), data)
            .map_err(|e| PrivchatSDKError::KvStore(format!("持久化任务失败: {}", e)))?;
        
        Ok(())
    }
    
    pub fn load_task(&self, id: &str) -> Result<Option<SendTask>> {
        let key = format!("task:{}", id);
        let result = self.db.get(key.as_bytes())
            .map_err(|e| PrivchatSDKError::KvStore(format!("加载任务失败: {}", e)))?;
        
        match result {
            Some(data) => {
                let task = bincode::deserialize(&data)
                    .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化任务失败: {}", e)))?;
                Ok(Some(task))
            }
            None => Ok(None),
        }
    }
}

#[async_trait::async_trait]
impl TaskQueueTrait for SendQueueManager {
    async fn push(&self, task: SendTask) -> Result<()> {
        self.enqueue_task(task);
        Ok(())
    }
    
    async fn pop(&self) -> Result<Option<SendTask>> {
        Ok(self.dequeue_task())
    }
    
    async fn batch_push(&self, tasks: Vec<SendTask>) -> Result<()> {
        for task in tasks {
            self.enqueue_task(task);
        }
        Ok(())
    }
    
    async fn batch_pop(&self, limit: usize) -> Result<Vec<SendTask>> {
        let mut tasks = Vec::with_capacity(limit);
        for _ in 0..limit {
            if let Some(task) = self.dequeue_task() {
                info!("🔍 Dequeued task: id={}", task.id);
                tasks.push(task);
            } else {
                break;
            }
        }
        if !tasks.is_empty() {
            info!("✅ batch_pop: 拉取了 {} 个任务", tasks.len());
        }
        Ok(tasks)
    }
    
    async fn remove(&self, _task_id: &str) -> Result<()> {
        // TODO: 实现任务移除
        Ok(())
    }
    
    async fn clear(&self) -> Result<()> {
        // 清空内存队列
        while self.dequeue_task().is_some() {}
        Ok(())
    }
    
    async fn size(&self) -> Result<usize> {
        // 无法获取 crossbeam channel 的准确大小，返回近似值
        Ok(0)
    }
    
    async fn is_empty(&self) -> Result<bool> {
        Ok(self.dequeue_task().is_none())
    }
    
    async fn stats(&self) -> Result<QueueStats> {
        Ok(QueueStats {
            total_tasks: 0,
            pending_tasks: 0,
            processing_tasks: 0,
            completed_tasks: 0,
            failed_tasks: 0,
            expired_tasks: 0,
            avg_processing_time_ms: 0.0,
            priority_distribution: HashMap::new(),
        })
    }
} 