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
    pub client_seq: Option<i64>,
    pub message_id: Option<String>,
    pub message_seq: i64,
    pub channel_id: String,
    pub channel_type: i32,
    pub timestamp: Option<i64>,
    pub from_uid: String,
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub voice_status: i32,
    pub created_at: String,
    pub updated_at: String,
    pub searchable_word: String,
    pub client_msg_no: String,
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
}

/// 会话实体 - 对应 conversation 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: Option<i64>,
    pub channel_id: String,
    pub channel_type: i32,
    pub last_client_msg_no: String,
    pub last_msg_timestamp: Option<i64>,
    pub unread_count: i32,
    pub is_deleted: i32,
    pub version: i64,
    pub extra: String,
    pub last_msg_seq: i64,
    // 父频道支持
    pub parent_channel_id: String,
    pub parent_channel_type: i32,
}

/// 频道实体 - 对应 channel 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub id: Option<i64>,
    pub channel_id: String,
    pub channel_type: i32,
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
    pub created_at: String,
    pub updated_at: String,
    pub avatar_cache_key: String,
    pub remote_extra: Option<String>,
    // 阅后即焚功能
    pub flame: i16,
    pub flame_second: i32,
    pub device_flag: i32,
    // 父频道支持
    pub parent_channel_id: String,
    pub parent_channel_type: i32,
}

/// 频道成员实体 - 对应 channel_members 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMember {
    pub id: Option<i64>,
    pub channel_id: String,
    pub channel_type: i32,
    pub member_uid: String,
    pub member_name: String,
    pub member_remark: String,
    pub member_avatar: String,
    pub member_invite_uid: String,
    pub role: i32,
    pub status: i32,
    pub is_deleted: i32,
    pub robot: i32,
    pub version: i64,
    pub created_at: String,
    pub updated_at: String,
    pub extra: String,
    pub forbidden_expiration_time: i64,
    pub member_avatar_cache_key: String,
}

/// 消息扩展实体 - 对应 message_extra 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageExtra {
    pub id: Option<i64>,
    pub message_id: Option<String>,
    pub channel_id: Option<String>,
    pub channel_type: i16,
    pub readed: i32,
    pub readed_count: i32,
    pub unread_count: i32,
    pub revoke: i16,
    pub revoker: Option<String>,
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
    pub channel_id: String,
    pub channel_type: i32,
    pub uid: String,
    pub name: String,
    pub emoji: String,
    pub message_id: String,
    pub seq: i64,
    pub is_deleted: i32,
    pub created_at: Option<String>,
}

/// 提醒实体 - 对应 reminders 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: Option<i64>,
    pub reminder_id: i32,
    pub message_id: String,
    pub message_seq: u64,
    pub channel_id: String,
    pub channel_type: i16,
    pub uid: String,
    pub reminder_type: i32,
    pub text: String,
    pub data: String,
    pub is_locate: i16,
    pub version: i64,
    pub done: i16,
    pub need_upload: i16,
    pub publisher: Option<String>,
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
    pub updated_at: Option<String>,
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
    pub updated_at: Option<String>,
}

/// 会话扩展实体 - 对应 conversation_extra 表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationExtra {
    pub id: Option<i64>,
    pub channel_id: String,
    pub channel_type: i16,
    pub browse_to: u64,
    pub keep_message_seq: u64,
    pub keep_offset_y: i32,
    pub draft: String,
    pub version: i64,
    pub draft_updated_at: u64,
}

/// 查询参数结构体

/// 消息查询参数
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub channel_id: Option<String>,
    pub channel_type: Option<i32>,
    pub from_uid: Option<String>,
    pub message_type: Option<i32>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub order_by: Option<String>,
    pub is_deleted: Option<i32>,
}

/// 会话查询参数
#[derive(Debug, Clone, Default)]
pub struct ConversationQuery {
    pub channel_id: Option<String>,
    pub channel_type: Option<i32>,
    pub is_deleted: Option<i32>,
    pub has_unread: Option<bool>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 频道查询参数
#[derive(Debug, Clone, Default)]
pub struct ChannelQuery {
    pub channel_id: Option<String>,
    pub channel_type: Option<i32>,
    pub status: Option<i32>,
    pub is_deleted: Option<i32>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 频道成员查询参数
#[derive(Debug, Clone, Default)]
pub struct ChannelMemberQuery {
    pub channel_id: Option<String>,
    pub channel_type: Option<i32>,
    pub member_uid: Option<String>,
    pub role: Option<i32>,
    pub status: Option<i32>,
    pub is_deleted: Option<i32>,
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

/// 消息类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Text = 1,
    Image = 2,
    Voice = 3,
    Video = 4,
    File = 5,
    Location = 6,
    System = 100,
}

/// 频道类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    Person = 1,      // 个人聊天
    Group = 2,       // 群组聊天
    CustomerService = 3, // 客服聊天
    System = 4,      // 系统消息
}

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Member = 0,      // 普通成员
    Admin = 1,       // 管理员
    Owner = 2,       // 群主
    SuperAdmin = 3,  // 超级管理员
} 