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
    } else if looks_like_json(&body.text) {
        // Non-text renderers consume typed fields. An unsupported renderer must not expose JSON.
        body.text.clear();
        body.entities.clear();
    }
    body
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
fn looks_like_json(value: &str) -> bool {
    let trimmed = value.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
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
