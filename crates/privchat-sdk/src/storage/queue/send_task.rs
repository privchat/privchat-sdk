use crate::storage::queue::priority::QueuePriority;
use crate::storage::queue::retry_policy::SendFailureReason;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// 消息数据结构（用于发送队列）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageData {
    /// message.id（SQLite 主键），所有操作只用此 id
    pub id: i64,
    /// 频道ID
    pub channel_id: u64,  // u64，与服务端一致
    /// 频道类型 (1: 个人聊天, 2: 群聊)
    pub channel_type: i32,
    /// 发送方用户ID
    pub from_uid: u64,  // u64，与服务端一致
    /// 消息内容
    pub content: String,
    /// 消息类型 (1: 文本, 2: 图片, 3: 语音, 等)
    pub message_type: i32,
    /// 扩展数据
    pub extra: HashMap<String, String>,
    /// 消息创建时间戳
    pub created_at: u64,
    /// 消息过期时间戳 (可选)
    pub expires_at: Option<u64>,
    /// 客户端雪花算法生成的 local_message_id，发往服务端用于去重与幂等
    #[serde(default)]
    pub local_message_id: u64,
}

impl MessageData {
    /// 创建新的消息数据
    /// - `local_message_id`: 雪花算法生成的 ID，发往服务端；传 0 时 send_consumer 会用 id 作为回退（兼容旧任务）
    pub fn new(
        id: i64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
        message_type: i32,
        local_message_id: u64,
    ) -> Self {
        Self {
            id,
            channel_id,
            channel_type,
            from_uid,
            content,
            message_type,
            extra: HashMap::new(),
            created_at: chrono::Utc::now().timestamp_millis() as u64,
            expires_at: None,
            local_message_id,
        }
    }
    
    /// 设置扩展数据
    pub fn with_extra(mut self, key: String, value: String) -> Self {
        self.extra.insert(key, value);
        self
    }
    
    /// 设置过期时间
    pub fn with_expires_at(mut self, expires_at: u64) -> Self {
        self.expires_at = Some(expires_at);
        self
    }
    
    /// 检查消息是否已过期
    pub fn is_expired(&self) -> bool {
        if let Some(expires_at) = self.expires_at {
            chrono::Utc::now().timestamp_millis() as u64 > expires_at
        } else {
            false
        }
    }
    
    /// 获取消息大小估算 (用于批量处理和限流)
    pub fn estimated_size(&self) -> usize {
        self.content.len() + 
        self.extra.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() + 
        200 // 基础字段大小估算
    }
    
    /// 检查是否为关键消息 (撤回、删除等)
    pub fn is_critical(&self) -> bool {
        self.message_type >= 1000 && self.message_type < 2000
    }
}

/// 发送任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendTask {
    pub task_id: String,
    /// message.id（SQLite 主键），所有操作只用此 id
    pub id: i64,
    pub channel_id: u64,  // u64，与服务端一致
    pub message_data: MessageData,
    pub created_at: u64,
    pub retry_count: u32,
    pub max_retries: u32,
    pub next_retry_at: Option<u64>,
    pub priority: QueuePriority,
    pub status: TaskStatus, 
    pub last_error: Option<String>, 
    pub last_failure_reason: Option<SendFailureReason>, 
    pub timeout_at: u64, 
    pub extra_data: HashMap<String, String>, 
}

/// 任务状态枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    /// 等待处理
    Pending,
    /// 正在处理
    Processing,
    /// 处理完成
    Completed,
    /// 处理失败
    Failed,
    /// 已取消
    Cancelled,
    /// 已过期
    Expired,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "等待处理"),
            TaskStatus::Processing => write!(f, "正在处理"),
            TaskStatus::Completed => write!(f, "处理完成"),
            TaskStatus::Failed => write!(f, "处理失败"),
            TaskStatus::Cancelled => write!(f, "已取消"),
            TaskStatus::Expired => write!(f, "已过期"),
        }
    }
}

impl SendTask {
    /// 创建新的发送任务
    pub fn new(
        id: i64,
        channel_id: u64,
        message_data: MessageData,
        priority: QueuePriority,
    ) -> Self {
        let created_at = chrono::Utc::now().timestamp_millis() as u64;
        let max_retries = priority.max_retries();
        let timeout_at = created_at + priority.timeout_ms();
        let task_id = format!("{}", id);
        
        Self {
            task_id,
            id,
            channel_id,
            message_data,
            created_at,
            retry_count: 0,
            max_retries,
            next_retry_at: None,
            priority,
            status: TaskStatus::Pending,
            last_error: None,
            last_failure_reason: None,
            timeout_at,
            extra_data: HashMap::new(),
        }
    }
    
    /// 从消息数据创建发送任务
    pub fn from_message_data(message_data: MessageData) -> Self {
        let priority = QueuePriority::from_message_type(message_data.message_type);
        Self::new(message_data.id, message_data.channel_id, message_data, priority)
    }
    
    /// 检查任务是否已过期
    pub fn is_expired(&self) -> bool {
        chrono::Utc::now().timestamp_millis() as u64 > self.timeout_at
    }
    
    /// 检查是否可以重试
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries && 
        !self.is_expired() &&
        matches!(self.status, TaskStatus::Pending | TaskStatus::Failed)
    }
    
    /// 检查是否应该重试 (到达重试时间)
    pub fn should_retry(&self) -> bool {
        if !self.can_retry() {
            return false;
        }
        
        if let Some(next_retry_at) = self.next_retry_at {
            chrono::Utc::now().timestamp_millis() as u64 >= next_retry_at
        } else {
            true
        }
    }
    
    /// 增加重试次数并计算下次重试时间
    pub fn increment_retry(&mut self) {
        self.retry_count += 1;
        
        if self.retry_count < self.max_retries {
            // 计算下次重试时间 (指数退避)
            let base_interval = self.priority.retry_base_interval();
            let backoff_multiplier = 2u64.pow(self.retry_count);
            let retry_delay = base_interval * backoff_multiplier;
            
            // 添加随机抖动，避免所有任务同时重试
            let jitter = (rand::random::<f64>() * 0.1 + 0.95) as u64; // 95-105% 的随机抖动
            let final_delay = (retry_delay * jitter).min(300); // 最大延迟 5 分钟
            
            self.next_retry_at = Some(
                chrono::Utc::now().timestamp_millis() as u64 + final_delay * 1000
            );
            self.status = TaskStatus::Pending;
        } else {
            self.status = TaskStatus::Failed;
            self.next_retry_at = None;
        }
    }
    
    /// 标记任务为处理中
    pub fn mark_processing(&mut self) {
        self.status = TaskStatus::Processing;
    }
    
    /// 标记任务为已完成
    pub fn mark_completed(&mut self) {
        self.status = TaskStatus::Completed;
        self.next_retry_at = None;
        self.last_error = None;
    }
    
    /// 标记任务为失败
    pub fn mark_failed(&mut self, error: String, failure_reason: Option<SendFailureReason>) {
        self.status = TaskStatus::Failed;
        self.last_error = Some(error);
        self.last_failure_reason = failure_reason;
    }
    
    /// 标记任务为已取消
    pub fn mark_cancelled(&mut self) {
        self.status = TaskStatus::Cancelled;
        self.next_retry_at = None;
    }
    
    /// 标记任务为已过期
    pub fn mark_expired(&mut self) {
        self.status = TaskStatus::Expired;
        self.next_retry_at = None;
    }
    
    /// 获取任务年龄 (毫秒)
    pub fn age_ms(&self) -> u64 {
        chrono::Utc::now().timestamp_millis() as u64 - self.created_at
    }
    
    /// 获取剩余超时时间 (毫秒)
    pub fn remaining_timeout_ms(&self) -> i64 {
        self.timeout_at as i64 - chrono::Utc::now().timestamp_millis() as i64
    }
    
    /// 获取下次重试剩余时间 (毫秒)
    pub fn remaining_retry_ms(&self) -> Option<i64> {
        self.next_retry_at.map(|retry_at| {
            retry_at as i64 - chrono::Utc::now().timestamp_millis() as i64
        })
    }
    
    /// 设置扩展数据
    pub fn set_extra(&mut self, key: String, value: String) {
        self.extra_data.insert(key, value);
    }
    
    /// 获取扩展数据
    pub fn get_extra(&self, key: &str) -> Option<&String> {
        self.extra_data.get(key)
    }
    
    /// 检查是否为高优先级任务
    pub fn is_high_priority(&self) -> bool {
        self.priority.is_high_priority()
    }
    
    /// 检查是否为低优先级任务
    pub fn is_low_priority(&self) -> bool {
        self.priority.is_low_priority()
    }
    
    /// 检查是否为后台任务
    pub fn is_background(&self) -> bool {
        self.priority.is_background()
    }
    
    /// 获取任务的详细信息字符串
    pub fn details(&self) -> String {
        format!(
            "SendTask(id={}, channel={}, type={}, priority={}, status={}, retry={}/{}, age={}ms)",
            self.id,
            self.channel_id,
            self.message_data.message_type,
            self.priority,
            self.status,
            self.retry_count,
            self.max_retries,
            self.age_ms()
        )
    }
}

/// 任务比较器 - 用于优先级队列排序
/// 
/// 排序规则：
/// 1. 优先级高的任务先处理
/// 2. 相同优先级下，创建时间早的先处理
/// 3. 关键消息优先处理
impl Ord for SendTask {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 首先按优先级排序 (数值越小优先级越高)
        let priority_cmp = self.priority.cmp(&other.priority);
        if priority_cmp != std::cmp::Ordering::Equal {
            return priority_cmp;
        }
        
        // 然后按创建时间排序 (早创建的先处理)
        self.created_at.cmp(&other.created_at)
    }
}

impl PartialOrd for SendTask {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for SendTask {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for SendTask {}

/// 任务过滤器
pub struct TaskFilter {
    pub channel_id: Option<u64>,  // u64，与服务端一致
    pub priority: Option<QueuePriority>,
    pub status: Option<TaskStatus>,
    pub max_age_ms: Option<u64>,
    pub max_retry_count: Option<u32>,
}

impl TaskFilter {
    /// 创建新的过滤器
    pub fn new() -> Self {
        Self {
            channel_id: None,
            priority: None,
            status: None,
            max_age_ms: None,
            max_retry_count: None,
        }
    }
    
    /// 设置频道ID过滤
    pub fn with_channel_id(mut self, channel_id: u64) -> Self {
        self.channel_id = Some(channel_id);
        self
    }
    
    /// 设置优先级过滤
    pub fn with_priority(mut self, priority: QueuePriority) -> Self {
        self.priority = Some(priority);
        self
    }
    
    /// 设置状态过滤
    pub fn with_status(mut self, status: TaskStatus) -> Self {
        self.status = Some(status);
        self
    }
    
    /// 设置最大年龄过滤
    pub fn with_max_age_ms(mut self, max_age_ms: u64) -> Self {
        self.max_age_ms = Some(max_age_ms);
        self
    }
    
    /// 设置最大重试次数过滤
    pub fn with_max_retry_count(mut self, max_retry_count: u32) -> Self {
        self.max_retry_count = Some(max_retry_count);
        self
    }
    
    /// 检查任务是否匹配过滤条件
    pub fn matches(&self, task: &SendTask) -> bool {
        if let Some(ref channel_id) = self.channel_id {
            if task.channel_id != *channel_id {
                return false;
            }
        }
        
        if let Some(priority) = self.priority {
            if task.priority != priority {
                return false;
            }
        }
        
        if let Some(status) = self.status {
            if task.status != status {
                return false;
            }
        }
        
        if let Some(max_age_ms) = self.max_age_ms {
            if task.age_ms() > max_age_ms {
                return false;
            }
        }
        
        if let Some(max_retry_count) = self.max_retry_count {
            if task.retry_count > max_retry_count {
                return false;
            }
        }
        
        true
    }
}

impl Default for TaskFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_message_data_creation() {
        let message_data = MessageData::new(123, 456, 1, 789, "Hello, world!".to_string(), 1, 0);
        assert_eq!(message_data.id, 123);
        assert_eq!(message_data.channel_id, 456);
        assert_eq!(message_data.channel_type, 1);
        assert_eq!(message_data.from_uid, 789);
        assert_eq!(message_data.content, "Hello, world!");
        assert_eq!(message_data.message_type, 1);
        assert!(!message_data.is_expired());
    }
    
    #[test]
    fn test_send_task_creation() {
        let message_data = MessageData::new(123, 456, 1, 789, "Hello, world!".to_string(), 1, 0);
        let task = SendTask::from_message_data(message_data);
        assert_eq!(task.id, 123);
        assert_eq!(task.retry_count, 0);
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.priority, QueuePriority::High);
        assert!(task.can_retry());
        assert!(!task.is_expired());
    }
    
    #[test]
    fn test_task_retry_logic() {
        let message_data = MessageData::new(123, 456, 1, 789, "Hello, world!".to_string(), 1, 0);
        let mut task = SendTask::from_message_data(message_data);
        task.mark_failed("Network error".to_string(), None);
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.can_retry());
        task.increment_retry();
        assert_eq!(task.retry_count, 1);
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(task.next_retry_at.is_some());
    }
    
    #[test]
    fn test_task_ordering() {
        let message_data1 = MessageData::new(1, 1, 1, 1, "Message 1".to_string(), 1, 0);
        let message_data2 = MessageData::new(2, 2, 1, 2, "Message 2".to_string(), 5, 0);
        let task1 = SendTask::from_message_data(message_data1);
        let task2 = SendTask::from_message_data(message_data2);
        assert!(task1 < task2);
    }
    
    #[test]
    fn test_task_filter() {
        let message_data = MessageData::new(123, 456, 1, 789, "Hello, world!".to_string(), 1, 0);
        let task = SendTask::from_message_data(message_data);
        let filter = TaskFilter::new().with_channel_id(456).with_priority(QueuePriority::High).with_status(TaskStatus::Pending);
        assert!(filter.matches(&task));
        let filter2 = TaskFilter::new().with_channel_id(789);
        assert!(!filter2.matches(&task));
    }
} 