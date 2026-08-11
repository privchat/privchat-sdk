use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::StoredMessage;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageTextEntity {
    pub kind: String,
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub value: String,
    pub user_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageContentRef {
    pub kind: String,
    pub target_id: Option<String>,
    pub text: Option<String>,
}

/// UI-safe, typed projection. Wire/storage JSON never crosses this boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MessageContentProjection {
    pub kind: String,
    pub text: String,
    pub entities: Vec<MessageTextEntity>,
    pub reply_to_message_id: Option<String>,
    pub mentioned_user_ids: Vec<u64>,
    pub attachment_url: Option<String>,
    pub attachment_file_id: Option<u64>,
    pub thumbnail_url: Option<String>,
    pub thumbnail_file_id: Option<u64>,
    pub file_name: Option<String>,
    pub file_size: Option<i64>,
    pub duration: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub coordinate_system: Option<String>,
    pub location_name: Option<String>,
    pub address: Option<String>,
    pub poi_id: Option<String>,
    pub poi_source: Option<String>,
    pub link_url: Option<String>,
    pub link_title: Option<String>,
    pub link_description: Option<String>,
    pub contact_user_id: Option<u64>,
    pub contact_name: Option<String>,
    pub contact_avatar_url: Option<String>,
    pub system_template: Option<String>,
    pub system_refs: Vec<MessageContentRef>,
    pub money_ref_id: Option<String>,
    pub money_title: Option<String>,
    pub money_summary: Option<String>,
    pub money_status: Option<String>,
    pub money_amount_text: Option<String>,
    pub money_scene: Option<String>,
    pub money_type: Option<i32>,
}

pub fn project_stored_message(message: &StoredMessage) -> MessageContentProjection {
    let (text, reply_to, mentions) = decode_envelope(&message.content);
    let extra = parse_object(&message.extra);
    let content = parse_object(&text);
    let metadata = extra.as_ref().and_then(|v| v.get("metadata"));
    let sources = [content.as_ref(), metadata, extra.as_ref()];
    let mut body = MessageContentProjection {
        kind: kind_name(message.message_type).to_string(),
        text: text.clone(),
        reply_to_message_id: reply_to.or_else(|| string_at(&sources, &["reply_to_message_id"])),
        mentioned_user_ids: if mentions.is_empty() {
            u64_list_at(&sources, "mentioned_user_ids")
        } else {
            mentions
        },
        ..Default::default()
    };
    body.entities = scan_entities(&body.text, &body.mentioned_user_ids);

    body.attachment_url = string_at(&sources, &["url", "attachment_url"]);
    body.attachment_file_id = u64_at(&sources, &["file_id", "attachment_file_id"]);
    body.thumbnail_url = string_at(&sources, &["thumbnail_url", "thumbnail"]);
    body.thumbnail_file_id = u64_at(&sources, &["thumbnail_file_id"]);
    body.file_name = string_at(&sources, &["file_name", "name"]);
    body.file_size = i64_at(&sources, &["file_size", "size"]);
    body.duration = i32_at(&sources, &["duration"]);
    body.width = i32_at(&sources, &["width"]);
    body.height = i32_at(&sources, &["height"]);
    body.latitude = f64_at(&sources, &["latitude", "lat"]);
    body.longitude = f64_at(&sources, &["longitude", "lng"]);
    body.coordinate_system = string_at(&sources, &["coordinate_system"]);
    body.location_name = string_at(&sources, &["name"]);
    body.address = string_at(&sources, &["address"]);
    body.poi_id = string_at(&sources, &["poi_id"]);
    body.poi_source = string_at(&sources, &["poi_source"]);
    body.link_url = string_at(&sources, &["url"]);
    body.link_title = string_at(&sources, &["title"]);
    body.link_description = string_at(&sources, &["description"]);
    body.contact_user_id = u64_at(&sources, &["user_id", "userId"]);
    body.contact_name = string_at(&sources, &["name", "nickname"]);
    body.contact_avatar_url = string_at(&sources, &["avatar", "avatar_url"]);

    if message.message_type == 5 {
        body.system_template = string_at(&sources, &["template"]);
        body.system_refs = refs_at(&sources);
        if let Some(value) = string_at(&sources, &["content", "text", "tip"]) {
            body.text = value;
        } else if let Some(template) = body.system_template.clone() {
            // A system template is the only safe generic fallback. Never leak its JSON payload.
            body.text = template;
        } else if looks_like_json(&body.text) {
            body.text.clear();
        }
        body.money_ref_id = body
            .system_refs
            .iter()
            .find(|r| r.kind == "red_packet")
            .and_then(|r| r.target_id.clone());
    } else if message.message_type == 11 || message.message_type == 12 {
        body.money_ref_id = string_at(&sources, &["redPacketId", "transferId"]);
        body.money_title = string_at(&sources, &["title"]);
        body.money_summary = string_at(&sources, &["summary"]);
        body.money_status = string_at(&sources, &["status"]);
        body.money_amount_text = string_at(&sources, &["amountText"]);
        body.money_scene = string_at(&sources, &["scene"]);
        body.money_type = i32_at(&sources, &["type"]);
        // Money cards render structured fields. Their generic preview may use only a safe caption.
        body.text = body
            .money_title
            .clone()
            .or_else(|| body.money_summary.clone())
            .unwrap_or_default();
    } else if message.message_type == 0 {
        if let Some(value) = string_at(&sources, &["content", "text"]) {
            body.text = value;
        }
        body.entities = scan_entities(&body.text, &body.mentioned_user_ids);
    } else if let Some(caption) = string_at(&sources, &["caption"]) {
        // 附件的说明文字是消息内容的一部分，跟图片一起显示。
        body.text = caption;
        body.entities = scan_entities(&body.text, &body.mentioned_user_ids);
    } else if is_attachment_placeholder(message.message_type, &body.text) {
        // 🔴 `[图片]` 不是用户写的字，是没有说明文字时给会话列表看的占位文案
        // （TS/Web 也按这个约定发）。当成 caption 的话，重发一次就凭空多出一句
        // 「[图片]」，再发一次还会叠上去。
        body.text.clear();
        body.entities.clear();
    } else if looks_like_json(&body.text) {
        // Non-text renderers consume typed fields. An unsupported renderer must not expose JSON.
        body.text.clear();
        body.entities.clear();
    }
    body
}

/// 这条正文是不是「没有说明文字」时的占位文案。
///
/// 只对附件类消息成立：文本消息里用户真的可以就发「[图片]」三个字。
fn is_attachment_placeholder(message_type: i32, text: &str) -> bool {
    use privchat_protocol::message::ContentMessageType;
    let placeholder = match message_type {
        t if t == ContentMessageType::Voice as i32 => "[语音]",
        t if t == ContentMessageType::Image as i32 => "[图片]",
        t if t == ContentMessageType::Video as i32 => "[视频]",
        t if t == ContentMessageType::File as i32 => "[文件]",
        _ => return false,
    };
    text.trim() == placeholder
}

fn kind_name(value: i32) -> &'static str {
    match value {
        0 => "text",
        1 => "voice",
        2 => "image",
        3 => "video",
        4 => "file",
        5 => "system",
        6 => "sticker",
        7 => "contact",
        8 => "location",
        9 => "link",
        10 => "forward",
        11 => "red_packet",
        12 => "money_transfer",
        _ => "unknown",
    }
}

fn decode_envelope(raw: &str) -> (String, Option<String>, Vec<u64>) {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return (raw.to_string(), None, vec![]);
    };
    let Some(obj) = value.as_object() else {
        return (raw.to_string(), None, vec![]);
    };
    let marked = obj.contains_key("metadata")
        || obj.contains_key("reply_to_message_id")
        || obj.contains_key("mentioned_user_ids")
        || obj.contains_key("message_source");
    if !marked {
        return (raw.to_string(), None, vec![]);
    }
    let Some(text) = obj.get("content").and_then(Value::as_str) else {
        return (raw.to_string(), None, vec![]);
    };
    let reply = obj.get("reply_to_message_id").and_then(value_string);
    let mentions = obj
        .get("mentioned_user_ids")
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    (text.to_string(), reply, mentions)
}

fn parse_object(raw: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .filter(Value::is_object)
}
/// 这段正文是不是原始 JSON 载荷。
///
/// 🔴 只看首字符是不是 `{`/`[` 是不够的：`[加班] 看这张` 这种正经说明文字也以 `[`
/// 开头，会被当成载荷整段清掉。判据是「能不能解析成 JSON」。
fn looks_like_json(value: &str) -> bool {
    let trimmed = value.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return false;
    }
    serde_json::from_str::<Value>(trimmed).is_ok()
}
fn value_string(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v.as_u64().map(|v| v.to_string()))
        .or_else(|| v.as_i64().map(|v| v.to_string()))
}
fn find<'a>(sources: &[Option<&'a Value>], keys: &[&str]) -> Option<&'a Value> {
    sources
        .iter()
        .flatten()
        .find_map(|v| keys.iter().find_map(|k| v.get(k)))
}
fn string_at(s: &[Option<&Value>], k: &[&str]) -> Option<String> {
    find(s, k).and_then(value_string)
}
fn u64_at(s: &[Option<&Value>], k: &[&str]) -> Option<u64> {
    find(s, k).and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
}
fn i64_at(s: &[Option<&Value>], k: &[&str]) -> Option<i64> {
    find(s, k).and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}
fn i32_at(s: &[Option<&Value>], k: &[&str]) -> Option<i32> {
    i64_at(s, k).and_then(|v| i32::try_from(v).ok())
}
fn f64_at(s: &[Option<&Value>], k: &[&str]) -> Option<f64> {
    find(s, k).and_then(|v| v.as_f64().or_else(|| v.as_str()?.parse().ok()))
}
fn u64_list_at(s: &[Option<&Value>], key: &str) -> Vec<u64> {
    find(s, &[key])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                .collect()
        })
        .unwrap_or_default()
}
fn refs_at(s: &[Option<&Value>]) -> Vec<MessageContentRef> {
    find(s, &["refs"])
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    Some(MessageContentRef {
                        kind: v.get("type")?.as_str()?.to_string(),
                        target_id: v.get("target_id").and_then(value_string),
                        text: v.get("text").and_then(Value::as_str).map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn scan_entities(text: &str, mentions: &[u64]) -> Vec<MessageTextEntity> {
    let patterns = [
        ("url", r#"https?://[^\s<>{}\[\]\"']+"#),
        ("email", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        ("phone", r"(?:\+?86[- ]?)?1[3-9][0-9]{9}"),
        ("number", r"[0-9]+"),
    ];
    let mut out = Vec::new();
    for (kind, pattern) in patterns {
        for m in Regex::new(pattern).expect("static regex").find_iter(text) {
            if kind == "phone" && has_adjacent_digit(text, m.start(), m.end()) {
                continue;
            }
            out.push(entity(kind, text, m.start(), m.end(), None));
        }
    }
    let mention_re = Regex::new(r"@[\p{L}\p{N}_-]+").expect("static regex");
    for (index, m) in mention_re.find_iter(text).enumerate() {
        out.push(entity(
            "mention",
            text,
            m.start(),
            m.end(),
            mentions.get(index).copied(),
        ));
    }
    out.sort_by_key(|e| e.start);
    let mut accepted: Vec<MessageTextEntity> = Vec::new();
    for item in out {
        if !accepted
            .iter()
            .any(|v| item.start < v.end && item.end > v.start)
        {
            accepted.push(item);
        }
    }
    accepted
}

fn has_adjacent_digit(text: &str, start: usize, end: usize) -> bool {
    text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_ascii_digit())
        || text[end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit())
}
fn entity(
    kind: &str,
    text: &str,
    start: usize,
    end: usize,
    user_id: Option<u64>,
) -> MessageTextEntity {
    let raw = &text[start..end];
    MessageTextEntity {
        kind: kind.to_string(),
        start: text[..start].encode_utf16().count() as u32,
        end: text[..end].encode_utf16().count() as u32,
        text: raw.to_string(),
        value: if kind == "mention" {
            raw[1..].to_string()
        } else if kind == "phone" {
            raw.replace([' ', '-'], "")
        } else {
            raw.to_string()
        },
        user_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_projection_is_typed_and_uses_utf16_offsets() {
        let mut message: StoredMessage = serde_json::from_value(serde_json::json!({"message_id":1,"server_message_id":null,"local_message_id":null,"channel_id":1,"channel_type":1,"from_uid":2,"message_type":0,"content":"😀 @客服 https://example.com hi@example.com 13800138000","status":2,"created_at":1,"updated_at":1,"extra":"{\"mentioned_user_ids\":[9]}","revoked":false,"revoked_by":null,"mime_type":null,"media_downloaded":false,"thumb_status":0,"delivered":false,"pts":null})).unwrap();
        message.extra = "{\"mentioned_user_ids\":[9]}".into();
        let body = project_stored_message(&message);
        assert_eq!(body.entities[0].start, 3);
        assert_eq!(body.entities[0].user_id, Some(9));
        assert_eq!(body.entities.len(), 4);
        assert_eq!(body.entities[2].kind, "email");
        assert_eq!(body.entities[3].value, "13800138000");
    }

    #[test]
    fn scanner_distinguishes_phone_numbers_from_other_numbers() {
        let text = "身份证：422124195812090021\n手机号码：13684915671";
        let entities = scan_entities(text, &[]);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].kind, "number");
        assert_eq!(entities[0].text, "422124195812090021");
        assert_eq!(entities[1].kind, "phone");
        assert_eq!(entities[1].text, "13684915671");
    }

    #[test]
    fn structured_payloads_never_fall_back_to_raw_json() {
        let message: StoredMessage = serde_json::from_value(serde_json::json!({
            "message_id": 2, "server_message_id": null, "local_message_id": null,
            "channel_id": 1, "channel_type": 1, "from_uid": 2, "message_type": 11,
            "content": "{\"redPacketId\":\"rp-1\",\"title\":\"恭喜发财\"}",
            "status": 2, "created_at": 1, "updated_at": 1, "extra": "{}",
            "revoked": false, "revoked_by": null, "mime_type": null,
            "media_downloaded": false, "thumb_status": 0, "delivered": false, "pts": null
        }))
        .unwrap();
        let body = project_stored_message(&message);
        assert_eq!(body.text, "恭喜发财");

        let mut unsupported = message;
        unsupported.message_type = 2;
        let body = project_stored_message(&unsupported);
        assert!(body.text.is_empty());
    }
}

#[cfg(test)]
mod caption_projection_tests {
    use super::*;

    /// 收方拿到的形态：`content` 是 wire envelope 的正文（caption 或占位文案），
    /// 附件描述在 `extra.metadata` 里。
    fn received(content: &str, metadata_extra: &str) -> StoredMessage {
        let mut m = attachment(metadata_extra);
        m.content = content.to_string();
        m
    }

    fn attachment(extra: &str) -> StoredMessage {
        StoredMessage {
            message_id: 1,
            server_message_id: None,
            local_message_id: None,
            channel_id: 10,
            channel_type: 1,
            from_uid: 7,
            // 🔴 Image = 2。之前写 1 还注释成 image，测的其实是语音消息。
            message_type: privchat_protocol::message::ContentMessageType::Image as i32,
            content: r#"{"file_id":42,"file_url":"http://cdn/a.jpg"}"#.to_string(),
            status: 0,
            created_at: 0,
            updated_at: 0,
            extra: extra.to_string(),
            revoked: false,
            revoked_by: None,
            mime_type: Some("image/jpeg".to_string()),
            media_downloaded: true,
            thumb_status: 1,
            delivered: false,
            pts: None,
        }
    }

    /// 🔴 附件的说明文字要跟图片一起显示；以前这一支一律清空 text，配的话就没了。
    #[test]
    fn a_caption_is_projected_as_the_message_text() {
        let body = project_stored_message(&attachment(
            r#"{"file_name":"a.jpg","caption":"周末爬山"}"#,
        ));
        assert_eq!(body.text, "周末爬山");
    }

    /// 没有说明时仍然不能把附件 JSON 泄露成正文。
    #[test]
    fn without_a_caption_the_json_is_not_exposed() {
        let body = project_stored_message(&attachment(r#"{"file_name":"a.jpg"}"#));
        assert_eq!(body.text, "");
    }

    /// 🔴 收方看到的正文是 `[图片]` —— 那是占位文案，不是用户写的字。
    /// 把它当 caption，重发一次就凭空多出一句「[图片]」。
    #[test]
    fn a_placeholder_is_not_a_caption() {
        let body = project_stored_message(&received(
            "[图片]",
            r#"{"content":"[图片]","metadata":{"file_id":42,"file_name":"a.jpg"}}"#,
        ));
        assert_eq!(body.text, "");
    }

    /// 真的写了说明文字的那条，正文照常是那句话。
    #[test]
    fn a_received_caption_is_kept() {
        let body = project_stored_message(&received(
            "周末爬山",
            r#"{"content":"周末爬山","metadata":{"file_id":42,"file_name":"a.jpg"},"caption":"周末爬山"}"#,
        ));
        assert_eq!(body.text, "周末爬山");
    }

    /// 🔴 以 `[` 开头的说明文字不是 JSON 载荷，不许整段清掉。
    #[test]
    fn a_caption_may_start_with_a_bracket() {
        let body = project_stored_message(&received(
            "[加班] 看这张",
            r#"{"content":"[加班] 看这张","metadata":{"file_id":42,"file_name":"a.jpg"}}"#,
        ));
        assert_eq!(body.text, "[加班] 看这张");
    }

    /// 原始载荷仍然不能泄露成正文。
    #[test]
    fn a_raw_payload_is_still_hidden() {
        let body = project_stored_message(&received(
            r#"{"file_id":42,"file_url":"http://cdn/a.jpg"}"#,
            r#"{"metadata":{"file_id":42}}"#,
        ));
        assert_eq!(body.text, "");
    }

    /// 文本消息里「[图片]」就是用户真发的三个字，不许清掉。
    #[test]
    fn a_text_message_may_literally_say_that() {
        let mut m = attachment("{}");
        m.message_type = privchat_protocol::message::ContentMessageType::Text as i32;
        m.content = "[图片]".to_string();
        let body = project_stored_message(&m);
        assert_eq!(body.text, "[图片]");
    }
}
