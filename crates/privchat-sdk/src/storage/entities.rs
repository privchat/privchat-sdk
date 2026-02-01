//! 数据实体定义 - 对应数据库表结构
//! 
//! 这里定义了所有数据库表对应的 Rust 结构体，用于：
//! - 类型安全的数据传输
//! - 统一的数据表示
//! - 序列化/反序列化支持

use serde::{Deserialize, Serialize};
use std::fmt;

/// 消息实体 - 对应 message 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Option<i64>,  // 本地主键（SQLite 自增）
    pub server_message_id: Option<u64>,  // 服务端消息ID（发送/同步成功后赋值）
    pub pts: i64,  // ⭐ 改名：message_seq -> pts（服务器顺序，per-channel）
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i32,
    pub timestamp: Option<i64>,
    pub from_uid: u64,  // u64，与服务端一致
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub voice_status: i32,
    pub created_at: i64,  // 毫秒时间戳（与服务端一致）
    pub updated_at: i64,  // 毫秒时间戳（与服务端一致）
    pub searchable_word: String,
    /// 客户端消息编号（local_message_id）
    /// 
    /// ⚠️ 重要：local_message_id 设计原则
    /// 
    /// local_message_id is a local transport identifier,
    /// it MUST NOT be persisted, synced, or relied on across devices.
    /// 
    /// - 作用域：仅发送端本地
    /// - 用途：发送队列、重试匹配、ACK 匹配、失败回滚
    /// - 禁止：进入 FFI Message Model、跨端同步、业务逻辑依赖
    /// 
    /// 注意：这是 SDK 内部存储字段，用于本地数据库存储和发送队列管理。
    /// FFI 层的 Message 结构体不包含此字段。
    pub local_message_id: u64,
    pub is_deleted: i32,
    pub setting: i32,
    pub order_seq: i64,
    pub extra: String,
    // 阅后即焚功能
    pub flame: i16,
    pub flame_second: i32,
    pub viewed: i16,
    pub viewed_at: i64,
    // 话题支持
    pub topic_id: String,
    // 消息过期时间
    pub expire_time: Option<i64>,
    pub expire_timestamp: Option<i64>,
    // 消息撤回状态
    pub revoked: i16,
    pub revoked_at: i64,
    pub revoked_by: Option<u64>,  // 撤回者用户ID（u64），可选（客户端可能不需要知道是谁撤回的）
}

/// 频道实体 - 对应 channel 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: Option<i64>,
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i32,
    // 会话列表相关字段
    pub last_local_message_id: u64,
    pub last_msg_timestamp: Option<i64>,
    pub unread_count: i32,
    pub last_msg_pts: i64,  // ⭐ last_msg_seq -> last_msg_pts（会话最后消息的 pts）
    // 频道信息字段
    pub show_nick: i32,
    pub username: String,
    pub channel_name: String,
    pub channel_remark: String,
    pub top: i32,
    pub mute: i32,
    pub save: i32,
    pub forbidden: i32,
    pub follow: i32,
    pub is_deleted: i32,
    pub receipt: i32,
    pub status: i32,
    pub invite: i32,
    pub robot: i32,
    pub version: i64,
    pub online: i16,
    pub last_offline: i64,
    pub avatar: String,
    pub category: String,
    pub extra: String,
    pub created_at: i64,  // 毫秒时间戳（与服务端一致）
    pub updated_at: i64,  // 毫秒时间戳（与服务端一致）
    pub avatar_cache_key: String,
    pub remote_extra: Option<String>,
    // 阅后即焚功能
    pub flame: i16,
    pub flame_second: i32,
    pub device_flag: i32,
    // 父频道支持
    pub parent_channel_id: u64,  // u64，与服务端一致
    pub parent_channel_type: i32,
}

/// 频道成员实体 - 对应 channel_member 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMember {
    pub id: Option<i64>,
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i32,
    pub member_uid: u64,  // u64，与服务端一致
    pub member_name: String,
    pub member_remark: String,
    pub member_avatar: String,
    pub member_invite_uid: u64,  // u64，与服务端一致
    pub role: i32,
    pub status: i32,
    pub is_deleted: i32,
    pub robot: i32,
    pub version: i64,
    pub created_at: i64,  // 毫秒时间戳（与服务端一致）
    pub updated_at: i64,  // 毫秒时间戳（与服务端一致）
    pub extra: String,
    pub forbidden_expiration_time: i64,
    pub member_avatar_cache_key: String,
}

/// 消息扩展实体 - 对应 message_extra 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageExtra {
    pub id: Option<i64>,
    pub message_id: Option<u64>,  // u64，与服务端一致
    pub channel_id: Option<u64>,  // u64，与服务端一致
    pub channel_type: i16,
    pub readed: i32,
    pub readed_count: i32,
    pub unread_count: i32,
    pub revoke: i16,
    pub revoker: Option<u64>,  // u64，与服务端一致
    pub extra_version: i64,
    pub is_mutual_deleted: i16,
    pub content_edit: Option<String>,
    pub edited_at: i32,
    pub need_upload: i16,
    pub is_pinned: i32,
}

/// 消息反应实体 - 对应 message_reaction 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReaction {
    pub id: Option<i64>,
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i32,
    pub uid: u64,  // u64，与服务端一致
    pub name: String,
    pub emoji: String,
    pub message_id: u64,  // u64，与服务端一致
    pub seq: i64,
    pub is_deleted: i32,
    pub created_at: Option<String>,
}

/// 提醒实体 - 对应 reminder 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: Option<i64>,
    pub reminder_id: i32,
    pub message_id: u64,  // u64，与服务端一致
    pub pts: u64,  // ⭐ message_seq -> pts（消息顺序）
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i16,
    pub uid: u64,  // u64，与服务端一致
    pub reminder_type: i32,
    pub text: String,
    pub data: String,
    pub is_locate: i16,
    pub version: i64,
    pub done: i16,
    pub need_upload: i16,
    pub publisher: Option<u64>,  // u64，与服务端一致
}

/// 机器人实体 - 对应 robot 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Robot {
    pub id: Option<i64>,
    pub robot_id: String,
    pub status: i16,
    pub version: i64,
    pub inline_on: i16,
    pub placeholder: String,
    pub username: String,
    pub created_at: Option<String>,
    pub updated_at: Option<i64>,  // 毫秒时间戳（与服务端一致）
}

/// 机器人菜单实体 - 对应 robot_menu 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RobotMenu {
    pub id: Option<i64>,
    pub robot_id: String,
    pub cmd: String,
    pub remark: String,
    pub menu_type: String,
    pub created_at: Option<String>,
    pub updated_at: Option<i64>,  // 毫秒时间戳（与服务端一致）
}

/// 会话扩展实体 - 对应 channel_extra 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelExtra {
    pub id: Option<i64>,
    pub channel_id: u64,  // u64，与服务端一致
    pub channel_type: i16,
    pub browse_to: u64,
    pub keep_pts: u64,  // ⭐ keep_message_seq -> keep_pts（保持的消息位置）
    pub keep_offset_y: i32,
    pub draft: String,
    pub version: i64,
    pub draft_updated_at: u64,
}

/// 查询参数结构体

/// 消息查询参数
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub channel_id: Option<u64>,  // u64，与服务端一致
    pub channel_type: Option<i32>,
    pub from_uid: Option<u64>,  // u64，与服务端一致
    pub message_type: Option<i32>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub order_by: Option<String>,
    pub is_deleted: Option<i32>,
}

/// 频道查询参数
#[derive(Debug, Clone, Default)]
pub struct ChannelQuery {
    pub channel_id: Option<u64>,  // u64，与服务端一致
    pub channel_type: Option<i32>,
    pub status: Option<i32>,
    pub is_deleted: Option<i32>,
    pub has_unread: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 分页结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageResult<T> {
    pub data: Vec<T>,
    pub total: i64,
    pub page: u32,
    pub page_size: u32,
    pub has_more: bool,
}

impl<T> PageResult<T> {
    pub fn new(data: Vec<T>, total: i64, page: u32, page_size: u32) -> Self {
        let has_more = (page * page_size) < total as u32;
        Self {
            data,
            total,
            page,
            page_size,
            has_more,
        }
    }
}

// 消息类型：业务层使用 string（如 "text"/"image"），DB 存 i32；不再提供 entities 层 MessageType 枚举。
// 频道类型：仅 1=私聊、2=群聊；客服/系统视为私聊（对端为机器人/系统用户），不再单独枚举。

/// 消息状态枚举
/// 
/// 状态流转图：
/// Draft → Sending → Sent → Delivered → Read
///           ↓        ↓
///       Retrying → Failed
///           ↓
///       Expired
/// 
/// 特殊状态：
/// - Revoked: 可以从 Sent/Delivered/Read 状态转换而来
/// - Burned: 阅后即焚消息的最终状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum MessageStatus {
    Draft = 0,         // 草稿
    Sending = 1,       // 发送中
    Sent = 2,          // 已发送
    Delivered = 3,     // 已投递
    Read = 4,          // 已读
    Failed = 5,        // 发送失败
    Revoked = 6,       // 已撤回
    Burned = 7,        // 已焚烧
    Retrying = 8,      // 重试中
    Expired = 9,       // 过期
    Received = 10,     // 已接收
}

impl MessageStatus {
    /// 检查状态是否为最终状态（不能再转换）
    pub fn is_final_state(&self) -> bool {
        matches!(self, 
            MessageStatus::Read | 
            MessageStatus::Revoked | 
            MessageStatus::Burned
        )
    }
    
    /// 检查状态是否表示发送成功
    pub fn is_sent_successfully(&self) -> bool {
        matches!(self, 
            MessageStatus::Sent | 
            MessageStatus::Delivered | 
            MessageStatus::Read
        )
    }
    
    /// 检查状态是否表示发送失败
    pub fn is_send_failed(&self) -> bool {
        matches!(self, 
            MessageStatus::Failed | 
            MessageStatus::Expired
        )
    }
    
    /// 检查状态是否需要网络处理
    pub fn needs_network_processing(&self) -> bool {
        matches!(self, 
            MessageStatus::Sending | 
            MessageStatus::Retrying
        )
    }
    
    /// 检查状态是否可以重试
    pub fn can_retry(&self) -> bool {
        matches!(self, 
            MessageStatus::Failed | 
            MessageStatus::Expired |
            MessageStatus::Retrying
        )
    }
    
    /// 检查状态是否可以撤回
    pub fn can_revoke(&self) -> bool {
        matches!(self, 
            MessageStatus::Sent | 
            MessageStatus::Delivered | 
            MessageStatus::Read
        )
    }
    
    /// 获取状态的显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            MessageStatus::Draft => "草稿",
            MessageStatus::Sending => "发送中",
            MessageStatus::Sent => "已发送",
            MessageStatus::Delivered => "已投递",
            MessageStatus::Read => "已读",
            MessageStatus::Failed => "发送失败",
            MessageStatus::Revoked => "已撤回",
            MessageStatus::Burned => "已焚烧",
            MessageStatus::Retrying => "重试中",
            MessageStatus::Expired => "已过期",
            MessageStatus::Received => "已接收",
        }
    }
    
    /// 从数据库整数值转换
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(MessageStatus::Draft),
            1 => Some(MessageStatus::Sending),
            2 => Some(MessageStatus::Sent),
            3 => Some(MessageStatus::Delivered),
            4 => Some(MessageStatus::Read),
            5 => Some(MessageStatus::Failed),
            6 => Some(MessageStatus::Revoked),
            7 => Some(MessageStatus::Burned),
            8 => Some(MessageStatus::Retrying),
            9 => Some(MessageStatus::Expired),
            10 => Some(MessageStatus::Received),
            _ => None,
        }
    }
    
    /// 转换为数据库整数值
    pub fn to_i32(&self) -> i32 {
        *self as i32
    }
}

impl fmt::Display for MessageStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

impl Default for MessageStatus {
    fn default() -> Self {
        MessageStatus::Draft
    }
}

/// 状态转换错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusTransitionError {
    pub from: MessageStatus,
    pub to: MessageStatus,
    pub reason: String,
}

impl fmt::Display for StatusTransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "不能从状态 '{}' 转换到 '{}': {}", self.from, self.to, self.reason)
    }
}

impl std::error::Error for StatusTransitionError {}

/// 状态转换验证器
pub struct StatusTransition;

impl StatusTransition {
    /// 验证状态转换是否有效
    pub fn validate(from: MessageStatus, to: MessageStatus) -> Result<(), StatusTransitionError> {
        if from.can_transition_to(to) {
            Ok(())
        } else {
            Err(StatusTransitionError {
                from,
                to,
                reason: "无效的状态转换".to_string(),
            })
        }
    }
    
    /// 验证状态转换并提供详细原因
    pub fn validate_with_reason(
        from: MessageStatus, 
        to: MessageStatus,
        context: Option<&str>
    ) -> Result<(), StatusTransitionError> {
        if from.can_transition_to(to) {
            Ok(())
        } else {
            let reason = match (from, to) {
                (MessageStatus::Read, MessageStatus::Sending) => 
                    "已读消息不能重新发送".to_string(),
                (MessageStatus::Revoked, _) => 
                    "已撤回的消息不能改变状态".to_string(),
                (MessageStatus::Burned, _) => 
                    "已焚烧的消息不能改变状态".to_string(),
                _ => context.map(|c| c.to_string()).unwrap_or_else(|| "无效的状态转换".to_string()),
            };
            
            Err(StatusTransitionError { from, to, reason })
        }
    }
}

/// 成员角色枚举
/// 与服务端保持一致：0: Owner, 1: Admin, 2: Member
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Owner = 0,       // 群主
    Admin = 1,       // 管理员
    Member = 2,      // 普通成员
}

/// 群成员状态：0=active, 1=left, 2=kicked
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum GroupMemberStatus {
    Active = 0,
    Left = 1,
    Kicked = 2,
}

// =============================================================================
// Entity Model V1：User / Group / GroupMember / Friend（SDK_ENTITY_MODEL_V1）
// =============================================================================

/// 用户实体 - 对应 user 表（好友/群成员/陌生人共用）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub user_id: u64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub alias: Option<String>,
    pub avatar: String,
    pub user_type: i32,
    pub is_deleted: bool,
    pub channel_id: String,
    pub updated_at: i64,
}

/// 群实体 - 对应 group 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 群成员实体 - 对应 group_member 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub group_id: u64,
    pub user_id: u64,
    pub role: i32,
    pub status: i32,
    pub alias: Option<String>,
    pub is_muted: bool,
    pub joined_at: i64,
    pub updated_at: i64,
}

/// 好友实体 - 对应 friend 表（仅关系；展示用 user）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friend {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}
