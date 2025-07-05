use serde::{Deserialize, Serialize};
use std::fmt;

/// 队列优先级枚举
/// 
/// 优先级决定了消息在发送队列中的处理顺序：
/// - Critical: 最高优先级，立即处理（撤回、删除等紧急操作）
/// - High: 高优先级，快速处理（文本消息、表情等用户直接感知的内容）
/// - Normal: 普通优先级，正常处理（图片、语音等媒体消息）
/// - Low: 低优先级，可延迟处理（大文件、视频等）
/// - Background: 后台优先级，最低优先级（已读回执、状态同步等）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum QueuePriority {
    Critical = 0,   // 撤回、删除
    High = 1,       // 文本消息、表情
    Normal = 2,     // 图片、语音
    Low = 3,        // 文件、视频
    Background = 4, // 已读回执、状态同步
}

impl QueuePriority {
    /// 根据消息类型获取优先级
    pub fn from_message_type(message_type: i32) -> Self {
        match message_type {
            // 系统消息类型
            1000..=1999 => QueuePriority::Critical,
            
            // 撤回消息
            2000 => QueuePriority::Critical,
            
            // 文本消息
            1 => QueuePriority::High,
            
            // 表情消息
            2 => QueuePriority::High,
            
            // 图片消息
            3 => QueuePriority::Normal,
            
            // 语音消息
            4 => QueuePriority::Normal,
            
            // 视频消息
            5 => QueuePriority::Low,
            
            // 文件消息
            6 => QueuePriority::Low,
            
            // 位置消息
            7 => QueuePriority::Normal,
            
            // 名片消息
            8 => QueuePriority::Normal,
            
            // 已读回执
            9 => QueuePriority::Background,
            
            // 正在输入状态
            10 => QueuePriority::Background,
            
            // 自定义消息
            100..=999 => QueuePriority::Normal,
            
            // 其他类型默认为普通优先级
            _ => QueuePriority::Normal,
        }
    }
    
    /// 根据操作类型获取优先级
    pub fn from_operation_type(operation: &str) -> Self {
        match operation {
            "revoke" | "delete" | "recall" => QueuePriority::Critical,
            "edit" | "update" => QueuePriority::High,
            "send" => QueuePriority::Normal,
            "upload" => QueuePriority::Low,
            "read_receipt" | "typing_status" | "sync" => QueuePriority::Background,
            _ => QueuePriority::Normal,
        }
    }
    
    /// 获取优先级的数值
    pub fn value(&self) -> u8 {
        *self as u8
    }
    
    /// 从数值创建优先级
    pub fn from_value(value: u8) -> Option<Self> {
        match value {
            0 => Some(QueuePriority::Critical),
            1 => Some(QueuePriority::High),
            2 => Some(QueuePriority::Normal),
            3 => Some(QueuePriority::Low),
            4 => Some(QueuePriority::Background),
            _ => None,
        }
    }
    
    /// 获取优先级的显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            QueuePriority::Critical => "关键",
            QueuePriority::High => "高",
            QueuePriority::Normal => "普通",
            QueuePriority::Low => "低",
            QueuePriority::Background => "后台",
        }
    }
    
    /// 获取优先级的英文名称
    pub fn name(&self) -> &'static str {
        match self {
            QueuePriority::Critical => "critical",
            QueuePriority::High => "high",
            QueuePriority::Normal => "normal",
            QueuePriority::Low => "low",
            QueuePriority::Background => "background",
        }
    }
    
    /// 检查是否为高优先级（Critical 或 High）
    pub fn is_high_priority(&self) -> bool {
        matches!(self, QueuePriority::Critical | QueuePriority::High)
    }
    
    /// 检查是否为低优先级（Low 或 Background）
    pub fn is_low_priority(&self) -> bool {
        matches!(self, QueuePriority::Low | QueuePriority::Background)
    }
    
    /// 检查是否为后台优先级
    pub fn is_background(&self) -> bool {
        matches!(self, QueuePriority::Background)
    }
    
    /// 获取处理超时时间（毫秒）
    /// 
    /// 不同优先级的消息有不同的处理超时时间：
    /// - Critical: 5秒
    /// - High: 10秒
    /// - Normal: 30秒
    /// - Low: 60秒
    /// - Background: 120秒
    pub fn timeout_ms(&self) -> u64 {
        match self {
            QueuePriority::Critical => 5_000,
            QueuePriority::High => 10_000,
            QueuePriority::Normal => 30_000,
            QueuePriority::Low => 60_000,
            QueuePriority::Background => 120_000,
        }
    }
    
    /// 获取最大重试次数
    pub fn max_retries(&self) -> u32 {
        match self {
            QueuePriority::Critical => 5,    // 关键消息多重试
            QueuePriority::High => 3,        // 高优先级消息适中重试
            QueuePriority::Normal => 3,      // 普通消息适中重试
            QueuePriority::Low => 2,         // 低优先级消息少重试
            QueuePriority::Background => 1,  // 后台消息最少重试
        }
    }
    
    /// 获取重试间隔基数（秒）
    /// 
    /// 实际重试间隔 = base_interval * (2 ^ retry_count)
    pub fn retry_base_interval(&self) -> u64 {
        match self {
            QueuePriority::Critical => 1,    // 关键消息快速重试
            QueuePriority::High => 2,        // 高优先级适中重试
            QueuePriority::Normal => 3,      // 普通消息正常重试
            QueuePriority::Low => 5,         // 低优先级慢重试
            QueuePriority::Background => 10, // 后台消息最慢重试
        }
    }
    
    /// 获取所有优先级的列表
    pub fn all() -> Vec<Self> {
        vec![
            QueuePriority::Critical,
            QueuePriority::High,
            QueuePriority::Normal,
            QueuePriority::Low,
            QueuePriority::Background,
        ]
    }
}

impl fmt::Display for QueuePriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

impl Default for QueuePriority {
    fn default() -> Self {
        QueuePriority::Normal
    }
}

impl From<u8> for QueuePriority {
    fn from(value: u8) -> Self {
        QueuePriority::from_value(value).unwrap_or(QueuePriority::Normal)
    }
}

impl From<QueuePriority> for u8 {
    fn from(priority: QueuePriority) -> Self {
        priority.value()
    }
}

/// 优先级比较器
/// 
/// 用于队列排序，确保高优先级消息先处理
pub struct PriorityComparator;

impl PriorityComparator {
    /// 比较两个优先级
    /// 
    /// 返回值：
    /// - `std::cmp::Ordering::Less`: a 优先级低于 b
    /// - `std::cmp::Ordering::Equal`: a 优先级等于 b
    /// - `std::cmp::Ordering::Greater`: a 优先级高于 b
    pub fn compare(a: QueuePriority, b: QueuePriority) -> std::cmp::Ordering {
        a.value().cmp(&b.value())
    }
    
    /// 检查优先级 a 是否高于优先级 b
    pub fn is_higher(a: QueuePriority, b: QueuePriority) -> bool {
        a.value() < b.value()
    }
    
    /// 检查优先级 a 是否低于优先级 b
    pub fn is_lower(a: QueuePriority, b: QueuePriority) -> bool {
        a.value() > b.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_priority_ordering() {
        assert!(QueuePriority::Critical < QueuePriority::High);
        assert!(QueuePriority::High < QueuePriority::Normal);
        assert!(QueuePriority::Normal < QueuePriority::Low);
        assert!(QueuePriority::Low < QueuePriority::Background);
    }
    
    #[test]
    fn test_priority_from_message_type() {
        assert_eq!(QueuePriority::from_message_type(1), QueuePriority::High);      // 文本
        assert_eq!(QueuePriority::from_message_type(3), QueuePriority::Normal);    // 图片
        assert_eq!(QueuePriority::from_message_type(5), QueuePriority::Low);       // 视频
        assert_eq!(QueuePriority::from_message_type(9), QueuePriority::Background); // 已读回执
        assert_eq!(QueuePriority::from_message_type(2000), QueuePriority::Critical); // 撤回
    }
    
    #[test]
    fn test_priority_from_operation_type() {
        assert_eq!(QueuePriority::from_operation_type("revoke"), QueuePriority::Critical);
        assert_eq!(QueuePriority::from_operation_type("edit"), QueuePriority::High);
        assert_eq!(QueuePriority::from_operation_type("send"), QueuePriority::Normal);
        assert_eq!(QueuePriority::from_operation_type("upload"), QueuePriority::Low);
        assert_eq!(QueuePriority::from_operation_type("read_receipt"), QueuePriority::Background);
    }
    
    #[test]
    fn test_priority_helpers() {
        assert!(QueuePriority::Critical.is_high_priority());
        assert!(QueuePriority::High.is_high_priority());
        assert!(!QueuePriority::Normal.is_high_priority());
        
        assert!(QueuePriority::Low.is_low_priority());
        assert!(QueuePriority::Background.is_low_priority());
        assert!(!QueuePriority::Normal.is_low_priority());
        
        assert!(QueuePriority::Background.is_background());
        assert!(!QueuePriority::Low.is_background());
    }
    
    #[test]
    fn test_priority_timeouts() {
        assert_eq!(QueuePriority::Critical.timeout_ms(), 5_000);
        assert_eq!(QueuePriority::High.timeout_ms(), 10_000);
        assert_eq!(QueuePriority::Normal.timeout_ms(), 30_000);
        assert_eq!(QueuePriority::Low.timeout_ms(), 60_000);
        assert_eq!(QueuePriority::Background.timeout_ms(), 120_000);
    }
    
    #[test]
    fn test_priority_retries() {
        assert_eq!(QueuePriority::Critical.max_retries(), 5);
        assert_eq!(QueuePriority::High.max_retries(), 3);
        assert_eq!(QueuePriority::Normal.max_retries(), 3);
        assert_eq!(QueuePriority::Low.max_retries(), 2);
        assert_eq!(QueuePriority::Background.max_retries(), 1);
    }
    
    #[test]
    fn test_priority_comparator() {
        assert!(PriorityComparator::is_higher(QueuePriority::Critical, QueuePriority::High));
        assert!(PriorityComparator::is_lower(QueuePriority::Low, QueuePriority::Normal));
        assert_eq!(
            PriorityComparator::compare(QueuePriority::High, QueuePriority::Normal),
            std::cmp::Ordering::Less
        );
    }
} 