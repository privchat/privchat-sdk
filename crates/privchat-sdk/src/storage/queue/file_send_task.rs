//! 文件发送任务 - 用于文件消费队列（步骤 5～8）
//!
//! 与 SendTask 分离：附件消息入文件队列，文本消息入消息队列。

use serde::{Deserialize, Serialize};

/// 文件发送任务（一条需上传附件的消息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSendTask {
    /// 本地消息 ID（message.id）
    pub id: i64,
    /// 用户 ID（用于定位 files 目录）
    pub uid: String,
    /// 频道 ID
    pub channel_id: u64,
    /// 频道类型 (1: 个人, 2: 群聊)
    pub channel_type: i32,
    /// 发送方 uid
    pub from_uid: u64,
    /// 客户端 local_message_id（发往服务端去重）
    pub local_message_id: u64,
    /// 消息类型："image" | "video" | "file"
    pub message_type: String,
    /// 消息插入时间戳（毫秒），用于 yyyymm/yyyymmdd 路径
    pub timestamp_ms: i64,
    /// 预上传的缩略图 file_id（仅视频无回调时由发送前上传 1x1 PNG 得到，消费者直接使用不再上传缩略图）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_uploaded_thumbnail_file_id: Option<String>,
}

impl FileSendTask {
    pub fn new(
        id: i64,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        local_message_id: u64,
        message_type: String,
        timestamp_ms: i64,
        pre_uploaded_thumbnail_file_id: Option<String>,
    ) -> Self {
        Self {
            id,
            uid,
            channel_id,
            channel_type,
            from_uid,
            local_message_id,
            message_type,
            timestamp_ms,
            pre_uploaded_thumbnail_file_id,
        }
    }

    /// 是否为图片/视频（需要先传缩略图再传本体）
    pub fn needs_thumbnail(&self) -> bool {
        self.message_type == "image" || self.message_type == "video"
    }
}
