//! 事件系统模块 - 处理 IM SDK 中的各种事件
//! 
//! 功能包括：
//! - 消息状态变更事件
//! - 已读回执事件
//! - 消息撤回事件
//! - 消息编辑事件
//! - Typing Indicator 事件
//! - 事件广播和订阅机制

use crate::storage::advanced_features::{ReadReceiptEvent, MessageRevokeEvent, MessageEditEvent};
use crate::storage::entities::MessageStatus;
use crate::storage::typing::TypingEvent;
use crate::storage::reaction::ReactionEvent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

/// SDK 事件类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SDKEvent {
    /// 消息状态变更
    MessageStatusChanged {
        message_id: String,
        old_status: MessageStatus,
        new_status: MessageStatus,
        timestamp: u64,
    },
    /// 已读回执接收
    ReadReceiptReceived(ReadReceiptEvent),
    /// 消息撤回
    MessageRevoked(MessageRevokeEvent),
    /// 消息编辑
    MessageEdited(MessageEditEvent),
    /// 用户开始输入
    TypingStarted(TypingEvent),
    /// 用户停止输入
    TypingStopped(TypingEvent),
    /// 表情反馈添加
    ReactionAdded(ReactionEvent),
    /// 表情反馈移除
    ReactionRemoved(ReactionEvent),
    /// 正在输入指示器（通用）
    TypingIndicator(TypingEvent),
    /// 用户在线状态变更
    UserPresenceChanged {
        user_id: String,
        is_online: bool,
        last_seen: Option<u64>,
        timestamp: u64,
    },
    /// 未读数变更
    UnreadCountChanged {
        channel_id: String,
        channel_type: i32,
        unread_count: i32,
        timestamp: u64,
    },
    /// 连接状态变更
    ConnectionStateChanged {
        old_state: ConnectionState,
        new_state: ConnectionState,
        timestamp: u64,
    },
    /// 消息接收
    MessageReceived {
        message_id: String,
        channel_id: String,
        channel_type: i32,
        from_uid: String,
        timestamp: u64,
    },
    /// 消息发送成功
    MessageSent {
        message_id: String,
        channel_id: String,
        channel_type: i32,
        timestamp: u64,
    },
    /// 消息发送失败
    MessageSendFailed {
        message_id: String,
        channel_id: String,
        channel_type: i32,
        error: String,
        timestamp: u64,
    },
}

/// 连接状态枚举
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConnectionState {
    Connected,
    Disconnected,
    Connecting,
    Reconnecting,
}

impl SDKEvent {
    /// 获取事件类型字符串
    pub fn event_type(&self) -> &'static str {
        match self {
            SDKEvent::MessageStatusChanged { .. } => "message_status_changed",
            SDKEvent::ReadReceiptReceived(_) => "read_receipt_received",
            SDKEvent::MessageRevoked(_) => "message_revoked",
            SDKEvent::MessageEdited(_) => "message_edited",
            SDKEvent::TypingStarted(_) => "typing_started",
            SDKEvent::TypingStopped(_) => "typing_stopped",
            SDKEvent::ReactionAdded(_) => "reaction_added",
            SDKEvent::ReactionRemoved(_) => "reaction_removed",
            SDKEvent::TypingIndicator(_) => "typing_indicator",
            SDKEvent::UserPresenceChanged { .. } => "user_presence_changed",
            SDKEvent::UnreadCountChanged { .. } => "unread_count_changed",
            SDKEvent::ConnectionStateChanged { .. } => "connection_state_changed",
            SDKEvent::MessageReceived { .. } => "message_received",
            SDKEvent::MessageSent { .. } => "message_sent",
            SDKEvent::MessageSendFailed { .. } => "message_send_failed",
        }
    }

    /// 获取事件关联的频道ID
    pub fn channel_id(&self) -> Option<&str> {
        match self {
            SDKEvent::MessageStatusChanged { .. } => None, // 消息状态变更事件可能没有频道信息
            SDKEvent::ReadReceiptReceived(event) => Some(&event.channel_id),
            SDKEvent::MessageRevoked(event) => Some(&event.channel_id),
            SDKEvent::MessageEdited(event) => Some(&event.channel_id),
            SDKEvent::TypingStarted(event) => Some(&event.channel_id),
            SDKEvent::TypingStopped(event) => Some(&event.channel_id),
            SDKEvent::ReactionAdded(event) => Some(&event.channel_id),
            SDKEvent::ReactionRemoved(event) => Some(&event.channel_id),
            SDKEvent::TypingIndicator(event) => Some(&event.channel_id),
            SDKEvent::UnreadCountChanged { channel_id, .. } => Some(channel_id),
            SDKEvent::MessageReceived { channel_id, .. } => Some(channel_id),
            SDKEvent::MessageSent { channel_id, .. } => Some(channel_id),
            SDKEvent::MessageSendFailed { channel_id, .. } => Some(channel_id),
            _ => None,
        }
    }

    /// 获取事件时间戳
    pub fn timestamp(&self) -> u64 {
        match self {
            SDKEvent::MessageStatusChanged { timestamp, .. } => *timestamp,
            SDKEvent::ReadReceiptReceived(event) => event.read_at,
            SDKEvent::MessageRevoked(event) => event.revoked_at,
            SDKEvent::MessageEdited(event) => event.edited_at,
            SDKEvent::TypingStarted(event) => event.timestamp,
            SDKEvent::TypingStopped(event) => event.timestamp,
            SDKEvent::ReactionAdded(event) => event.timestamp,
            SDKEvent::ReactionRemoved(event) => event.timestamp,
            SDKEvent::TypingIndicator(event) => event.timestamp,
            SDKEvent::UserPresenceChanged { timestamp, .. } => *timestamp,
            SDKEvent::UnreadCountChanged { timestamp, .. } => *timestamp,
            SDKEvent::ConnectionStateChanged { timestamp, .. } => *timestamp,
            SDKEvent::MessageReceived { timestamp, .. } => *timestamp,
            SDKEvent::MessageSent { timestamp, .. } => *timestamp,
            SDKEvent::MessageSendFailed { timestamp, .. } => *timestamp,
        }
    }

    /// 获取事件相关的用户ID
    pub fn user_id(&self) -> Option<&str> {
        match self {
            SDKEvent::ReadReceiptReceived(e) => Some(&e.reader_uid),
            SDKEvent::MessageRevoked(e) => Some(&e.revoker_uid),
            SDKEvent::MessageEdited(e) => Some(&e.editor_uid),
            SDKEvent::TypingStarted(e) => Some(&e.user_id),
            SDKEvent::TypingStopped(e) => Some(&e.user_id),
            SDKEvent::ReactionAdded(e) => Some(&e.user_id),
            SDKEvent::ReactionRemoved(e) => Some(&e.user_id),
            SDKEvent::TypingIndicator(e) => Some(&e.user_id),
            SDKEvent::UserPresenceChanged { user_id, .. } => Some(user_id),
            SDKEvent::MessageReceived { from_uid, .. } => Some(from_uid),
            _ => None,
        }
    }
}

/// 事件过滤器
#[derive(Debug, Clone)]
pub struct EventFilter {
    /// 事件类型过滤器
    pub event_types: Option<Vec<String>>,
    /// 频道ID过滤器
    pub channel_ids: Option<Vec<String>>,
    /// 用户ID过滤器
    pub user_ids: Option<Vec<String>>,
}

impl EventFilter {
    /// 创建新的事件过滤器
    pub fn new() -> Self {
        Self {
            event_types: None,
            channel_ids: None,
            user_ids: None,
        }
    }

    /// 添加事件类型过滤
    pub fn with_event_types(mut self, event_types: Vec<String>) -> Self {
        self.event_types = Some(event_types);
        self
    }

    /// 添加频道ID过滤
    pub fn with_channel_ids(mut self, channel_ids: Vec<String>) -> Self {
        self.channel_ids = Some(channel_ids);
        self
    }

    /// 添加用户ID过滤
    pub fn with_user_ids(mut self, user_ids: Vec<String>) -> Self {
        self.user_ids = Some(user_ids);
        self
    }

    /// 检查事件是否匹配过滤器
    pub fn matches(&self, event: &SDKEvent) -> bool {
        // 检查事件类型
        if let Some(ref types) = self.event_types {
            if !types.contains(&event.event_type().to_string()) {
                return false;
            }
        }

        // 检查频道ID
        if let Some(ref channel_ids) = self.channel_ids {
            if let Some(channel_id) = event.channel_id() {
                if !channel_ids.contains(&channel_id.to_string()) {
                    return false;
                }
            } else {
                return false; // 事件没有频道ID但过滤器要求有
            }
        }

        // 检查用户ID
        if let Some(ref user_ids) = self.user_ids {
            let event_user_id = match event {
                SDKEvent::ReadReceiptReceived(e) => Some(&e.reader_uid),
                SDKEvent::MessageRevoked(e) => Some(&e.revoker_uid),
                SDKEvent::MessageEdited(e) => Some(&e.editor_uid),
                SDKEvent::TypingStarted(e) => Some(&e.user_id),
                SDKEvent::TypingStopped(e) => Some(&e.user_id),
                SDKEvent::ReactionAdded(e) => Some(&e.user_id),
                SDKEvent::ReactionRemoved(e) => Some(&e.user_id),
                SDKEvent::TypingIndicator(e) => Some(&e.user_id),
                SDKEvent::UserPresenceChanged { user_id, .. } => Some(user_id),
                SDKEvent::MessageReceived { from_uid, .. } => Some(from_uid),
                _ => None,
            };

            if let Some(user_id) = event_user_id {
                if !user_ids.contains(&user_id.to_string()) {
                    return false;
                }
            } else {
                return false; // 事件没有用户ID但过滤器要求有
            }
        }

        true
    }
}

/// 事件监听器类型
pub type EventListener = Box<dyn Fn(&SDKEvent) + Send + Sync>;

/// 事件管理器
pub struct EventManager {
    /// 广播发送器
    sender: broadcast::Sender<SDKEvent>,
    /// 事件监听器映射
    listeners: Arc<tokio::sync::RwLock<HashMap<String, Vec<EventListener>>>>,
    /// 事件统计
    stats: Arc<tokio::sync::RwLock<EventStats>>,
}

/// 事件统计信息
#[derive(Debug, Clone, Default)]
pub struct EventStats {
    /// 总事件数
    pub total_events: u64,
    /// 按类型分组的事件数
    pub events_by_type: HashMap<String, u64>,
    /// 监听器数量
    pub listener_count: usize,
    /// 最后事件时间
    pub last_event_time: Option<u64>,
}

impl EventManager {
    /// 创建新的事件管理器
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        
        Self {
            sender,
            listeners: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            stats: Arc::new(tokio::sync::RwLock::new(EventStats::default())),
        }
    }

    /// 发布事件
    pub async fn emit(&self, event: SDKEvent) {
        debug!("Emitting event: {}", event.event_type());
        
        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.total_events += 1;
            *stats.events_by_type.entry(event.event_type().to_string()).or_insert(0) += 1;
            stats.last_event_time = Some(event.timestamp());
        }

        // 广播事件
        if let Err(e) = self.sender.send(event.clone()) {
            warn!("Failed to broadcast event: {}", e);
        }

        // 调用监听器
        let listeners = self.listeners.read().await;
        if let Some(event_listeners) = listeners.get(event.event_type()) {
            for listener in event_listeners {
                listener(&event);
            }
        }
        
        // 调用通用监听器
        if let Some(general_listeners) = listeners.get("*") {
            for listener in general_listeners {
                listener(&event);
            }
        }
    }

    /// 订阅事件
    pub async fn subscribe(&self) -> broadcast::Receiver<SDKEvent> {
        self.sender.subscribe()
    }

    /// 订阅特定类型的事件
    pub async fn subscribe_filtered(&self, filter: EventFilter) -> FilteredEventReceiver {
        let receiver = self.sender.subscribe();
        FilteredEventReceiver::new(receiver, filter)
    }

    /// 添加事件监听器
    pub async fn add_listener<F>(&self, event_type: &str, listener: F)
    where
        F: Fn(&SDKEvent) + Send + Sync + 'static,
    {
        let mut listeners = self.listeners.write().await;
        listeners.entry(event_type.to_string()).or_insert_with(Vec::new).push(Box::new(listener));
        
        // 更新监听器统计
        let mut stats = self.stats.write().await;
        stats.listener_count = listeners.values().map(|v| v.len()).sum();
        
        info!("Added listener for event type: {}", event_type);
    }

    /// 移除所有监听器
    pub async fn clear_listeners(&self) {
        let mut listeners = self.listeners.write().await;
        listeners.clear();
        
        let mut stats = self.stats.write().await;
        stats.listener_count = 0;
        
        info!("Cleared all event listeners");
    }

    /// 获取事件统计
    pub async fn get_stats(&self) -> EventStats {
        self.stats.read().await.clone()
    }

    /// 获取活跃订阅者数量
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }

    /// 创建一个示例 typing 事件
    pub fn create_typing_event(
        channel_id: &str,
        channel_type: i32,
        user_id: &str,
        is_typing: bool,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::TypingIndicator(TypingEvent {
            channel_id: channel_id.to_string(),
            channel_type,
            user_id: user_id.to_string(),
            is_typing,
            timestamp,
            session_id: None,
        })
    }

    /// 创建一个示例连接状态变更事件
    pub fn create_connection_event(old_state: ConnectionState, new_state: ConnectionState) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::ConnectionStateChanged {
            old_state,
            new_state,
            timestamp,
        }
    }
}

/// 过滤事件接收器
pub struct FilteredEventReceiver {
    receiver: broadcast::Receiver<SDKEvent>,
    filter: EventFilter,
}

impl FilteredEventReceiver {
    /// 创建新的过滤事件接收器
    pub fn new(receiver: broadcast::Receiver<SDKEvent>, filter: EventFilter) -> Self {
        Self { receiver, filter }
    }

    /// 接收下一个匹配的事件
    pub async fn recv(&mut self) -> Result<SDKEvent, broadcast::error::RecvError> {
        loop {
            let event = self.receiver.recv().await?;
            if self.filter.matches(&event) {
                return Ok(event);
            }
        }
    }

    /// 尝试接收事件（非阻塞）
    pub fn try_recv(&mut self) -> Result<SDKEvent, broadcast::error::TryRecvError> {
        loop {
            let event = self.receiver.try_recv()?;
            if self.filter.matches(&event) {
                return Ok(event);
            }
        }
    }
}

/// 事件生成器 - 辅助函数
pub mod event_builders {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 创建消息状态变更事件
    pub fn message_status_changed(
        message_id: String,
        old_status: MessageStatus,
        new_status: MessageStatus,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::MessageStatusChanged {
            message_id,
            old_status,
            new_status,
            timestamp,
        }
    }

    /// 创建正在输入事件
    pub fn typing_indicator(
        channel_id: String,
        channel_type: i32,
        user_id: String,
        is_typing: bool,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::TypingIndicator(TypingEvent {
            channel_id,
            channel_type,
            user_id,
            is_typing,
            timestamp,
            session_id: None,
        })
    }

    /// 创建用户在线状态变更事件
    pub fn user_presence_changed(
        user_id: String,
        is_online: bool,
        last_seen: Option<u64>,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::UserPresenceChanged {
            user_id,
            is_online,
            last_seen,
            timestamp,
        }
    }

    /// 创建未读数变更事件
    pub fn unread_count_changed(
        channel_id: String,
        channel_type: i32,
        unread_count: i32,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::UnreadCountChanged {
            channel_id,
            channel_type,
            unread_count,
            timestamp,
        }
    }

    /// 创建连接状态变更事件
    pub fn connection_state_changed(old_state: ConnectionState, new_state: ConnectionState) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::ConnectionStateChanged {
            old_state,
            new_state,
            timestamp,
        }
    }

    /// 创建消息接收事件
    pub fn message_received(
        message_id: String,
        channel_id: String,
        channel_type: i32,
        from_uid: String,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::MessageReceived {
            message_id,
            channel_id,
            channel_type,
            from_uid,
            timestamp,
        }
    }

    /// 创建消息发送成功事件
    pub fn message_sent(
        message_id: String,
        channel_id: String,
        channel_type: i32,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::MessageSent {
            message_id,
            channel_id,
            channel_type,
            timestamp,
        }
    }

    /// 创建消息发送失败事件
    pub fn message_send_failed(
        message_id: String,
        channel_id: String,
        channel_type: i32,
        error: String,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::MessageSendFailed {
            message_id,
            channel_id,
            channel_type,
            error,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_event_manager_basic_functionality() {
        let manager = EventManager::new(100);
        
        // 测试订阅
        let mut receiver = manager.subscribe().await;
        
        // 测试发布事件
        let event = event_builders::typing_indicator(
            "channel1".to_string(),
            1,
            "user1".to_string(),
            true,
        );
        
        manager.emit(event.clone()).await;
        
        // 测试接收事件
        let received_event = receiver.recv().await.unwrap();
        assert_eq!(received_event.event_type(), "typing_indicator");
        
        // 测试统计
        let stats = manager.get_stats().await;
        assert_eq!(stats.total_events, 1);
        assert_eq!(stats.events_by_type.get("typing_indicator"), Some(&1));
    }

    #[tokio::test]
    async fn test_event_filter() {
        let manager = EventManager::new(100);
        
        // 创建过滤器
        let filter = EventFilter::new()
            .with_event_types(vec!["typing_indicator".to_string()])
            .with_channel_ids(vec!["channel1".to_string()]);
        
        let mut filtered_receiver = manager.subscribe_filtered(filter).await;
        
        // 发布匹配的事件
        let matching_event = event_builders::typing_indicator(
            "channel1".to_string(),
            1,
            "user1".to_string(),
            true,
        );
        manager.emit(matching_event).await;
        
        // 发布不匹配的事件
        let non_matching_event = event_builders::typing_indicator(
            "channel2".to_string(),
            1,
            "user1".to_string(),
            true,
        );
        manager.emit(non_matching_event).await;
        
        // 应该只接收到匹配的事件
        let received_event = filtered_receiver.recv().await.unwrap();
        if let SDKEvent::TypingIndicator(_) = received_event {
            assert_eq!(received_event.channel_id(), Some("channel1"));
        } else {
            panic!("Expected typing indicator event");
        }
    }

    #[tokio::test]
    async fn test_event_listeners() {
        let manager = EventManager::new(100);
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();
        
        // 添加监听器
        manager.add_listener("typing_indicator", move |_event| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
        }).await;
        
        // 发布事件
        for _ in 0..3 {
            let event = event_builders::typing_indicator(
                "channel1".to_string(),
                1,
                "user1".to_string(),
                true,
            );
            manager.emit(event).await;
        }
        
        // 等待一下确保监听器被调用
        sleep(Duration::from_millis(10)).await;
        
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let manager = EventManager::new(100);
        
        let mut receiver1 = manager.subscribe().await;
        let mut receiver2 = manager.subscribe().await;
        
        assert_eq!(manager.subscriber_count(), 2);
        
        let event = event_builders::message_sent(
            "msg1".to_string(),
            "channel1".to_string(),
            1,
        );
        
        manager.emit(event).await;
        
        // 两个订阅者都应该收到事件
        let event1 = receiver1.recv().await.unwrap();
        let event2 = receiver2.recv().await.unwrap();
        
        assert_eq!(event1.event_type(), "message_sent");
        assert_eq!(event2.event_type(), "message_sent");
    }

    #[tokio::test]
    async fn test_event_properties() {
        let event = event_builders::typing_indicator(
            "channel1".to_string(),
            1,
            "user1".to_string(),
            true,
        );
        
        assert_eq!(event.event_type(), "typing_indicator");
        assert_eq!(event.channel_id(), Some("channel1"));
        assert!(event.timestamp() > 0);
    }
} 