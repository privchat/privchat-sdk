//! SDK 消息类型定义
//!
//! 与 protocol 层 ContentMessageType（u32）对齐：发送时字符串转 u32 填协议字段，接收时从协议 message_type 取 u32 转字符串。

use privchat_protocol::ContentMessageType;
use serde_json::Value;
use std::fmt;

/// 消息类型就是字符串，支持无限扩展
pub type ChatMessageType = String;

/// 已知消息类型常量
/// 
/// 这些常量提供类型安全和 IDE 自动补全，但业务系统可以定义任意其他类型
pub mod message_types {
    /// 文本消息
    pub const TEXT: &str = "text";
    /// 图片消息
    pub const IMAGE: &str = "image";
    /// 语音消息
    pub const AUDIO: &str = "audio";
    /// 视频消息
    pub const VIDEO: &str = "video";
    /// 文件消息
    pub const FILE: &str = "file";
    /// 位置消息
    pub const LOCATION: &str = "location";
    /// 系统消息（包括系统通知、群组操作等）
    pub const SYSTEM: &str = "system";
    /// 名片消息
    pub const CONTACT_CARD: &str = "contact_card";
    /// 表情包消息
    pub const STICKER: &str = "sticker";
    /// 转发消息
    pub const FORWARD: &str = "forward";
}

/// 消息类型工具函数
/// 
/// 由于 ChatMessageType 是类型别名，不能定义 inherent impl
/// 所以使用模块级函数

/// 判断是否是已知的消息类型
pub fn is_known_message_type(msg_type: &str) -> bool {
        matches!(
            msg_type,
            message_types::TEXT
                | message_types::IMAGE
                | message_types::AUDIO
                | message_types::VIDEO
                | message_types::FILE
                | message_types::LOCATION
                | message_types::SYSTEM
                | message_types::CONTACT_CARD
                | message_types::STICKER
                | message_types::FORWARD
        )
    }

/// 获取消息类型的显示名称
pub fn message_type_display_name(msg_type: &str) -> &str {
        match msg_type {
            message_types::TEXT => "文本",
            message_types::IMAGE => "图片",
            message_types::AUDIO => "语音",
            message_types::VIDEO => "视频",
            message_types::FILE => "文件",
            message_types::LOCATION => "位置",
            message_types::SYSTEM => "系统消息",
            message_types::CONTACT_CARD => "名片",
            message_types::STICKER => "表情",
            message_types::FORWARD => "转发",
            _ => "未知消息类型",
        }
    }

/// 获取消息的占位符文本
/// 
/// 对于未知消息类型，返回【未知消息类型】
/// 对于已知类型，返回对应的占位符
pub fn message_type_placeholder(msg_type: &str) -> String {
    if !is_known_message_type(msg_type) {
            return "【未知消息类型】".to_string();
        }

        match msg_type {
            message_types::TEXT => "[文本]".to_string(),
            message_types::IMAGE => "[图片]".to_string(),
            message_types::AUDIO => "[语音]".to_string(),
            message_types::VIDEO => "[视频]".to_string(),
            message_types::FILE => "[文件]".to_string(),
            message_types::LOCATION => "[位置]".to_string(),
            message_types::SYSTEM => "[系统消息]".to_string(),
            message_types::CONTACT_CARD => "[名片]".to_string(),
            message_types::STICKER => "[表情]".to_string(),
            message_types::FORWARD => "[转发]".to_string(),
            _ => format!("[{}]", msg_type),
        }
    }

/// 协议层 message_type（u32）转字符串（用于显示、存储）
pub fn message_type_from_u32(value: u32) -> String {
    ContentMessageType::from_u32(value)
        .map(|t| t.as_str().to_string())
        .unwrap_or_else(|| message_types::TEXT.to_string())
}

/// 字符串转协议层 message_type（u32），用于发送时填 SendMessageRequest.message_type
pub fn message_type_str_to_u32(s: &str) -> u32 {
    match s.to_lowercase().as_str() {
        message_types::TEXT => ContentMessageType::Text.as_u32(),
        message_types::IMAGE => ContentMessageType::Image.as_u32(),
        message_types::FILE => ContentMessageType::File.as_u32(),
        "voice" => ContentMessageType::Voice.as_u32(),
        message_types::VIDEO => ContentMessageType::Video.as_u32(),
        message_types::SYSTEM => ContentMessageType::System.as_u32(),
        message_types::AUDIO => ContentMessageType::Audio.as_u32(),
        message_types::LOCATION => ContentMessageType::Location.as_u32(),
        message_types::CONTACT_CARD => ContentMessageType::ContactCard.as_u32(),
        message_types::STICKER => ContentMessageType::Sticker.as_u32(),
        message_types::FORWARD => ContentMessageType::Forward.as_u32(),
        _ => ContentMessageType::Text.as_u32(),
    }
}

/// 从 channel_type (u8) 解析消息类型字符串（兼容旧逻辑，优先使用协议 message_type）
pub fn message_type_from_channel_type(channel_type: u8) -> String {
    message_type_from_u32(channel_type as u32)
}

/// 从 payload JSON 中解析消息类型
/// 
/// payload 应该是 JSON 格式：{"content": "...", "metadata": {...}}
/// 如果 payload 中包含 message_type 字段，优先使用该字段
/// 返回：(消息类型, 内容, 元数据)
pub fn message_type_from_payload(payload: &[u8]) -> (String, String, Option<Value>) {
        // 尝试解析为 JSON
        match serde_json::from_slice::<Value>(payload) {
            Ok(json) => {
                // 检查是否有 message_type 字段
                if let Some(msg_type_str) = json.get("message_type").and_then(|v| v.as_str()) {
                    let msg_type = msg_type_str.to_string();
                    let content = json
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| String::from_utf8_lossy(payload).to_string());
                    // metadata 保持原样，不包含 forward_from（forward_from 在顶层）
                    let metadata = json.get("metadata").cloned();
                    return (msg_type, content, metadata);
                }

                // 如果没有 message_type，尝试从 metadata 中推断
                if let Some(metadata) = json.get("metadata").and_then(|v| v.as_object()) {
                    // 检查 metadata 中的键名来推断消息类型
                    if metadata.contains_key("image") {
                        return (
                            message_types::IMAGE.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[图片]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("video") {
                        return (
                            message_types::VIDEO.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[视频]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("audio") {
                        return (
                            message_types::AUDIO.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[语音]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("file") {
                        return (
                            message_types::FILE.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[文件]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("sticker") {
                        return (
                            message_types::STICKER.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[表情]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("contact_card") {
                        return (
                            message_types::CONTACT_CARD.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[名片]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("location") {
                        return (
                            message_types::LOCATION.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[位置]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if metadata.contains_key("forward") {
                        // 批量转发消息：metadata 包含 forward 对象
                        return (
                            message_types::FORWARD.to_string(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| "[转发]".to_string()),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    } else if let Some(custom_key) = metadata.keys().next() {
                        // 自定义消息类型：使用 metadata 的键名作为消息类型
                        return (
                            custom_key.clone(),
                            json.get("content")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| format!("[{}]", custom_key)),
                            Some(json.get("metadata").cloned().unwrap_or(Value::Null)),
                        );
                    }
                }

                // 如果有 content 字段，假设是文本消息
                if json.get("content").is_some() {
                    return (
                        message_types::TEXT.to_string(),
                        json.get("content")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_default(),
                        json.get("metadata").cloned(),
                    );
                }
            }
            Err(_) => {
                // 不是 JSON，尝试作为纯文本处理
            }
        }

        // 默认作为文本消息处理
        let content = String::from_utf8_lossy(payload).to_string();
        (message_types::TEXT.to_string(), content, None)
    }

/// 检查是否需要特殊处理（非文本消息）
pub fn message_type_needs_special_handling(msg_type: &str) -> bool {
    msg_type != message_types::TEXT && is_known_message_type(msg_type)
}

/// 解析后的消息内容
#[derive(Debug, Clone)]
pub struct ParsedMessage {
    /// 消息类型（字符串）
    pub message_type: String,
    /// 消息内容（用于显示）
    pub content: String,
    /// 消息元数据（结构化数据）
    pub metadata: Option<Value>,
}

impl ParsedMessage {
    /// 从 PushMessageRequest 解析消息
    /// 
    /// 根据 channel_type 和 payload 解析消息类型和内容
    pub fn from_push_message_request(channel_type: u8, payload: &[u8]) -> Self {
        // 首先尝试从 payload 解析（优先级更高）
        let (msg_type, content, metadata) = message_type_from_payload(payload);

        // 如果从 payload 解析出的是 text 且没有 metadata，尝试使用 channel_type
        let final_type = if msg_type == message_types::TEXT && metadata.is_none() {
            message_type_from_channel_type(channel_type)
        } else {
            msg_type
        };

        // 如果是未知类型或内容为空，使用占位符
        let display_content = if !is_known_message_type(&final_type) {
            message_type_placeholder(&final_type)
        } else if content.is_empty() {
            message_type_placeholder(&final_type)
        } else {
            content
        };

        Self {
            message_type: final_type,
            content: display_content,
            metadata,
        }
    }

    /// 获取用于显示的消息文本
    /// 
    /// 对于未知消息类型，返回【未知消息类型】
    pub fn display_text(&self) -> &str {
        if !is_known_message_type(&self.message_type) {
            "【未知消息类型】"
        } else {
            &self.content
        }
    }

    /// 检查是否是未知消息类型
    pub fn is_unknown(&self) -> bool {
        !is_known_message_type(&self.message_type)
    }

    /// 检查是否是已知的消息类型
    pub fn is_known(&self) -> bool {
        is_known_message_type(&self.message_type)
    }
}

impl fmt::Display for ParsedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_known() {
        assert!(is_known_message_type(message_types::TEXT));
        assert!(is_known_message_type(message_types::IMAGE));
        assert!(!is_known_message_type("red_packet"));
        assert!(!is_known_message_type("unknown_type"));
    }

    #[test]
    fn test_from_channel_type() {
        assert_eq!(
            message_type_from_channel_type(0),
            message_types::TEXT
        );
        assert_eq!(
            message_type_from_channel_type(1),
            message_types::IMAGE
        );
        assert_eq!(
            message_type_from_channel_type(10),
            message_types::CONTACT_CARD
        );
        assert_eq!(message_type_from_channel_type(100), "custom_100");
        assert_eq!(message_type_from_channel_type(99), "unknown_99");
    }

    #[test]
    fn test_placeholder() {
        assert_eq!(
            message_type_placeholder("unknown_type"),
            "【未知消息类型】"
        );
        assert_eq!(
            message_type_placeholder(message_types::TEXT),
            "[文本]"
        );
        assert_eq!(
            message_type_placeholder(message_types::IMAGE),
            "[图片]"
        );
    }

    #[test]
    fn test_from_payload() {
        // 测试 JSON payload with message_type
        let json_payload = r#"{"message_type": "image", "content": "Hello", "metadata": {"image": {"url": "test.jpg"}}}"#;
        let (msg_type, content, metadata) = message_type_from_payload(json_payload.as_bytes());
        assert_eq!(msg_type, message_types::IMAGE);
        assert_eq!(content, "Hello");
        assert!(metadata.is_some());

        // 测试 JSON payload without message_type, infer from metadata
        let json_payload = r#"{"content": "Hello", "metadata": {"image": {"url": "test.jpg"}}}"#;
        let (msg_type, content, _) = message_type_from_payload(json_payload.as_bytes());
        assert_eq!(msg_type, message_types::IMAGE);
        assert_eq!(content, "Hello");

        // 测试纯文本 payload
        let text_payload = b"Hello, World!";
        let (msg_type, content, _) = message_type_from_payload(text_payload);
        assert_eq!(msg_type, message_types::TEXT);
        assert_eq!(content, "Hello, World!");

        // 测试自定义消息类型
        let custom_payload = r#"{"content": "红包", "metadata": {"red_packet": {"amount": 100}}}"#;
        let (msg_type, content, _) = message_type_from_payload(custom_payload.as_bytes());
        assert_eq!(msg_type, "red_packet");
        assert_eq!(content, "红包");
    }

    #[test]
    fn test_parsed_message() {
        // 测试已知消息类型
        let parsed = ParsedMessage::from_push_message_request(0, b"Hello");
        assert_eq!(parsed.message_type, message_types::TEXT);
        assert_eq!(parsed.content, "Hello");
        assert!(!parsed.is_unknown());

        // 测试未知消息类型
        let unknown_payload = r#"{"message_type": "red_packet", "content": "红包"}"#;
        let parsed = ParsedMessage::from_push_message_request(0, unknown_payload.as_bytes());
        assert_eq!(parsed.message_type, "red_packet");
        assert!(parsed.is_unknown());
        assert_eq!(parsed.display_text(), "【未知消息类型】");
    }
}
