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
use tracing::{debug, info};

/// 发送消息选项（v1 冻结设计）
/// 
/// 设计目标：未来 5-10 年，99% 的消息发送能力扩展，不破 API
/// 
/// 设计原则：
/// - 不为某个功能单独加 send_xxx() 方法
/// - 不在协议层制造"消息类型爆炸"
/// - 所有"发送维度"都通过 options 扩展
/// 
/// 参考 Telegram / Signal / WhatsApp 的设计：
/// - 回复是消息属性，不是消息类型
/// - 所有扩展功能都通过 options 参数传递
#[derive(Debug, Clone, Default)]
pub struct SendMessageOptions {
    /// 回复哪条消息（Reply，合约 v1）
    ///
    /// 为本地主键 message.id；发送前会查库填 server_message_id 到协议。
    /// 若提供，表示这是一条回复消息；回复是消息属性，不是消息类型。
    pub in_reply_to_message_id: Option<u64>,
    
    /// @ 提及的用户
    /// 
    /// 用户 ID 列表，用于 @ 提及功能。
    pub mentions: Vec<u64>,
    
    /// 是否静默发送（不触发推送）
    /// 
    /// Telegram / Signal 都有此功能。
    pub silent: bool,
    
    /// 客户端扩展字段（不会被 SDK 解析）
    /// 
    /// 这是给"未来自己 + 第三方插件"留的逃生通道。
    /// SDK 永远不要解释它，直接透传到服务端。
    pub extra: Option<serde_json::Value>,
}

impl SendMessageOptions {
    /// 创建默认选项
    pub fn new() -> Self {
        Self::default()
    }
    
    /// 设置回复消息（message.id，合约 v1）
    pub fn with_reply(mut self, message_id: u64) -> Self {
        self.in_reply_to_message_id = Some(message_id);
        self
    }
    
    /// 设置 @提及的用户列表
    pub fn with_mentions(mut self, user_ids: Vec<u64>) -> Self {
        self.mentions = user_ids;
        self
    }
    
    /// 设置静默发送
    pub fn with_silent(mut self, silent: bool) -> Self {
        self.silent = silent;
        self
    }
    
    /// 设置客户端扩展字段
    pub fn with_extra(mut self, extra: serde_json::Value) -> Self {
        self.extra = Some(extra);
        self
    }
}

/// 附件信息（SDK 层）
/// 
/// 用于表示消息中的附件（图片、视频、文件等）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    /// 附件 URL（服务端返回的文件访问地址）
    pub url: String,
    /// MIME 类型
    pub mime_type: String,
    /// 文件大小（字节）
    pub size: u64,
    /// 缩略图 URL（可选，主要用于图片和视频）
    pub thumbnail_url: Option<String>,
    /// 文件名（可选）
    pub filename: Option<String>,
    /// 文件ID（服务端分配的唯一标识）
    pub file_id: Option<String>,
    /// 宽度（图片/视频，可选）
    pub width: Option<u32>,
    /// 高度（图片/视频，可选）
    pub height: Option<u32>,
    /// 时长（视频/音频，秒，可选）
    pub duration: Option<u32>,
}

/// SDK 事件类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SDKEvent {
    /// 消息状态变更
    MessageStatusChanged {
        message_id: u64,
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
        user_id: u64,
        is_online: bool,
        last_seen: Option<u64>,
        timestamp: u64,
    },
    /// 未读数变更
    UnreadCountChanged {
        channel_id: u64,
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
    /// 消息接收（server_message_id 为服务端消息 ID）
    MessageReceived {
        server_message_id: u64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        timestamp: u64,
        content: String, // ✅ 添加消息内容字段
    },
    /// 消息发送成功
    MessageSent {
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        timestamp: u64,
    },
    /// 消息发送失败
    MessageSendFailed {
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        error: String,
        timestamp: u64,
    },
    /// 发送状态更新（按 message.id）
    SendStatusUpdate {
        channel_id: u64,
        id: u64,  // message.id，无值时 0
        state: SendStatusState,
        attempts: u32,
        error: Option<String>,
        timestamp: u64,
    },
    /// 时间线差异（Timeline Diff）
    /// 
    /// 用于实时更新消息时间线，支持增量更新
    TimelineDiff {
        channel_id: u64,
        diff_kind: TimelineDiffKind,
        timestamp: u64,
    },
    /// 会话列表更新
    /// 
    /// 用于实时更新会话列表，包括未读数、最后消息等
    ChannelListUpdate {
        update_kind: ChannelListUpdateKind,
        timestamp: u64,
    },
}

/// 发送状态枚举（SDK 层）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SendStatusState {
    Enqueued,    // 已入队
    Sending,     // 发送中
    Sent,        // 已发送（已获得 message_id）
    Retrying,    // 重试中
    Failed,       // 发送失败
}

/// 时间线差异类型（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TimelineDiffKind {
    /// 重置整个时间线
    Reset {
        messages: Vec<TimelineMessage>,
    },
    /// 追加新消息到时间线末尾。messages 已按 pts 升序；客户端应追加到列表尾部，勿插到头部。
    Append {
        messages: Vec<TimelineMessage>,
    },
    /// 更新指定消息
    UpdateByItemId {
        item_id: u64,  // message_id
        message: TimelineMessage,
    },
    /// 删除指定消息
    RemoveByItemId {
        item_id: u64,  // message_id
    },
}

/// 时间线消息（SDK 层）
/// 
/// - id：客户端唯一标识（message.id），用于 itemId 与分页游标
/// - server_message_id：服务端消息 ID（可选）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineMessage {
    /// 客户端唯一标识（message.id）
    pub id: u64,
    #[serde(alias = "message_id")]
    pub server_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub content: String,
    pub message_type: i32,
    pub timestamp: u64,
    pub pts: u64,
}

/// 会话列表更新类型（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChannelListUpdateKind {
    /// 重置整个会话列表
    Reset {
        channels: Vec<ChannelListEntry>,
    },
    /// 更新单个会话
    Update {
        channel: ChannelListEntry,
    },
    /// 删除会话
    Remove {
        channel_id: u64,
    },
}

/// 会话列表条目（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelListEntry {
    pub channel_id: u64,
    pub channel_type: i32,
    pub name: String,
    pub last_ts: u64,
    pub notifications: u32,
    pub messages: u32,
    pub mentions: u32,
    pub marked_unread: bool,
    pub is_favourite: bool,
    pub is_low_priority: bool,
    pub avatar_url: Option<String>,
    pub is_dm: bool,
    pub is_encrypted: bool,
    pub member_count: u32,
    pub topic: Option<String>,
    pub latest_event: Option<LatestChannelEvent>,
}

/// 最新会话事件（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatestChannelEvent {
    pub event_type: String,
    pub content: String,
    pub timestamp: u64,
}

/// 未读统计（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadStats {
    pub messages: u64,
    pub notifications: u64,
    pub mentions: u64,
}

/// 同步阶段（SDK 层）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SyncPhase {
    Idle,       // 空闲
    Running,    // 正在同步
    BackingOff, // 退避中（等待重试）
    Error,      // 错误
}

/// 同步状态（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub phase: SyncPhase,
    pub message: Option<String>,
}

/// 搜索结果页面（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub next_offset: Option<u32>,
}

/// 搜索结果条目（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub channel_id: u64,
    pub server_message_id: u64,
    pub sender: u64,
    pub body: String,
    pub timestamp_ms: u64,
}

/// 通知模式（SDK 层）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationMode {
    All,        // 所有通知
    Mentions,   // 仅 @ 提及
    None,       // 无通知
}

/// 会话标签（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTags {
    pub favourite: bool,
    pub low_priority: bool,
}

/// 设备摘要（SDK 层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub device_id: String,
    pub device_name: String,
    pub device_model: Option<String>,
    pub app_id: Option<String>,
    pub device_type: Option<String>,
    pub last_active_at: Option<u64>,
    pub created_at: Option<u64>,
    pub ip_address: Option<String>,
    pub is_current: bool,
}

/// 反应芯片（Reaction Chip）- 表示一个表情及其用户列表
/// 
/// 用于显示消息的反应，例如：👍 (3个用户), ❤️ (5个用户)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionChip {
    /// 表情符号（如 👍, ❤️, 😂）
    pub emoji: String,
    /// 添加此反应的用户ID列表
    pub user_ids: Vec<u64>,
    /// 反应数量（等于 user_ids.len()）
    pub count: usize,
}

/// 已读用户条目（Seen By Entry）
/// 
/// 表示已读某条消息的用户信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeenByEntry {
    /// 用户ID
    pub user_id: u64,
    /// 已读时间（UNIX 时间戳，毫秒，UTC）
    pub read_at: u64,
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
            SDKEvent::SendStatusUpdate { .. } => "send_status_update",
            SDKEvent::TimelineDiff { .. } => "timeline_diff",
            SDKEvent::ChannelListUpdate { .. } => "channel_list_update",
        }
    }

    /// 获取事件关联的频道ID
    pub fn channel_id(&self) -> Option<&u64> {
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
            SDKEvent::SendStatusUpdate { timestamp, .. } => *timestamp,
            SDKEvent::TimelineDiff { timestamp, .. } => *timestamp,
            SDKEvent::ChannelListUpdate { timestamp, .. } => *timestamp,
        }
    }

    /// 获取事件相关的用户ID
    pub fn user_id(&self) -> Option<u64> {
        match self {
            SDKEvent::ReadReceiptReceived(e) => Some(e.reader_uid),
            SDKEvent::MessageRevoked(e) => Some(e.revoker_uid),
            SDKEvent::MessageEdited(e) => Some(e.editor_uid),
            SDKEvent::TypingStarted(e) => Some(e.user_id),
            SDKEvent::TypingStopped(e) => Some(e.user_id),
            SDKEvent::ReactionAdded(e) => Some(e.user_id),
            SDKEvent::ReactionRemoved(e) => Some(e.user_id),
            SDKEvent::TypingIndicator(e) => Some(e.user_id),
            SDKEvent::UserPresenceChanged { user_id, .. } => Some(*user_id),
            SDKEvent::MessageReceived { from_uid, .. } => Some(*from_uid),
            _ => None,
        }
    }
}

/// 事件过滤器
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EventFilter {
    /// 事件类型过滤器
    pub event_types: Option<Vec<String>>,
    /// 频道ID过滤器
    pub channel_ids: Option<Vec<u64>>,
    /// 用户ID过滤器
    pub user_ids: Option<Vec<u64>>,
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
    pub fn with_channel_ids(mut self, channel_ids: Vec<u64>) -> Self {
        self.channel_ids = Some(channel_ids);
        self
    }

    /// 添加用户ID过滤
    pub fn with_user_ids(mut self, user_ids: Vec<u64>) -> Self {
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
                if !channel_ids.contains(&channel_id) {
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
                if !user_ids.contains(user_id) {
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

        // 广播事件（无订阅者时 send 会失败，属正常场景如压测/无 UI 客户端，仅打 debug）
        if let Err(e) = self.sender.send(event.clone()) {
            debug!("Failed to broadcast event (no active receivers): {}", e);
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
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
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
        message_id: u64,
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
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
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
        user_id: u64,
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
        channel_id: u64,
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

    /// 创建消息接收事件（server_message_id 为服务端消息 ID）
    pub fn message_received(
        server_message_id: u64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
    ) -> SDKEvent {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        SDKEvent::MessageReceived {
            server_message_id,
            channel_id,
            channel_type,
            from_uid,
            timestamp,
            content: String::new(), // 默认空内容
        }
    }

    /// 创建消息发送成功事件
    pub fn message_sent(
        message_id: u64,
        channel_id: u64,
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
        message_id: u64,
        channel_id: u64,
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
            10,
            1,
            1,
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
            .with_channel_ids(vec![10]);
        
        let mut filtered_receiver = manager.subscribe_filtered(filter).await;
        
        // 发布匹配的事件
        let matching_event = event_builders::typing_indicator(
            10,
            1,
            1,
            true,
        );
        manager.emit(matching_event).await;
        
        // 发布不匹配的事件
        let non_matching_event = event_builders::typing_indicator(
            11,
            1,
            1,
            true,
        );
        manager.emit(non_matching_event).await;
        
        // 应该只接收到匹配的事件
        let received_event = filtered_receiver.recv().await.unwrap();
        if let SDKEvent::TypingIndicator(_) = received_event {
            assert_eq!(received_event.channel_id(), Some(&10));
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
                10,
                1,
                1,
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
            1,
            10,
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
            10,
            1,
            1,
            true,
        );
        
        assert_eq!(event.event_type(), "typing_indicator");
        assert_eq!(event.channel_id(), Some(&10));
        assert!(event.timestamp() > 0);
    }
} 