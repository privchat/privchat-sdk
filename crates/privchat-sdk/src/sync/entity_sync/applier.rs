//! 同步结果应用器 - 将 SyncEntityItem 写入本地表（ENTITY_SYNC_V1 SyncApplier）

use crate::error::{PrivchatSDKError, Result};
use crate::storage::{StorageManager, entities};
use privchat_protocol::rpc::sync::SyncEntityItem;
use super::EntityType;
use tracing::debug;

/// 将单条同步项应用到本地 DB
pub async fn apply(
    storage: &StorageManager,
    entity_type: EntityType,
    _scope: Option<&str>,
    item: &SyncEntityItem,
) -> Result<()> {
    // UserSettings 的 entity_id 为 setting_key（字符串），不解析为 u64
    if entity_type == EntityType::UserSettings {
        if item.deleted {
            if let Some(kv) = storage.kv_store().await {
                let key = format!("entity_sync:user_settings:{}", item.entity_id);
                let _ = kv.delete(key.as_bytes()).await;
            }
        } else {
            let payload = item
                .payload
                .as_ref()
                .ok_or_else(|| PrivchatSDKError::Other("user_settings 非 tombstone 时应有 payload".to_string()))?;
            apply_user_settings(storage, &item.entity_id, payload).await?;
        }
        return Ok(());
    }

    let entity_id = item
        .entity_id
        .parse::<u64>()
        .map_err(|_| PrivchatSDKError::Other(format!("无效 entity_id: {}", item.entity_id)))?;

    if item.deleted {
        return apply_deleted(storage, entity_type, _scope, entity_id).await;
    }

    let payload = item
        .payload
        .as_ref()
        .ok_or_else(|| PrivchatSDKError::Other("tombstone 外 deleted=false 时应有 payload".to_string()))?;

    match entity_type {
        EntityType::Friend => apply_friend(storage, entity_id, payload).await,
        EntityType::User => apply_user(storage, entity_id, payload).await,
        EntityType::Group => apply_group(storage, entity_id, payload).await,
        EntityType::Channel => apply_channel(storage, entity_id, payload).await,
        EntityType::GroupMember => apply_group_member(storage, _scope, entity_id, payload).await,
        EntityType::UserSettings => unreachable!("UserSettings 已在上方单独处理"),
        EntityType::UserBlock => apply_user_block(storage, entity_id, payload).await,
    }
}

async fn apply_deleted(
    storage: &StorageManager,
    entity_type: EntityType,
    scope: Option<&str>,
    entity_id: u64,
) -> Result<()> {
    match entity_type {
        EntityType::Friend => {
            storage.delete_friend(entity_id).await?;
        }
        EntityType::Group => {
            // 群解散：本地标记 is_dismissed，若本地无该群则忽略
            if let Ok(Some(mut g)) = storage.get_group(entity_id).await {
                g.is_dismissed = true;
                if storage.save_groups(vec![g]).await.is_ok() {
                    debug!("entity_sync applier: group tombstone entity_id={} 已标记 is_dismissed", entity_id);
                }
            }
        }
        EntityType::Channel => {
            // 频道删除：需 channel_type，当前 tombstone 无携带；仅打日志，后续可扩展
            debug!("entity_sync applier: channel tombstone entity_id={}", entity_id);
        }
        EntityType::GroupMember => {
            // scope 为逻辑范围（group_id），分页 cursor 形如 "cursor:xxx" 不作为 group_id
            let group_id = scope
                .filter(|s| !s.starts_with("cursor:"))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            if group_id != 0 {
                storage.delete_channel_member(group_id, 2, entity_id).await?;
            }
        }
        EntityType::User | EntityType::UserSettings | EntityType::UserBlock => {
            debug!("entity_sync applier: {} tombstone entity_id={}", entity_type.as_str(), entity_id);
        }
    }
    Ok(())
}

async fn apply_friend(
    storage: &StorageManager,
    entity_id: u64,
    payload: &serde_json::Value,
) -> Result<()> {
    let user = parse_user_from_payload(entity_id, payload.get("user"))?;
    let friend = parse_friend_from_payload(entity_id, payload.get("friend"))?;
    storage.save_users(vec![user]).await?;
    storage.save_friends(vec![friend]).await?;
    Ok(())
}

async fn apply_user(
    storage: &StorageManager,
    entity_id: u64,
    payload: &serde_json::Value,
) -> Result<()> {
    let user = parse_user_from_payload(entity_id, Some(payload))?;
    storage.save_users(vec![user]).await?;
    Ok(())
}

async fn apply_group(
    storage: &StorageManager,
    group_id: u64,
    payload: &serde_json::Value,
) -> Result<()> {
    let group = parse_group_from_payload(group_id, payload)?;
    storage.save_groups(vec![group]).await?;
    Ok(())
}

fn parse_group_from_payload(group_id: u64, payload: &serde_json::Value) -> Result<entities::Group> {
    let now = chrono::Utc::now().timestamp_millis();
    let created_at = payload
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(now);
    let updated_at = payload
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(now);
    Ok(entities::Group {
        group_id,
        name: payload.get("name").and_then(|x| x.as_str()).map(String::from),
        avatar: payload
            .get("avatar_url")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        owner_id: payload.get("owner_id").and_then(|x| x.as_u64()).or(Some(0)),
        is_dismissed: false,
        created_at,
        updated_at,
    })
}

fn parse_user_from_payload(
    user_id: u64,
    v: Option<&serde_json::Value>,
) -> Result<entities::User> {
    let v = v.ok_or_else(|| PrivchatSDKError::Other("缺少 user 字段".to_string()))?;
    let now = chrono::Utc::now().timestamp_millis();
    Ok(entities::User {
        user_id,
        username: v.get("username").and_then(|x| x.as_str()).map(String::from),
        nickname: v.get("nickname").and_then(|x| x.as_str()).map(String::from),
        alias: None,
        avatar: v.get("avatar").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        user_type: v.get("user_type").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
        is_deleted: false,
        channel_id: String::new(),
        updated_at: now,
    })
}

fn parse_friend_from_payload(
    user_id: u64,
    v: Option<&serde_json::Value>,
) -> Result<entities::Friend> {
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let v = v.unwrap_or(&empty);
    let now = chrono::Utc::now().timestamp_millis();
    Ok(entities::Friend {
        user_id,
        tags: v.get("tags").and_then(|x| x.as_str()).map(String::from),
        is_pinned: v.get("is_pinned").and_then(|x| x.as_bool()).unwrap_or(false),
        created_at: v
            .get("created_at")
            .and_then(|x| x.as_i64())
            .unwrap_or(now),
        updated_at: now,
    })
}

async fn apply_channel(
    storage: &StorageManager,
    channel_id: u64,
    payload: &serde_json::Value,
) -> Result<()> {
    let ch = parse_channel_from_payload(channel_id, payload)?;
    storage.save_channel(&ch).await?;
    Ok(())
}

fn parse_channel_from_payload(channel_id: u64, payload: &serde_json::Value) -> Result<entities::Channel> {
    let now = chrono::Utc::now().timestamp_millis();
    let channel_type = payload.get("channel_type").or(payload.get("type")).and_then(|x| x.as_i64()).unwrap_or(1) as i32;
    let channel_name = payload.get("channel_name").or(payload.get("name")).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let avatar = payload.get("avatar").and_then(|x| x.as_str()).unwrap_or("").to_string();
    Ok(entities::Channel {
        id: None,
        channel_id,
        channel_type,
        last_local_message_id: 0,
        last_msg_timestamp: None,
        unread_count: payload.get("unread_count").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
        last_msg_pts: 0,
        show_nick: 0,
        username: String::new(),
        channel_name,
        channel_remark: String::new(),
        top: 0,
        mute: 0,
        save: 0,
        forbidden: 0,
        follow: 0,
        is_deleted: 0,
        receipt: 0,
        status: 0,
        invite: 0,
        robot: 0,
        version: 0,
        online: 0,
        last_offline: 0,
        avatar,
        category: String::new(),
        extra: String::new(),
        created_at: now,
        updated_at: now,
        avatar_cache_key: String::new(),
        remote_extra: None,
        flame: 0,
        flame_second: 0,
        device_flag: 0,
        parent_channel_id: 0,
        parent_channel_type: 0,
    })
}

async fn apply_group_member(
    storage: &StorageManager,
    scope: Option<&str>,
    user_id: u64,
    payload: &serde_json::Value,
) -> Result<()> {
    let group_id = scope
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| PrivchatSDKError::Other("group_member 同步缺少 scope (group_id)".to_string()))?;
    let member = parse_channel_member_from_payload(group_id, 2, user_id, payload)?;
    storage.save_channel_member(&member).await?;
    Ok(())
}

fn parse_channel_member_from_payload(
    channel_id: u64,
    channel_type: i32,
    member_uid: u64,
    payload: &serde_json::Value,
) -> Result<entities::ChannelMember> {
    let now = chrono::Utc::now().timestamp_millis();
    Ok(entities::ChannelMember {
        id: None,
        channel_id,
        channel_type,
        member_uid,
        member_name: payload.get("member_name").or(payload.get("nickname")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        member_remark: String::new(),
        member_avatar: payload.get("member_avatar").or(payload.get("avatar")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        member_invite_uid: 0,
        role: payload.get("role").and_then(|x| x.as_i64()).unwrap_or(2) as i32,
        status: payload.get("status").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
        is_deleted: 0,
        robot: 0,
        version: 0,
        created_at: now,
        updated_at: now,
        extra: String::new(),
        forbidden_expiration_time: 0,
        member_avatar_cache_key: String::new(),
    })
}

/// 将单条 user_settings 项写入 KV，entity_id 即 setting_key（如 "theme", "notification_enabled"）
async fn apply_user_settings(
    storage: &StorageManager,
    setting_key: &str,
    payload: &serde_json::Value,
) -> Result<()> {
    if let Some(kv) = storage.kv_store().await {
        let key = format!("entity_sync:user_settings:{}", setting_key);
        kv.set(key.as_str(), payload).await?;
    }
    Ok(())
}

async fn apply_user_block(
    _storage: &StorageManager,
    _entity_id: u64,
    _payload: &serde_json::Value,
) -> Result<()> {
    debug!("entity_sync applier: user_block 落库暂用 KV 或后续扩展");
    Ok(())
}
