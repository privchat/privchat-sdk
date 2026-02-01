//! Event system for FFI callbacks

// ============================================================================
// Callback Interface (defined as trait stub for UniFFI)
// ============================================================================

/// Callback interface for SDK events
/// 
/// UniFFI 0.31: Type-safe callback interface - no JSON serialization needed
/// Implement this in your UI layer to receive real-time events.
#[uniffi::export(callback_interface)]
pub trait PrivchatDelegate: Send + Sync {
    /// Called when a message is received (type-safe, no JSON parsing needed)
    fn on_message_received(&self, message: MessageEntry);
    
    /// Called when connection state changes (type-safe)
    fn on_connection_state_changed(&self, state: ConnectionState);
    
    /// Called when platform network status changes (Online/Offline/Connecting/Limited)
    fn on_network_status_changed(&self, old_status: NetworkStatus, new_status: NetworkStatus);
    
    /// Called for generic SDK events (type-safe)
    fn on_event(&self, event: SDKEvent);
}

/// Platform network status (WiFi/cellular availability)
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NetworkStatus {
    Online,
    Offline,
    Connecting,
    Limited,
}

impl From<privchat_sdk::network::NetworkStatus> for NetworkStatus {
    fn from(s: privchat_sdk::network::NetworkStatus) -> Self {
        match s {
            privchat_sdk::network::NetworkStatus::Online => Self::Online,
            privchat_sdk::network::NetworkStatus::Offline => Self::Offline,
            privchat_sdk::network::NetworkStatus::Connecting => Self::Connecting,
            privchat_sdk::network::NetworkStatus::Limited => Self::Limited,
        }
    }
}

// ============================================================================
// Event Types
// ============================================================================

/// Connection state
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

impl From<privchat_sdk::connection_state::ConnectionStatus> for ConnectionState {
    fn from(status: privchat_sdk::connection_state::ConnectionStatus) -> Self {
        match status {
            privchat_sdk::connection_state::ConnectionStatus::Disconnected => Self::Disconnected,
            privchat_sdk::connection_state::ConnectionStatus::Connecting => Self::Connecting,
            privchat_sdk::connection_state::ConnectionStatus::Connected => Self::Connected,
            privchat_sdk::connection_state::ConnectionStatus::Reconnecting => Self::Reconnecting,
            privchat_sdk::connection_state::ConnectionStatus::Failed => Self::Failed,
            privchat_sdk::connection_state::ConnectionStatus::Authenticating => Self::Connecting,
            privchat_sdk::connection_state::ConnectionStatus::Authenticated => Self::Connected,
        }
    }
}

/// Message status
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MessageStatus {
    Pending,
    Sending,
    Sent,
    Failed,
    Read,
}

impl From<privchat_sdk::storage::entities::MessageStatus> for MessageStatus {
    fn from(status: privchat_sdk::storage::entities::MessageStatus) -> Self {
        match status {
            privchat_sdk::storage::entities::MessageStatus::Draft => Self::Pending,
            privchat_sdk::storage::entities::MessageStatus::Sending => Self::Sending,
            privchat_sdk::storage::entities::MessageStatus::Sent => Self::Sent,
            privchat_sdk::storage::entities::MessageStatus::Delivered => Self::Sent,
            privchat_sdk::storage::entities::MessageStatus::Read => Self::Read,
            privchat_sdk::storage::entities::MessageStatus::Failed => Self::Failed,
            privchat_sdk::storage::entities::MessageStatus::Revoked => Self::Failed,
            privchat_sdk::storage::entities::MessageStatus::Burned => Self::Failed,
            privchat_sdk::storage::entities::MessageStatus::Retrying => Self::Sending,
            privchat_sdk::storage::entities::MessageStatus::Expired => Self::Failed,
            privchat_sdk::storage::entities::MessageStatus::Received => Self::Sent,
        }
    }
}

/// Event type classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum, serde::Serialize, serde::Deserialize)]
pub enum EventType {
    MessageReceived,
    MessageSent,
    MessageFailed,
    ConnectionStateChanged,
    TypingIndicator,
    ReadReceipt,
    UserPresenceChanged,
}

/// A message data structure for FFI
/// 
/// 设计原则：
/// - **id**：客户端唯一标识（对应 SQLite message.id 自增主键），用于列表项 identity、分页游标（Load earlier）、
///   itemId/eventId。无论 message_id / local_message_id 是否存在，id 始终存在且有序。
/// - **message_id**：服务端分配的消息 ID（可选），用于 revoke、mark_as_read 等与服务端交互的 API。
/// - ❌ 禁止：local_message_id 进入 Message Model（仅 SendObserver / SendUpdate 暴露）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageEntry {
    /// 客户端唯一标识（message.id），用于 itemId/eventId 与分页游标，始终存在且有序
    pub id: u64,
    /// 服务端消息 ID（可选），用于与服务端交互的 API
    pub server_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub content: String,
    pub status: MessageStatus,
    pub timestamp: u64,
}

/// Channel (session) info for FFI
#[derive(Debug, Clone, uniffi::Record)]
pub struct Channel {
    pub channel_id: u64,
    pub channel_type: i32,
    pub last_msg_timestamp: i64,
    pub unread_count: u32,
    pub last_msg_seq: i64,
}

/// 在线状态条目（FFI Entry/DTO）
/// 
/// 这是 UI 层可见的在线状态类型，对应 Java 的 DTO 层。
#[derive(Debug, Clone, uniffi::Record)]
pub struct PresenceEntry {
    pub user_id: u64,
    pub is_online: bool,
    pub last_seen: Option<i64>,
    pub device_type: Option<String>,
}

/// Generic SDK event for FFI
/// 
/// UniFFI 0.31: Type-safe event structure
#[derive(Debug, Clone, uniffi::Record)]
pub struct SDKEvent {
    pub event_type: EventType,
    /// Event data as JSON string (for complex nested data)
    /// For simple events, use specific callback methods instead
    pub data: String,
    pub timestamp: u64,
}

/// Typing indicator event (type-safe)
#[derive(Debug, Clone, uniffi::Record)]
pub struct TypingIndicatorEvent {
    pub channel_id: u64,
    pub user_id: u64,
    pub is_typing: bool,
}

/// Read receipt event (type-safe)
#[derive(Debug, Clone, uniffi::Record)]
pub struct ReadReceiptEvent {
    pub channel_id: u64,
    pub server_message_id: u64,
    pub reader_uid: u64,
    pub timestamp: u64,
}

/// 发送状态枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SendState {
    Enqueued,    // 已入队
    Sending,     // 发送中
    Sent,        // 已发送（已获得 message_id）
    Retrying,    // 重试中
    Failed,       // 发送失败
}

/// 发送状态更新（按 message.id）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SendUpdate {
    pub channel_id: u64,
    /// message.id，无值时 0
    pub id: u64,
    pub state: SendState,
    pub attempts: u32,
    pub error: Option<String>,
}

/// 发送观察者回调接口
/// 
/// 用于跟踪消息发送状态（Enqueued → Sending → Sent/Failed）
/// 这是 local_message_id 的唯一暴露点
#[uniffi::export(callback_interface)]
pub trait SendObserver: Send + Sync {
    fn on_update(&self, update: SendUpdate);
}

/// 输入状态观察者回调接口
/// 
/// 用于实时接收其他用户的输入状态变化
#[uniffi::export(callback_interface)]
pub trait TypingObserver: Send + Sync {
    fn on_typing(&self, event: TypingIndicatorEvent);
}

/// 已读回执观察者回调接口
/// 
/// 用于实时接收消息已读回执
#[uniffi::export(callback_interface)]
pub trait ReceiptsObserver: Send + Sync {
    fn on_receipt(&self, event: ReadReceiptEvent);
}

/// 时间线差异类型（FFI 层）
#[derive(Debug, Clone, uniffi::Enum)]
pub enum TimelineDiffKind {
    /// 重置整个时间线
    Reset {
        values: Vec<MessageEntry>,
    },
    /// 追加新消息到时间线**末尾**。values 已按 pts 升序排列，实现时必须追加到列表尾部，切勿插入到头部，否则会显示为倒序（如 3、2、1）。
    Append {
        values: Vec<MessageEntry>,
    },
    /// 更新指定消息
    UpdateByItemId {
        item_id: u64,  // message.id（客户端唯一标识）
        value: MessageEntry,
    },
    /// 删除指定消息
    RemoveByItemId {
        item_id: u64,  // message.id（客户端唯一标识）
    },
}

/// 时间线观察者回调接口
///
/// 用于实时接收消息时间线的变化（新消息、更新、删除）。
/// - **Append**：必须将 `values` 按顺序追加到当前时间线**末尾**（不要插入到头部），否则离线推送会显示为倒序。
#[uniffi::export(callback_interface)]
pub trait TimelineObserver: Send + Sync {
    fn on_diff(&self, diff: TimelineDiffKind);
    fn on_error(&self, message: String);
}

/// 会话列表条目（FFI 层）
#[derive(Debug, Clone, uniffi::Record)]
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

/// 最新会话事件（FFI 层）
#[derive(Debug, Clone, uniffi::Record)]
pub struct LatestChannelEvent {
    pub event_type: String,
    pub content: String,
    pub timestamp: u64,
}

/// 频道列表观察者回调接口
/// 
/// 用于实时接收频道列表的变化（未读数、最后消息等）
#[uniffi::export(callback_interface)]
pub trait ChannelListObserver: Send + Sync {
    fn on_reset(&self, items: Vec<ChannelListEntry>);
    fn on_update(&self, item: ChannelListEntry);
}

// ============================================================================
// Auth Result Types
// ============================================================================

/// 认证结果（FFI Entry/DTO）
/// 
/// 用于 register() 和 login() 的返回类型
#[derive(Debug, Clone, uniffi::Record)]
pub struct AuthResult {
    pub user_id: u64,
    pub token: String,
}

/// 群组创建结果（FFI Entry/DTO）
/// 
/// 用于 create_group() 的返回类型
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupCreateResult {
    pub group_id: u64,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u32,
    pub created_at: String,  // ISO 8601
    pub creator_id: u64,
}

// ============================================================================
// Entry Types (DTO for UI Layer)
// ============================================================================
// 这些是 UI 层可见的 Entry/DTO 类型，对应 Java 的 DTO 层
// SDK 层的 entities 是数据库绑定层（Model），不应该直接暴露给 UI

/// 好友条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct FriendEntry {
    pub user_id: u64,
    pub username: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub user_type: i16,  // 0: 普通用户, 1: 系统用户, 2: 机器人
    pub status: String,  // accepted, deleted, blocked
    pub added_at: i64,   // 添加时间（毫秒时间戳）
    pub remark: Option<String>,  // 备注
}

/// 用户条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct UserEntry {
    pub user_id: u64,
    pub username: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub user_type: i16,
    pub is_friend: bool,
    /// 是否可以发送消息（RPC 搜索结果）
    pub can_send_message: bool,
    /// 搜索会话 ID（用于添加好友时传 source_id，来源如「根据账号搜索」）
    pub search_session_id: Option<u64>,
    pub is_online: Option<bool>,
}

/// 待处理好友申请条目（别人申请我为好友的一条请求）
#[derive(Debug, Clone, uniffi::Record)]
pub struct FriendPendingEntry {
    pub from_user_id: u64,
    pub message: Option<String>,
    pub created_at: String,
}

/// 群组条目（FFI Entry/DTO，本地群列表）
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupEntry {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 群组成员条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupMemberEntry {
    pub user_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub name: String,
    pub remark: String,
    pub avatar: String,
    pub role: i32,  // 角色：0=普通成员, 1=管理员, 2=群主
    pub status: i32,  // 状态：0=正常, 1=被禁言, 2=已退出
    pub invite_user_id: u64,  // 邀请者用户ID
}

/// 黑名单条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct BlacklistEntry {
    pub user_id: u64,
    pub username: String,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub blocked_at: i64,  // 拉黑时间（毫秒时间戳）
}

/// 同步状态条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncStateEntry {
    pub channel_id: u64,
    pub channel_type: i32,
    pub local_pts: u64,  // 本地 PTS
    pub server_pts: u64,  // 服务器 PTS
    pub needs_sync: bool,  // 是否需要同步
    pub last_sync_at: Option<i64>,  // 最后同步时间（毫秒时间戳）
}

/// 未读统计（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct UnreadStats {
    pub messages: u64,
    pub notifications: u64,
    pub mentions: u64,
}

/// 最后已读位置（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct LastReadPosition {
    pub server_message_id: Option<u64>,
    pub timestamp: Option<u64>,
}

/// 通过二维码加入群组结果（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupQRCodeJoinResult {
    pub status: String,  // "pending" 或 "joined"
    pub group_id: u64,
    pub request_id: Option<String>,  // 如果需要审批
    pub message: Option<String>,
    pub expires_at: Option<String>,  // ISO 8601
    pub user_id: Option<u64>,  // 如果已加入
    pub joined_at: Option<String>,  // ISO 8601，如果已加入
}

/// 获取或创建私聊会话结果（非好友发消息流程）
#[derive(Debug, Clone, uniffi::Record)]
pub struct GetOrCreateDirectChannelResult {
    pub channel_id: u64,
    /// 是否本次新创建的会话
    pub created: bool,
}

/// 同步阶段（FFI Entry/DTO）
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SyncPhase {
    Idle,       // 空闲
    Running,    // 正在同步
    BackingOff, // 退避中（等待重试）
    Error,      // 错误
}

/// 同步状态（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncStatus {
    pub phase: SyncPhase,
    pub message: Option<String>,
}

/// 同步观察者回调接口
/// 
/// 用于实时接收同步状态变化
#[uniffi::export(callback_interface)]
pub trait SyncObserver: Send + Sync {
    fn on_state(&self, status: SyncStatus);
}

/// 搜索结果页面（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub next_offset: Option<u32>,
}

/// 搜索结果条目（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchHit {
    pub channel_id: u64,
    pub server_message_id: u64,
    pub sender: u64,
    pub body: String,
    pub timestamp_ms: u64,
}

/// 通知模式（FFI Entry/DTO）
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationMode {
    All,        // 所有通知
    Mentions,   // 仅 @ 提及
    None,       // 无通知
}

/// 会话标签（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelTags {
    pub favourite: bool,
    pub low_priority: bool,
}

/// 设备摘要（FFI Entry/DTO）
#[derive(Debug, Clone, uniffi::Record)]
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

/// 反应芯片（FFI Entry/DTO）
/// 
/// 表示一个表情及其用户列表，用于显示消息的反应
#[derive(Debug, Clone, uniffi::Record)]
pub struct ReactionChip {
    /// 表情符号（如 👍, ❤️, 😂）
    pub emoji: String,
    /// 添加此反应的用户ID列表
    pub user_ids: Vec<u64>,
    /// 反应数量（等于 user_ids.len()）
    pub count: u64,
}

/// 媒体处理操作类型（Contract v1.1）
/// 
/// 视频发送时，SDK 需平台提供缩略图生成与视频压缩能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaProcessOp {
    Thumbnail,
    Compress,
}

/// 视频处理钩子（Contract v1.1）
/// 
/// 平台实现此接口，SDK 在发送视频时调用。未设置时，视频缩略图使用 1x1 透明 PNG 占位。
#[uniffi::export(callback_interface)]
pub trait VideoProcessHook: Send + Sync {
    /// 执行视频处理操作
    /// 
    /// - `op`: Thumbnail 生成缩略图，Compress 压缩视频
    /// - `source_path`: 源视频路径
    /// - `meta_path`: meta.json 路径
    /// - `output_path`: 输出路径（缩略图时为 .jpg，压缩时为视频文件）
    /// 
    /// 返回：Ok(true) 成功，Ok(false) 跳过（如原视频已满足要求），Err 失败
    fn process(
        &self,
        op: MediaProcessOp,
        source_path: String,
        meta_path: String,
        output_path: String,
    ) -> Result<bool, crate::error::PrivchatError>;
}

/// 已读用户条目（FFI Entry/DTO）
/// 
/// 表示已读某条消息的用户信息
#[derive(Debug, Clone, uniffi::Record)]
pub struct SeenByEntry {
    /// 用户ID
    pub user_id: u64,
    /// 已读时间（UNIX 时间戳，毫秒，UTC）
    pub read_at: u64,
}

/// 发送消息选项（FFI Entry/DTO，v1 冻结设计）
/// 
/// 设计目标：未来 5-10 年，99% 的消息发送能力扩展，不破 API
/// 
/// FFI 层映射原则：
/// - Option → nullable（Swift/Kotlin/JS 友好）
/// - Vec<u64> → Vec<String>（UniFFI 最稳定模式）
/// - serde_json::Value → Option<String>（JSON 字符串是事实标准）
/// 
/// 参考 Telegram / Signal / WhatsApp 的设计：
/// - 回复是消息属性，不是消息类型
/// - 所有扩展功能都通过 options 参数传递
#[derive(Debug, Clone, uniffi::Record)]
pub struct SendMessageOptions {
    /// 回复哪条消息（Reply）
    /// 
    /// 如果提供，表示这是一条回复消息。
    /// 回复是消息属性，不是消息类型。
    pub in_reply_to_message_id: Option<u64>,
    
    /// @ 提及的用户
    /// 
    /// 用户 ID 列表（作为字符串，UniFFI 最稳定模式）。
    /// 注意：FFI 层使用 Vec<String>，SDK 层会自动转换为 Vec<u64>。
    pub mentions: Vec<u64>,
    
    /// 是否静默发送（不触发推送）
    /// 
    /// Telegram / Signal 都有此功能。
    pub silent: bool,
    
    /// 客户端扩展字段（不会被 SDK 解析）
    /// 
    /// JSON 字符串格式，SDK 永远不解释它，直接透传到服务端。
    /// 这是给"未来自己 + 第三方插件"留的逃生通道。
    pub extra_json: Option<String>,
}

impl Default for SendMessageOptions {
    fn default() -> Self {
        Self {
            in_reply_to_message_id: None,
            mentions: Vec::new(),
            silent: false,
            extra_json: None,
        }
    }
}

/// 附件信息（FFI Entry/DTO）
/// 
/// 用于表示消息中的附件（图片、视频、文件等）
#[derive(Debug, Clone, uniffi::Record)]
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

/// 附件发送结果（FFI Entry/DTO）
/// 
/// 包含消息ID和附件信息
#[derive(Debug, Clone, uniffi::Record)]
pub struct AttachmentSendResult {
    /// message.id，无值时 0
    pub id: u64,
    /// 附件信息
    pub attachment: AttachmentInfo,
}

/// 进度观察者回调接口
/// 
/// 用于跟踪文件上传/下载进度
#[uniffi::export(callback_interface)]
pub trait ProgressObserver: Send + Sync {
    /// 进度更新回调
    /// 
    /// # 参数
    /// - `current`: 当前已传输的字节数
    /// - `total`: 总字节数（可选，某些情况下可能未知）
    fn on_progress(&self, current: u64, total: Option<u64>);
}

impl SDKEvent {
    /// Create a new SDK event
    pub fn new(event_type: EventType, data: String) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        Self {
            event_type,
            data,
            timestamp,
        }
    }
}

/// Convert internal SDK events to FFI events
impl From<privchat_sdk::events::SDKEvent> for SDKEvent {
    fn from(event: privchat_sdk::events::SDKEvent) -> Self {
        use privchat_sdk::events::SDKEvent as InternalEvent;
        
        let (event_type, data) = match event {
            InternalEvent::MessageReceived { server_message_id, channel_id, .. } => {
                (EventType::MessageReceived, format!("server_message_id={},channel_id={}", server_message_id, channel_id))
            }
            InternalEvent::MessageSent { message_id, .. } => {
                (EventType::MessageSent, format!("message_id={}", message_id))
            }
            InternalEvent::MessageSendFailed { message_id, error, .. } => {
                (EventType::MessageFailed, format!("message_id={},error={}", message_id, error))
            }
            InternalEvent::ConnectionStateChanged { new_state, .. } => {
                (EventType::ConnectionStateChanged, format!("state={:?}", new_state))
            }
            InternalEvent::TypingIndicator(typing_event) => {
                (EventType::TypingIndicator, format!("user_id={},channel_id={}", typing_event.user_id, typing_event.channel_id))
            }
            InternalEvent::ReadReceiptReceived(receipt) => {
                (EventType::ReadReceipt, format!("reader_uid={},message_id={}", receipt.reader_uid, receipt.message_id))
            }
            InternalEvent::UserPresenceChanged { user_id, is_online, .. } => {
                (EventType::UserPresenceChanged, format!("user_id={},is_online={}", user_id, is_online))
            }
            _ => (EventType::MessageReceived, "unknown".to_string()),
        };
        
        Self::new(event_type, data)
    }
}

// PHASE 0: Callback temporarily disabled
// Will be re-enabled in Phase 3 using polling or Phase 4 using macros
//
// #[privchat_ffi_macros::export(callback_interface)]
// pub trait PrivchatDelegate: Send + Sync + fmt::Debug {
//     fn on_message_received(&self, message: Message);
//     fn on_connection_state_changed(&self, state: ConnectionState);
//     fn on_event(&self, event: SDKEvent);
//     fn on_error(&self, error: String);
// }
