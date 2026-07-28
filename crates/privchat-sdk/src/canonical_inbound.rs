//! 服务端来的一条消息，规范化之后的样子。
//!
//! 四条**投影**来源——realtime push、`sync/get_difference` commit、
//! `message/history/get`、`message/history/around`——的 transport 形态本来就不一样：
//! 字段名不同、
//! 时间单位不同（push 是秒，其余是毫秒）、metadata 有的在 envelope 里有的在顶层。
//! 以前每条来源各自拼一份数据库投影，于是「同一条消息从哪条路进来」决定了它在本地
//! 长什么样：history 进来的图片没有 `extra`（缩略图永远下不来），push 进来的时间少
//! 了三个数量级（新消息排到会话最前）。两个线上事故，同一个形状。
//!
//! 现在的规则：**来源只做适配，不做投影**。
//!
//! send ACK **不在其列**,这是有意的:它不构造消息行,只在已有的乐观行上补服务端
//! 身份与顺序(`outbox_ack_sent` 只写 server_message_id / status / pts,连
//! created_at 都不碰——本机发送时那个值本来就是准的)。给它硬造一个 adapter 只会
//! 多一个没有生产调用者的函数,和一条测得很好看却不存在的路径。
//!
//! ```text
//!   push / sync commit / history / around
//!                      ↓  (各自的 from_* 适配器)
//!              CanonicalInboundMessage
//!                      ↓  (唯一一条投影)
//!                UpsertRemoteMessageInput
//! ```
//!
//! 判断两条路径是否一致，比的是 [`SemanticProjection`]——语义字段，不是数据库行：
//! 行里还有 `message.id`、status、本地媒体状态这些本来就该不同的东西。

use serde::{Deserialize, Serialize};

use crate::UpsertRemoteMessageInput;

/// 时间戳精度。wire 上两种都有，合并时必须知道手里这个是哪种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TimePrecision {
    /// `PushMessageRequest.timestamp`（u32 秒）。
    Seconds,
    /// history / sync（毫秒）。缺省取这一档:绝大多数写入路径都是毫秒,而把毫秒
    /// 误标成秒会让它被任何来源覆盖(更危险的方向)。
    #[default]
    Milliseconds,
}

/// 一条服务端消息的规范形态。所有来源适配到这里，投影只从这里出去。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalInboundMessage {
    /// 服务端全局标识。>0 才算确认消息。
    pub server_message_id: u64,
    /// 发送命令的幂等键；0 表示这条不是本机发出的。
    pub local_message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    /// `ContentMessageType` 判别值（Text=0, Voice=1, Image=2, Video=3, File=4 …）。
    pub message_type: i32,
    /// 用于显示的正文。媒体消息是占位文案（如 `[图片]`），细节在 `extra`。
    pub content: String,
    /// canonical envelope JSON：`{"content": …, "metadata": {…}}`。
    ///
    /// 空串只允许表示「服务端确实没给 metadata」，不允许表示「这条路径忘了带」——
    /// 这个区别就是图片能不能加载的区别，见 [`Self::has_metadata`]。
    pub extra: String,
    /// per-channel 权威顺序。
    pub pts: i64,
    /// **发送时间，毫秒**。适配器负责把秒的来源乘到毫秒，读取方不再猜。
    pub sent_at_ms: i64,
    /// 这个时间戳**原本**的精度。归一之后数值上已看不出来了，所以必须显式带着走:
    /// 合并两条来源时靠它决定谁覆盖谁，靠数值形状去猜是猜不准的（真实发送时间正好
    /// 落在整秒上完全可能）。
    pub sent_at_precision: TimePrecision,
    pub revoked: bool,
}

/// 跨路径比较用的语义投影。
///
/// 只包含「同一条消息不论从哪条路进来都必须相同」的字段。刻意排除：
/// 本地 `message.id`、`status`（history 落 2、乐观发送落 0）、`local_message_id`
/// （只有自己发的那条路径有）、原始 transport bytes、本地媒体路径与下载状态。
/// 把这些一起比会得到一个永远红的测试，然后被人删掉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticProjection {
    pub server_message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    /// 归一化后的 metadata（键序无关，缺失为 None）。
    pub metadata: Option<serde_json::Value>,
    pub pts: i64,
    pub sent_at_ms: i64,
    pub revoked: bool,
}

impl SemanticProjection {
    /// 两条来源是否描述了同一条消息。
    ///
    /// 除 `sent_at_ms` 外全部严格相等；`sent_at_ms` 只要求**落在同一秒**——
    /// `PushMessageRequest.timestamp` 是 u32 秒，它在结构上就给不出毫秒，要求逐字段
    /// 相等只会得到一个永远红的门禁，然后被人删掉。真正要卡的是「同一条消息在不同
    /// 来源上指向同一个时刻」，秒内差异不是分叉。
    ///
    /// 反过来说：**低精度来源不得覆盖已有的高精度值**（见 `prefer_precise_sent_at`），
    /// 否则一条 history 拿到 .317 的消息会被随后的 push 退化成 .000，时间戳会随
    /// 「最后一条到达的路径」抖动。
    pub fn agrees_with(&self, other: &Self) -> bool {
        self.server_message_id == other.server_message_id
            && self.channel_id == other.channel_id
            && self.channel_type == other.channel_type
            && self.from_uid == other.from_uid
            && self.message_type == other.message_type
            && self.content == other.content
            && self.metadata == other.metadata
            && self.pts == other.pts
            && self.revoked == other.revoked
            && self.sent_at_ms / 1_000 == other.sent_at_ms / 1_000
    }
}

/// 已有值与新值指向同一秒时，保留精度更高的那个。
///
/// 秒精度只可能来自 push；任何毫秒值都比它更接近真实发送时刻。跨秒则以新值为准
/// （那是真的更新，不是精度差）。
pub fn prefer_precise_sent_at(existing_ms: i64, incoming_ms: i64) -> i64 {
    if existing_ms > 0 && existing_ms / 1_000 == incoming_ms / 1_000 {
        existing_ms.max(incoming_ms)
    } else {
        incoming_ms
    }
}

/// 10^11 毫秒 = 1973 年。真毫秒必在其上，真秒必在其下。
///
/// 这个判据存在是因为 wire 上两种单位都有：`PushMessageRequest.timestamp` 是 u32
/// 秒（u32 装不下毫秒纪元，这是类型给的硬约束），history/sync 是毫秒。秒值当毫秒
/// 存下去会落到 1970，于是「刚收到的消息」比整个会话都旧。
const MIN_PLAUSIBLE_MS: i64 = 100_000_000_000;

/// 把任意来源的时间戳归一到毫秒。
pub fn normalize_sent_at_ms(value: i64) -> i64 {
    match value {
        v if v >= MIN_PLAUSIBLE_MS => v,
        v if v > 0 => v.saturating_mul(1_000),
        _ => 0,
    }
}

/// 构造 canonical envelope。`metadata` 为 None 时返回空串（表示服务端确实没给）。
pub fn build_extra_envelope(content: &str, metadata: Option<&serde_json::Value>) -> String {
    match metadata {
        Some(metadata) if !metadata.is_null() => {
            serde_json::json!({ "content": content, "metadata": metadata }).to_string()
        }
        _ => String::new(),
    }
}

impl CanonicalInboundMessage {
    /// `extra` 里是否带着可用的 metadata。
    ///
    /// 缩略图状态这类**终态**判定必须先过这一关：解析不出 metadata 时只能停在
    /// 未知/待重试，不能写「这条消息没有缩略图」——那是拿「我没看见」当「不存在」，
    /// 上一次就是这么把整段历史的图片永久变成灰块的。
    pub fn has_metadata(&self) -> bool {
        if self.extra.is_empty() {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(&self.extra)
            .ok()
            .and_then(|value| value.get("metadata").cloned())
            .map(|metadata| !metadata.is_null())
            .unwrap_or(false)
    }

    /// 服务端是否**明确表示**这条消息没有缩略图。
    ///
    /// 这是写终态 `thumb_status=3` 的唯一许可条件。三件事必须同时成立：
    /// metadata 解析出来了、其中没有 `thumbnail_file_id`(或为 0)、也没有非空的
    /// `thumbnail_url`。
    ///
    /// 反过来最容易错的一种情况:metadata 里**明明有** `thumbnail_file_id`,只是这次
    /// `file/get_url` 因为网络/token/服务端抖动没拿到票据。那是一次可重试的失败,不是
    /// 「这条消息没有缩略图」——把它写成终态,一次网络抖动就能让一张图永久变成灰块。
    pub fn server_says_no_thumbnail(&self) -> bool {
        let Some(metadata) = self.metadata() else {
            // 连 metadata 都没有 = 没有证据,不能下结论。
            return false;
        };
        let has_file_id = metadata
            .get("thumbnail_file_id")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .map(|id| id > 0)
            .unwrap_or(false);
        let has_url = metadata
            .get("thumbnail_url")
            .and_then(|v| v.as_str())
            .map(|u| !u.trim().is_empty())
            .unwrap_or(false);
        !has_file_id && !has_url
    }

    /// 取出 metadata 子对象（若有）。
    pub fn metadata(&self) -> Option<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(&self.extra)
            .ok()
            .and_then(|value| value.get("metadata").cloned())
            .filter(|metadata| !metadata.is_null())
    }

    /// 跨路径比较用的语义投影。
    pub fn semantic(&self) -> SemanticProjection {
        SemanticProjection {
            server_message_id: self.server_message_id,
            channel_id: self.channel_id,
            channel_type: self.channel_type,
            from_uid: self.from_uid,
            message_type: self.message_type,
            content: self.content.clone(),
            metadata: self.metadata(),
            pts: self.pts,
            sent_at_ms: self.sent_at_ms,
            revoked: self.revoked,
        }
    }

    /// 唯一一条数据库投影。
    ///
    /// `status` 由调用方给：同一条消息 history 回填是「已确认」，而乐观发送那条在
    /// ACK 之前是 pending——这是真实差异，不属于 canonical model。
    pub fn to_upsert_input(&self, status: i32, mime_type: Option<String>) -> UpsertRemoteMessageInput {
        UpsertRemoteMessageInput {
            server_message_id: self.server_message_id,
            local_message_id: self.local_message_id,
            channel_id: self.channel_id,
            channel_type: if self.channel_type == 0 { 1 } else { self.channel_type },
            timestamp: self.sent_at_ms,
            from_uid: self.from_uid,
            message_type: self.message_type,
            content: self.content.clone(),
            status,
            pts: self.pts,
            setting: 0,
            order_seq: self.pts,
            searchable_word: String::new(),
            extra: self.extra.clone(),
            timestamp_precision: self.sent_at_precision,
            mime_type,
        }
    }
}

// ----- 来源适配器：wire → canonical。每条来源只允许出现在这里一次。 -----

impl CanonicalInboundMessage {
    /// `message/history/get` 与 `message/history/around` 的一条消息。
    ///
    /// 两条路由返回的是同一个 JSON 视图（server `message_view_json`），所以共用一个
    /// 适配器；分开写就是又开了一次分叉的口子。
    pub fn from_history_item(
        item: &privchat_protocol::rpc::message::history::MessageHistoryItem,
        channel_type: i32,
        wire_message_type_to_i32: impl Fn(&str) -> i32,
    ) -> Self {
        Self {
            server_message_id: item.message_id,
            local_message_id: 0,
            channel_id: item.channel_id,
            channel_type,
            from_uid: item.sender_id,
            message_type: wire_message_type_to_i32(&item.message_type),
            content: item.content.clone(),
            extra: build_extra_envelope(
                &item.content,
                item.metadata
                    .as_ref()
                    .map(|m| serde_json::Value::Object(m.clone()))
                    .as_ref(),
            ),
            pts: item.message_seq.unwrap_or(0),
            // server 侧是 created_at.timestamp_millis()，已是毫秒；仍过一次归一，
            // 因为「这条路径是毫秒」属于对端实现，不该是本地的隐含假设。
            sent_at_ms: normalize_sent_at_ms(i64::try_from(item.timestamp).unwrap_or(i64::MAX)),
            sent_at_precision: TimePrecision::Milliseconds,
            revoked: item.revoked,
        }
    }

    /// `sync/get_difference` 的一条 message 实体。
    ///
    /// 字段已由调用方从 sync payload 解出（那层还要处理 deleted/版本回退等 sync
    /// 专属语义），这里只负责归一与 envelope 形状。
    #[allow(clippy::too_many_arguments)]
    pub fn from_sync_entity(
        server_message_id: u64,
        local_message_id: u64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        message_type: i32,
        content: String,
        extra: String,
        pts: i64,
        timestamp: i64,
    ) -> Self {
        Self {
            server_message_id,
            local_message_id,
            channel_id,
            channel_type,
            from_uid,
            message_type,
            content,
            extra,
            pts,
            sent_at_ms: normalize_sent_at_ms(timestamp),
            sent_at_precision: TimePrecision::Milliseconds,
            revoked: false,
        }
    }

    /// realtime push。
    ///
    /// `PushMessageRequest.timestamp` 是 **秒**（protocol/push.fbs 里是 `uint`，u32
    /// 装不下毫秒纪元）。归一在这里做一次，别处不再判断。
    #[allow(clippy::too_many_arguments)]
    pub fn from_push(
        server_message_id: u64,
        local_message_id: u64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        message_type: i32,
        content: String,
        extra: String,
        message_seq: i64,
        timestamp_secs: i64,
    ) -> Self {
        Self {
            server_message_id,
            local_message_id,
            channel_id,
            channel_type,
            from_uid,
            message_type,
            content,
            extra,
            pts: message_seq,
            sent_at_ms: normalize_sent_at_ms(timestamp_secs),
            // push 是**秒**——这一位就是它与其他来源的唯一真实差异。
            sent_at_precision: TimePrecision::Seconds,
            revoked: false,
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// 五条来源，一份共享 fixture，比 semantic projection。
    ///
    /// 这是本轮两个线上事故的门禁：history 丢 metadata、push 秒当毫秒，都是「某条
    /// 来源与其他来源不一致」。比数据库行会假失败（行里的 message.id / status /
    /// 本地媒体状态本来就该不同），比 semantic projection 才是这个问题本身。
    ///
    /// fixture 与 TypeScript 侧是同一个文件，不是两份手抄。
    #[test]
    fn every_source_converges_on_the_same_semantic_projection() {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../privchat-docs/fixtures/canonical-inbound.json"
        ))
        .expect("read shared canonical fixture");
        let fixture: serde_json::Value = serde_json::from_str(&raw).expect("parse fixture");
        let cases = fixture["cases"].as_array().expect("cases");
        assert!(!cases.is_empty(), "fixture 是空的,这个测试就什么都没证明");

        let wire_type = |t: &str| -> i32 {
            match t {
                "voice" => 1,
                "image" => 2,
                "video" => 3,
                "file" => 4,
                "system" => 5,
                _ => 0,
            }
        };

        /// fixture 里 u64 一律是十进制字符串。
        fn u64_of(payload: &serde_json::Value, key: &str) -> u64 {
            payload[key]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| payload[key].as_u64())
                .unwrap_or(0)
        }
        fn i64_of(payload: &serde_json::Value, key: &str) -> i64 {
            payload[key]
                .as_str()
                .and_then(|s| s.parse().ok())
                .or_else(|| payload[key].as_i64())
                .unwrap_or(0)
        }

        let mut checked_paths = 0usize;
        for case in cases {
            let name = case["name"].as_str().unwrap_or("?");
            // fixture 用中立表示（u64 是十进制字符串、message_type 是 word form），
            // 两端各自转成本地表示——JSON 数字装不下 u64，TS 那边一读就截断。
            let e = &case["expected"];
            let expected = SemanticProjection {
                server_message_id: e["server_message_id"].as_str().unwrap().parse().unwrap(),
                channel_id: e["channel_id"].as_str().unwrap().parse().unwrap(),
                channel_type: e["channel_type"].as_i64().unwrap() as i32,
                from_uid: e["from_uid"].as_str().unwrap().parse().unwrap(),
                message_type: wire_type(e["message_type"].as_str().unwrap()),
                content: e["content"].as_str().unwrap_or("").to_string(),
                metadata: match &e["metadata"] {
                    serde_json::Value::Null => None,
                    other => Some(other.clone()),
                },
                pts: e["pts"].as_str().unwrap().parse().unwrap(),
                sent_at_ms: e["sent_at_ms"].as_i64().unwrap(),
                revoked: e["revoked"].as_bool().unwrap_or(false),
            };
            let sources = case["sources"].as_object().expect("sources");

            for (path, payload) in sources {
                let canonical = match path.as_str() {
                    "history" | "around" => {
                        let item = privchat_protocol::rpc::message::history::MessageHistoryItem {
                            message_id: u64_of(payload, "message_id"),
                            channel_id: u64_of(payload, "channel_id"),
                            sender_id: u64_of(payload, "sender_id"),
                            content: payload["content"].as_str().unwrap_or("").to_string(),
                            message_type: payload["message_type"]
                                .as_str()
                                .unwrap_or("text")
                                .to_string(),
                            timestamp: payload["timestamp"].as_u64().unwrap_or(0),
                            message_seq: Some(u64_of(payload, "message_seq") as i64),
                            reply_to_message_id: None,
                            metadata: payload["metadata"].as_object().cloned(),
                            revoked: payload["revoked"].as_bool().unwrap_or(false),
                            revoked_at: None,
                            revoked_by: None,
                        };
                        CanonicalInboundMessage::from_history_item(&item, 1, wire_type)
                    }
                    "push" => CanonicalInboundMessage::from_push(
                        u64_of(payload, "server_message_id"),
                        u64_of(payload, "local_message_id"),
                        u64_of(payload, "channel_id"),
                        payload["channel_type"].as_i64().unwrap_or(1) as i32,
                        u64_of(payload, "from_uid"),
                        payload["message_type"].as_i64().unwrap_or(0) as i32,
                        payload["content"].as_str().unwrap_or("").to_string(),
                        payload["extra"].as_str().unwrap_or("").to_string(),
                        i64_of(payload, "message_seq"),
                        payload["timestamp_secs"].as_i64().unwrap_or(0),
                    ),
                    "sync" => CanonicalInboundMessage::from_sync_entity(
                        u64_of(payload, "server_message_id"),
                        u64_of(payload, "local_message_id"),
                        u64_of(payload, "channel_id"),
                        payload["channel_type"].as_i64().unwrap_or(1) as i32,
                        u64_of(payload, "from_uid"),
                        payload["message_type"].as_i64().unwrap_or(0) as i32,
                        payload["content"].as_str().unwrap_or("").to_string(),
                        payload["extra"].as_str().unwrap_or("").to_string(),
                        i64_of(payload, "pts"),
                        payload["timestamp"].as_i64().unwrap_or(0),
                    ),
                    other if other.starts_with('$') => continue,
                    other => panic!("{name}: fixture 里有未知来源 {other}"),
                };

                let actual = canonical.semantic();
                assert!(
                    actual.agrees_with(&expected),
                    "{name}: {path} 这条来源投影出来和其他来源不一致\n  actual   = {actual:?}\n  expected = {expected:?}"
                );
                checked_paths += 1;
            }
        }
        // 四条投影来源都必须在 fixture 里出现过,否则「全都一致」可能只是没测到。
        // (send ACK 不是投影路径,见本文件头部。)
        assert!(
            checked_paths >= 9,
            "只比对了 {checked_paths} 条来源投影,fixture 覆盖不足"
        );
    }

    #[test]
    fn seconds_and_milliseconds_normalize_to_the_same_instant() {
        let ms = 1_785_148_271_317i64;
        assert_eq!(normalize_sent_at_ms(ms), ms);
        // push 给的是秒；归一后必须落在同一秒，而不是 1970。
        assert_eq!(normalize_sent_at_ms(ms / 1000), (ms / 1000) * 1000);
        assert!(normalize_sent_at_ms(ms / 1000) > MIN_PLAUSIBLE_MS);
        assert_eq!(normalize_sent_at_ms(0), 0);
    }

    /// 秒精度来源不得把已有的毫秒值改粗。
    #[test]
    fn a_second_resolution_source_never_degrades_a_millisecond_value() {
        let precise = 1_785_148_271_317i64;
        let coarse = 1_785_148_271_000i64;
        // 同一秒:保留毫秒。
        assert_eq!(prefer_precise_sent_at(precise, coarse), precise);
        assert_eq!(prefer_precise_sent_at(coarse, precise), precise);
        // 跨秒:这是真的更新,以新值为准。
        let next_second = 1_785_148_272_000i64;
        assert_eq!(prefer_precise_sent_at(precise, next_second), next_second);
        // 本地没有值:直接用新值。
        assert_eq!(prefer_precise_sent_at(0, coarse), coarse);
    }

    /// 终态 `thumb_status=3` 的许可条件。
    ///
    /// 关键是第三种情况:metadata 里有 thumbnail_file_id,只是这次没拿到下载票据。
    /// 那是可重试失败,不是「没有缩略图」;写成终态的话,一次网络抖动就永久毁掉一张图。
    #[test]
    fn only_an_explicit_absence_may_become_terminal() {
        let mk = |extra: String| CanonicalInboundMessage {
            server_message_id: 1,
            local_message_id: 0,
            channel_id: 45,
            channel_type: 1,
            from_uid: 9,
            message_type: 2,
            content: "[图片]".into(),
            extra,
            pts: 1,
            sent_at_ms: 1_785_148_271_317,
            sent_at_precision: TimePrecision::Milliseconds,
            revoked: false,
        };

        // 服务端明确说了没有缩略图:可以进终态。
        assert!(mk(build_extra_envelope(
            "[图片]",
            Some(&serde_json::json!({ "file_id": 7120 }))
        ))
        .server_says_no_thumbnail());

        // metadata 里有 thumbnail_file_id —— 票据这次没拿到只是暂时失败。
        assert!(!mk(build_extra_envelope(
            "[图片]",
            Some(&serde_json::json!({ "thumbnail_file_id": 7119 }))
        ))
        .server_says_no_thumbnail());
        // legacy 明文 url 同理。
        assert!(!mk(build_extra_envelope(
            "[图片]",
            Some(&serde_json::json!({ "thumbnail_url": "https://example.invalid/a.webp" }))
        ))
        .server_says_no_thumbnail());
        // thumbnail_file_id=0 等于没有。
        assert!(mk(build_extra_envelope(
            "[图片]",
            Some(&serde_json::json!({ "thumbnail_file_id": 0 }))
        ))
        .server_says_no_thumbnail());

        // 没有 metadata = 没有证据,一律不许进终态。
        assert!(!mk(String::new()).server_says_no_thumbnail());
        assert!(!mk("not json".into()).server_says_no_thumbnail());
    }

    #[test]
    fn missing_metadata_is_distinguishable_from_absent_metadata() {
        let with = CanonicalInboundMessage {
            server_message_id: 1,
            local_message_id: 0,
            channel_id: 45,
            channel_type: 1,
            from_uid: 9,
            message_type: 2,
            content: "[图片]".into(),
            extra: build_extra_envelope(
                "[图片]",
                Some(&serde_json::json!({ "thumbnail_file_id": 7119 })),
            ),
            pts: 14,
            sent_at_ms: 1_785_148_271_317,
            sent_at_precision: TimePrecision::Milliseconds,
            revoked: false,
        };
        let without = CanonicalInboundMessage {
            extra: build_extra_envelope("[图片]", None),
            ..with.clone()
        };

        assert!(with.has_metadata());
        // 这一条是「没有证据」，不是「证明没有」——终态判定必须在这里止步。
        assert!(!without.has_metadata());
        assert_eq!(
            with.metadata(),
            Some(serde_json::json!({ "thumbnail_file_id": 7119 }))
        );
        assert_eq!(without.metadata(), None);
    }
}
