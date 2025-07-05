//! 数据实体定义 - 对应数据库表结构
//! 
//! 这里定义了所有数据库表对应的 Rust 结构体，用于：
//! - 类型安全的数据传输
//! - 统一的数据表示
//! - 序列化/反序列化支持

use serde::{Deserialize, Serialize};

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
    pub viewed_at: i32,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    Sending = 0,     // 发送中
    Sent = 1,        // 已发送
    Failed = 2,      // 发送失败
    Delivered = 3,   // 已送达
    Read = 4,        // 已读
}

/// 成员角色枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Member = 0,      // 普通成员
    Admin = 1,       // 管理员
    Owner = 2,       // 群主
    SuperAdmin = 3,  // 超级管理员
} 