#![allow(clippy::new_without_default)]

use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::{
    AccountSearchQueryRequest, AccountSearchResponse, BlacklistAddRequest, BlacklistAddResponse,
    BlacklistCheckRequest, BlacklistCheckResponse, BlacklistListRequest, BlacklistListResponse,
    BlacklistRemoveRequest, BlacklistRemoveResponse, FriendAcceptRequest, FriendAcceptResponse,
    FriendApplyRequest, FriendApplyResponse, FriendCheckRequest, FriendCheckResponse,
    FriendPendingRequest, FriendPendingResponse, FriendRejectRequest, FriendRejectResponse,
    FriendRemoveRequest, FriendRemoveResponse, GetOrCreateDirectChannelRequest,
    GetOrCreateDirectChannelResponse, GroupCreateRequest, GroupCreateResponse, GroupInfoRequest, GroupInfoResponse,
    GroupMemberListRequest, GroupMemberListResponse, MessageStatusReadRequest, MessageStatusReadResponse,
    MessageRevokeRequest, MessageRevokeResponse, MessageReactionAddRequest, MessageReactionAddResponse,
    MessageReactionRemoveRequest, MessageReactionRemoveResponse, MessageReactionListRequest,
    MessageReactionListResponse, MessageReactionStatsRequest, MessageReactionStatsResponse,
    ChannelPinRequest, ChannelPinResponse, ChannelHideRequest, ChannelHideResponse, ChannelMuteRequest,
    ChannelMuteResponse, DevicePushUpdateRequest, DevicePushUpdateResponse, DevicePushStatusRequest,
    DevicePushStatusResponse, MessageHistoryGetRequest, MessageHistoryResponse, MessageReadListRequest,
    MessageReadListResponse, MessageReadStatsRequest, MessageReadStatsResponse, MessageStatusCountRequest,
    MessageStatusCountResponse,
    AccountPrivacyGetRequest, AccountPrivacyGetResponse, AccountPrivacyUpdateRequest, AccountPrivacyUpdateResponse,
    QRCodeGenerateRequest, QRCodeGenerateResponse, QRCodeResolveRequest, QRCodeResolveResponse, QRCodeListRequest,
    UserQRCodeGetRequest, UserQRCodeGenerateRequest, UserQRCodeRefreshRequest, QRCodeRefreshRequest,
    QRCodeRevokeRequest, AccountSearchByQRCodeRequest, AccountUserDetailRequest, AccountUserDetailResponse,
    AccountUserShareCardRequest, AccountUserShareCardResponse, AccountUserUpdateRequest,
    AccountUserUpdateResponse, GroupMemberAddRequest, GroupMemberAddResponse, GroupMemberRemoveRequest,
    GroupMemberRemoveResponse, GroupMemberLeaveRequest, GroupMemberLeaveResponse, GroupMemberMuteRequest,
    GroupMemberMuteResponse, GroupMemberUnmuteRequest, GroupMemberUnmuteResponse, GroupTransferOwnerRequest,
    GroupTransferOwnerResponse, GroupRoleSetRequest, GroupRoleSetResponse, GroupSettingsGetRequest,
    GroupSettingsGetResponse, GroupSettingsUpdateRequest, GroupSettingsUpdateResponse, GroupMuteAllRequest,
    GroupApprovalListRequest, GroupApprovalListResponse, GroupApprovalHandleRequest, GroupApprovalHandleResponse,
    GroupQRCodeGenerateRequest, GroupQRCodeGenerateResponse, GroupQRCodeJoinRequest, GroupQRCodeJoinResponse,
    ClientSubmitRequest, ClientSubmitResponse, SyncEntitiesRequest, SyncEntitiesResponse,
    GetDifferenceRequest, GetDifferenceResponse, GetChannelPtsRequest, GetChannelPtsResponse,
    BatchGetChannelPtsRequest, BatchGetChannelPtsResponse, FileRequestUploadTokenRequest,
    FileRequestUploadTokenResponse, FileUploadCallbackRequest, FileUploadCallbackResponse,
    ChannelBroadcastSubscribeRequest, ChannelBroadcastSubscribeResponse,
    AuthLogoutRequest, AuthLogoutResponse, AuthRefreshRequest, AuthRefreshResponse,
    AccountProfileGetRequest, AccountProfileGetResponse, AccountProfileUpdateRequest,
    AccountProfileUpdateResponse,
    ChannelBroadcastCreateRequest, ChannelBroadcastCreateResponse, ChannelBroadcastListRequest,
    ChannelBroadcastListResponse, ChannelContentPublishRequest, ChannelContentPublishResponse,
    ChannelContentListRequest, ChannelContentListResponse, StickerPackageListRequest,
    StickerPackageListResponse, StickerPackageDetailRequest, StickerPackageDetailResponse,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast::error::RecvError, Mutex as AsyncMutex};
use privchat_sdk::{
    ConnectionState as SdkConnectionState, Error as SdkError, FileQueueRef as SdkFileQueueRef,
    LoginResult as SdkLoginResult, MentionInput as SdkMentionInput, NewMessage as SdkNewMessage,
    PresenceStatus as SdkPresenceStatus, PrivchatConfig as SdkConfig, PrivchatSdk as InnerSdk,
    QueueMessage as SdkQueueMessage, ServerEndpoint as SdkServerEndpoint,
    SequencedSdkEvent as SdkSequencedSdkEvent,
    SessionSnapshot as SdkSessionSnapshot, StoredChannel as SdkStoredChannel,
    StoredChannelExtra as SdkStoredChannelExtra, StoredChannelMember as SdkStoredChannelMember,
    StoredBlacklistEntry as SdkStoredBlacklistEntry, StoredFriend as SdkStoredFriend, StoredGroup as SdkStoredGroup,
    StoredGroupMember as SdkStoredGroupMember, StoredMessage as SdkStoredMessage,
    StoredMessageExtra as SdkStoredMessageExtra, StoredMessageReaction as SdkStoredMessageReaction,
    StoredReminder as SdkStoredReminder, StoredUser as SdkStoredUser,
    TransportProtocol as SdkProtocol, TypingActionType as SdkTypingActionType,
    UnreadMentionCount as SdkUnreadMentionCount,
    UpsertBlacklistInput as SdkUpsertBlacklistInput,
    UpsertChannelExtraInput as SdkUpsertChannelExtraInput,
    UpsertChannelInput as SdkUpsertChannelInput,
    UpsertChannelMemberInput as SdkUpsertChannelMemberInput,
    UpsertFriendInput as SdkUpsertFriendInput, UpsertGroupInput as SdkUpsertGroupInput,
    UpsertGroupMemberInput as SdkUpsertGroupMemberInput,
    UpsertMessageReactionInput as SdkUpsertMessageReactionInput,
    UpsertReminderInput as SdkUpsertReminderInput, UpsertUserInput as SdkUpsertUserInput,
    UserStoragePaths as SdkUserStoragePaths,
};

const USER_SETTINGS_KEY: &str = "__user_settings_json__";

fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}

fn channel_prefs_key(channel_id: u64, channel_type: i32) -> String {
    format!("__channel_prefs__:{channel_id}:{channel_type}")
}

fn group_settings_key(group_id: u64) -> String {
    format!("__group_settings__:{group_id}")
}

fn json_encode<T: Serialize>(value: &T, what: &str) -> Result<String, PrivchatFfiError> {
    serde_json::to_string(value).map_err(|e| PrivchatFfiError::SdkError {
        code: privchat_protocol::ErrorCode::InvalidJson as u32,
        detail: format!("serialize {what} failed: {e}"),
    })
}

fn json_decode<T: DeserializeOwned>(value: &str, what: &str) -> Result<T, PrivchatFfiError> {
    serde_json::from_str::<T>(value).map_err(|e| PrivchatFfiError::SdkError {
        code: privchat_protocol::ErrorCode::InvalidJson as u32,
        detail: format!("decode {what} failed: {e}"),
    })
}

async fn rpc_call_typed<Req, Resp>(
    sdk: &InnerSdk,
    route: &str,
    request: &Req,
) -> Result<Resp, PrivchatFfiError>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let body_json = json_encode(request, &format!("{route} request"))?;
    let raw = sdk
        .rpc_call(route.to_string(), body_json)
        .await
        .map_err(PrivchatFfiError::from)?;
    json_decode::<Resp>(&raw, &format!("{route} response"))
}

fn parse_group_role_to_code(role: &str) -> i32 {
    match role.to_ascii_lowercase().as_str() {
        "owner" => 2,
        "admin" | "administrator" => 1,
        _ => 0,
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PrivchatFfiError {
    #[error("[{code}] {detail}")]
    SdkError { code: u32, detail: String },
}

impl From<SdkError> for PrivchatFfiError {
    fn from(value: SdkError) -> Self {
        Self::SdkError {
            code: value.protocol_code(),
            detail: value.to_string(),
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum TransportProtocol {
    Quic,
    Tcp,
    WebSocket,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ServerEndpoint {
    pub protocol: TransportProtocol,
    pub host: String,
    pub port: u16,
    pub path: Option<String>,
    pub use_tls: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PrivchatConfig {
    pub endpoints: Vec<ServerEndpoint>,
    pub connection_timeout_secs: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct LoginResult {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub refresh_token: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SessionSnapshot {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub bootstrap_completed: bool,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum ConnectionState {
    New,
    Connected,
    LoggedIn,
    Authenticated,
    Shutdown,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum SdkEvent {
    ConnectionStateChanged {
        from: ConnectionState,
        to: ConnectionState,
    },
    BootstrapCompleted {
        user_id: u64,
    },
    ShutdownStarted,
    ShutdownCompleted,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SequencedSdkEvent {
    pub sequence_id: u64,
    pub timestamp_ms: i64,
    pub event: SdkEvent,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QueueMessage {
    pub message_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileQueueRef {
    pub queue_index: u64,
    pub message_id: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PresenceStatus {
    pub user_id: u64,
    pub status: String,
    pub last_seen: i64,
    pub online_devices: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum TypingActionType {
    Typing,
    Recording,
    UploadingPhoto,
    UploadingVideo,
    UploadingFile,
    ChoosingSticker,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct NewMessage {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub searchable_word: String,
    pub setting: i32,
    pub extra: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredMessage {
    pub message_id: u64,
    pub server_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub extra: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertChannelInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub channel_name: String,
    pub channel_remark: String,
    pub avatar: String,
    pub unread_count: i32,
    pub top: i32,
    pub mute: i32,
    pub last_msg_timestamp: i64,
    pub last_local_message_id: u64,
    pub last_msg_content: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredChannel {
    pub channel_id: u64,
    pub channel_type: i32,
    pub channel_name: String,
    pub channel_remark: String,
    pub avatar: String,
    pub unread_count: i32,
    pub top: i32,
    pub mute: i32,
    pub last_msg_timestamp: i64,
    pub last_local_message_id: u64,
    pub last_msg_content: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertChannelExtraInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub browse_to: u64,
    pub keep_pts: u64,
    pub keep_offset_y: i32,
    pub draft: String,
    pub draft_updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredChannelExtra {
    pub channel_id: u64,
    pub channel_type: i32,
    pub browse_to: u64,
    pub keep_pts: u64,
    pub keep_offset_y: i32,
    pub draft: String,
    pub draft_updated_at: u64,
    pub version: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredMessageExtra {
    pub message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub readed: i32,
    pub readed_count: i32,
    pub unread_count: i32,
    pub revoke: bool,
    pub revoker: Option<u64>,
    pub extra_version: i64,
    pub is_mutual_deleted: bool,
    pub content_edit: Option<String>,
    pub edited_at: i32,
    pub need_upload: bool,
    pub is_pinned: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertUserInput {
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

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredUser {
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

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertFriendInput {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredFriend {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertBlacklistInput {
    pub blocked_user_id: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredBlacklistEntry {
    pub blocked_user_id: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertGroupInput {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredGroup {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertGroupMemberInput {
    pub group_id: u64,
    pub user_id: u64,
    pub role: i32,
    pub status: i32,
    pub alias: Option<String>,
    pub is_muted: bool,
    pub joined_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredGroupMember {
    pub group_id: u64,
    pub user_id: u64,
    pub role: i32,
    pub status: i32,
    pub alias: Option<String>,
    pub is_muted: bool,
    pub joined_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertChannelMemberInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub member_uid: u64,
    pub member_name: String,
    pub member_remark: String,
    pub member_avatar: String,
    pub member_invite_uid: u64,
    pub role: i32,
    pub status: i32,
    pub is_deleted: bool,
    pub robot: i32,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub extra: String,
    pub forbidden_expiration_time: i64,
    pub member_avatar_cache_key: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredChannelMember {
    pub channel_id: u64,
    pub channel_type: i32,
    pub member_uid: u64,
    pub member_name: String,
    pub member_remark: String,
    pub member_avatar: String,
    pub member_invite_uid: u64,
    pub role: i32,
    pub status: i32,
    pub is_deleted: bool,
    pub robot: i32,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub extra: String,
    pub forbidden_expiration_time: i64,
    pub member_avatar_cache_key: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertMessageReactionInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub uid: u64,
    pub name: String,
    pub emoji: String,
    pub message_id: u64,
    pub seq: i64,
    pub is_deleted: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredMessageReaction {
    pub id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub uid: u64,
    pub name: String,
    pub emoji: String,
    pub message_id: u64,
    pub seq: i64,
    pub is_deleted: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MentionInput {
    pub message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub mentioned_user_id: u64,
    pub sender_id: u64,
    pub is_mention_all: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UnreadMentionCount {
    pub channel_id: u64,
    pub channel_type: i32,
    pub unread_count: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UpsertReminderInput {
    pub reminder_id: u64,
    pub message_id: u64,
    pub pts: i64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub uid: u64,
    pub reminder_type: i32,
    pub text: String,
    pub data: String,
    pub is_locate: bool,
    pub version: i64,
    pub done: bool,
    pub need_upload: bool,
    pub publisher: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredReminder {
    pub id: u64,
    pub reminder_id: u64,
    pub message_id: u64,
    pub pts: i64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub uid: u64,
    pub reminder_type: i32,
    pub text: String,
    pub data: String,
    pub is_locate: bool,
    pub version: i64,
    pub done: bool,
    pub need_upload: bool,
    pub publisher: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UserStoragePaths {
    pub user_root: String,
    pub db_path: String,
    pub kv_path: String,
    pub queue_root: String,
    pub normal_queue_path: String,
    pub file_queue_paths: Vec<String>,
    pub media_root: String,
}

fn map_protocol(p: TransportProtocol) -> SdkProtocol {
    match p {
        TransportProtocol::Quic => SdkProtocol::Quic,
        TransportProtocol::Tcp => SdkProtocol::Tcp,
        TransportProtocol::WebSocket => SdkProtocol::WebSocket,
    }
}

fn map_config(c: PrivchatConfig) -> SdkConfig {
    SdkConfig {
        endpoints: c
            .endpoints
            .into_iter()
            .map(|e| SdkServerEndpoint {
                protocol: map_protocol(e.protocol),
                host: e.host,
                port: e.port,
                path: e.path,
                use_tls: e.use_tls,
            })
            .collect(),
        connection_timeout_secs: c.connection_timeout_secs,
    }
}

fn map_login(r: SdkLoginResult) -> LoginResult {
    LoginResult {
        user_id: r.user_id,
        token: r.token,
        device_id: r.device_id,
        refresh_token: r.refresh_token,
        expires_at: r.expires_at,
    }
}

fn map_session(r: SdkSessionSnapshot) -> SessionSnapshot {
    SessionSnapshot {
        user_id: r.user_id,
        token: r.token,
        device_id: r.device_id,
        bootstrap_completed: r.bootstrap_completed,
    }
}

fn map_connection_state(v: SdkConnectionState) -> ConnectionState {
    match v {
        SdkConnectionState::New => ConnectionState::New,
        SdkConnectionState::Connected => ConnectionState::Connected,
        SdkConnectionState::LoggedIn => ConnectionState::LoggedIn,
        SdkConnectionState::Authenticated => ConnectionState::Authenticated,
        SdkConnectionState::Shutdown => ConnectionState::Shutdown,
    }
}

#[cfg(test)]
fn parse_read_list_entries(raw: &str) -> Vec<serde_json::Value> {
    let json: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    if let Some(arr) = json.as_array() {
        return arr.clone();
    }
    if let Some(arr) = json.get("data").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = json.get("items").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = json
        .get("result")
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_array())
    {
        return arr.clone();
    }
    vec![]
}

#[cfg(test)]
fn parse_read_list_user_ids(raw: &str) -> Vec<u64> {
    parse_read_list_entries(raw)
        .into_iter()
        .filter_map(|entry| {
            entry
                .get("user_id")
                .or_else(|| entry.get("uid"))
                .and_then(|v| v.as_u64())
        })
        .collect()
}

fn parse_profile_user(raw: &str) -> Option<SdkUpsertUserInput> {
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;
    let data = json.get("data").unwrap_or(&json);
    let user = data.get("user").unwrap_or(data);

    let user_id = user
        .get("user_id")
        .and_then(|v| v.as_u64())
        .or_else(|| user.get("uid").and_then(|v| v.as_u64()))?;

    let username = user
        .get("username")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let nickname = user
        .get("nickname")
        .or_else(|| user.get("name"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let alias = user
        .get("alias")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let avatar = user
        .get("avatar")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let user_type = user
        .get("user_type")
        .or_else(|| user.get("type"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let channel_id = user
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let updated_at = user
        .get("updated_at")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(now_millis);

    Some(SdkUpsertUserInput {
        user_id,
        username,
        nickname,
        alias,
        avatar,
        user_type,
        is_deleted: false,
        channel_id,
        updated_at,
    })
}


fn map_sdk_event(v: privchat_sdk::SdkEvent) -> SdkEvent {
    match v {
        privchat_sdk::SdkEvent::ConnectionStateChanged { from, to } => {
            SdkEvent::ConnectionStateChanged {
                from: map_connection_state(from),
                to: map_connection_state(to),
            }
        }
        privchat_sdk::SdkEvent::BootstrapCompleted { user_id } => {
            SdkEvent::BootstrapCompleted { user_id }
        }
        privchat_sdk::SdkEvent::ShutdownStarted => SdkEvent::ShutdownStarted,
        privchat_sdk::SdkEvent::ShutdownCompleted => SdkEvent::ShutdownCompleted,
    }
}

fn map_sequenced_sdk_event(v: SdkSequencedSdkEvent) -> SequencedSdkEvent {
    SequencedSdkEvent {
        sequence_id: v.sequence_id,
        timestamp_ms: v.timestamp_ms,
        event: map_sdk_event(v.event),
    }
}

fn map_queue_message(r: SdkQueueMessage) -> QueueMessage {
    QueueMessage {
        message_id: r.message_id,
        payload: r.payload,
    }
}

fn map_file_queue_ref(r: SdkFileQueueRef) -> FileQueueRef {
    FileQueueRef {
        queue_index: r.queue_index as u64,
        message_id: r.message_id,
    }
}

fn map_presence_status(v: SdkPresenceStatus) -> PresenceStatus {
    PresenceStatus {
        user_id: v.user_id,
        status: v.status,
        last_seen: v.last_seen,
        online_devices: v.online_devices,
    }
}

fn map_typing_action(v: TypingActionType) -> SdkTypingActionType {
    match v {
        TypingActionType::Typing => SdkTypingActionType::Typing,
        TypingActionType::Recording => SdkTypingActionType::Recording,
        TypingActionType::UploadingPhoto => SdkTypingActionType::UploadingPhoto,
        TypingActionType::UploadingVideo => SdkTypingActionType::UploadingVideo,
        TypingActionType::UploadingFile => SdkTypingActionType::UploadingFile,
        TypingActionType::ChoosingSticker => SdkTypingActionType::ChoosingSticker,
    }
}

fn map_new_message(v: NewMessage) -> SdkNewMessage {
    SdkNewMessage {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        message_type: v.message_type,
        content: v.content,
        searchable_word: v.searchable_word,
        setting: v.setting,
        extra: v.extra,
    }
}

fn map_upsert_channel(v: UpsertChannelInput) -> SdkUpsertChannelInput {
    SdkUpsertChannelInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        channel_name: v.channel_name,
        channel_remark: v.channel_remark,
        avatar: v.avatar,
        unread_count: v.unread_count,
        top: v.top,
        mute: v.mute,
        last_msg_timestamp: v.last_msg_timestamp,
        last_local_message_id: v.last_local_message_id,
        last_msg_content: v.last_msg_content,
    }
}

fn map_upsert_channel_extra(v: UpsertChannelExtraInput) -> SdkUpsertChannelExtraInput {
    SdkUpsertChannelExtraInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        browse_to: v.browse_to,
        keep_pts: v.keep_pts,
        keep_offset_y: v.keep_offset_y,
        draft: v.draft,
        draft_updated_at: v.draft_updated_at,
    }
}

fn map_stored_message(v: SdkStoredMessage) -> StoredMessage {
    StoredMessage {
        message_id: v.message_id,
        server_message_id: v.server_message_id,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        message_type: v.message_type,
        content: v.content,
        status: v.status,
        created_at: v.created_at,
        updated_at: v.updated_at,
        extra: v.extra,
    }
}

fn map_stored_channel(v: SdkStoredChannel) -> StoredChannel {
    StoredChannel {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        channel_name: v.channel_name,
        channel_remark: v.channel_remark,
        avatar: v.avatar,
        unread_count: v.unread_count,
        top: v.top,
        mute: v.mute,
        last_msg_timestamp: v.last_msg_timestamp,
        last_local_message_id: v.last_local_message_id,
        last_msg_content: v.last_msg_content,
        updated_at: v.updated_at,
    }
}

fn map_stored_channel_extra(v: SdkStoredChannelExtra) -> StoredChannelExtra {
    StoredChannelExtra {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        browse_to: v.browse_to,
        keep_pts: v.keep_pts,
        keep_offset_y: v.keep_offset_y,
        draft: v.draft,
        draft_updated_at: v.draft_updated_at,
        version: v.version,
    }
}

fn map_stored_message_extra(v: SdkStoredMessageExtra) -> StoredMessageExtra {
    StoredMessageExtra {
        message_id: v.message_id,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        readed: v.readed,
        readed_count: v.readed_count,
        unread_count: v.unread_count,
        revoke: v.revoke,
        revoker: v.revoker,
        extra_version: v.extra_version,
        is_mutual_deleted: v.is_mutual_deleted,
        content_edit: v.content_edit,
        edited_at: v.edited_at,
        need_upload: v.need_upload,
        is_pinned: v.is_pinned,
    }
}

fn map_upsert_user(v: UpsertUserInput) -> SdkUpsertUserInput {
    SdkUpsertUserInput {
        user_id: v.user_id,
        username: v.username,
        nickname: v.nickname,
        alias: v.alias,
        avatar: v.avatar,
        user_type: v.user_type,
        is_deleted: v.is_deleted,
        channel_id: v.channel_id,
        updated_at: v.updated_at,
    }
}

fn map_stored_user(v: SdkStoredUser) -> StoredUser {
    StoredUser {
        user_id: v.user_id,
        username: v.username,
        nickname: v.nickname,
        alias: v.alias,
        avatar: v.avatar,
        user_type: v.user_type,
        is_deleted: v.is_deleted,
        channel_id: v.channel_id,
        updated_at: v.updated_at,
    }
}

fn map_upsert_friend(v: UpsertFriendInput) -> SdkUpsertFriendInput {
    SdkUpsertFriendInput {
        user_id: v.user_id,
        tags: v.tags,
        is_pinned: v.is_pinned,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_stored_friend(v: SdkStoredFriend) -> StoredFriend {
    StoredFriend {
        user_id: v.user_id,
        tags: v.tags,
        is_pinned: v.is_pinned,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_upsert_blacklist(v: UpsertBlacklistInput) -> SdkUpsertBlacklistInput {
    SdkUpsertBlacklistInput {
        blocked_user_id: v.blocked_user_id,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_stored_blacklist(v: SdkStoredBlacklistEntry) -> StoredBlacklistEntry {
    StoredBlacklistEntry {
        blocked_user_id: v.blocked_user_id,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_upsert_group(v: UpsertGroupInput) -> SdkUpsertGroupInput {
    SdkUpsertGroupInput {
        group_id: v.group_id,
        name: v.name,
        avatar: v.avatar,
        owner_id: v.owner_id,
        is_dismissed: v.is_dismissed,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_stored_group(v: SdkStoredGroup) -> StoredGroup {
    StoredGroup {
        group_id: v.group_id,
        name: v.name,
        avatar: v.avatar,
        owner_id: v.owner_id,
        is_dismissed: v.is_dismissed,
        created_at: v.created_at,
        updated_at: v.updated_at,
    }
}

fn map_upsert_group_member(v: UpsertGroupMemberInput) -> SdkUpsertGroupMemberInput {
    SdkUpsertGroupMemberInput {
        group_id: v.group_id,
        user_id: v.user_id,
        role: v.role,
        status: v.status,
        alias: v.alias,
        is_muted: v.is_muted,
        joined_at: v.joined_at,
        updated_at: v.updated_at,
    }
}

fn map_stored_group_member(v: SdkStoredGroupMember) -> StoredGroupMember {
    StoredGroupMember {
        group_id: v.group_id,
        user_id: v.user_id,
        role: v.role,
        status: v.status,
        alias: v.alias,
        is_muted: v.is_muted,
        joined_at: v.joined_at,
        updated_at: v.updated_at,
    }
}

fn map_upsert_channel_member(v: UpsertChannelMemberInput) -> SdkUpsertChannelMemberInput {
    SdkUpsertChannelMemberInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        member_uid: v.member_uid,
        member_name: v.member_name,
        member_remark: v.member_remark,
        member_avatar: v.member_avatar,
        member_invite_uid: v.member_invite_uid,
        role: v.role,
        status: v.status,
        is_deleted: v.is_deleted,
        robot: v.robot,
        version: v.version,
        created_at: v.created_at,
        updated_at: v.updated_at,
        extra: v.extra,
        forbidden_expiration_time: v.forbidden_expiration_time,
        member_avatar_cache_key: v.member_avatar_cache_key,
    }
}

fn map_stored_channel_member(v: SdkStoredChannelMember) -> StoredChannelMember {
    StoredChannelMember {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        member_uid: v.member_uid,
        member_name: v.member_name,
        member_remark: v.member_remark,
        member_avatar: v.member_avatar,
        member_invite_uid: v.member_invite_uid,
        role: v.role,
        status: v.status,
        is_deleted: v.is_deleted,
        robot: v.robot,
        version: v.version,
        created_at: v.created_at,
        updated_at: v.updated_at,
        extra: v.extra,
        forbidden_expiration_time: v.forbidden_expiration_time,
        member_avatar_cache_key: v.member_avatar_cache_key,
    }
}

fn map_upsert_message_reaction(v: UpsertMessageReactionInput) -> SdkUpsertMessageReactionInput {
    SdkUpsertMessageReactionInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        uid: v.uid,
        name: v.name,
        emoji: v.emoji,
        message_id: v.message_id,
        seq: v.seq,
        is_deleted: v.is_deleted,
        created_at: v.created_at,
    }
}

fn map_stored_message_reaction(v: SdkStoredMessageReaction) -> StoredMessageReaction {
    StoredMessageReaction {
        id: v.id,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        uid: v.uid,
        name: v.name,
        emoji: v.emoji,
        message_id: v.message_id,
        seq: v.seq,
        is_deleted: v.is_deleted,
        created_at: v.created_at,
    }
}

fn map_mention_input(v: MentionInput) -> SdkMentionInput {
    SdkMentionInput {
        message_id: v.message_id,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        mentioned_user_id: v.mentioned_user_id,
        sender_id: v.sender_id,
        is_mention_all: v.is_mention_all,
        created_at: v.created_at,
    }
}

fn map_unread_mention_count(v: SdkUnreadMentionCount) -> UnreadMentionCount {
    UnreadMentionCount {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        unread_count: v.unread_count,
    }
}

fn map_upsert_reminder(v: UpsertReminderInput) -> SdkUpsertReminderInput {
    SdkUpsertReminderInput {
        reminder_id: v.reminder_id,
        message_id: v.message_id,
        pts: v.pts,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        uid: v.uid,
        reminder_type: v.reminder_type,
        text: v.text,
        data: v.data,
        is_locate: v.is_locate,
        version: v.version,
        done: v.done,
        need_upload: v.need_upload,
        publisher: v.publisher,
    }
}

fn map_stored_reminder(v: SdkStoredReminder) -> StoredReminder {
    StoredReminder {
        id: v.id,
        reminder_id: v.reminder_id,
        message_id: v.message_id,
        pts: v.pts,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        uid: v.uid,
        reminder_type: v.reminder_type,
        text: v.text,
        data: v.data,
        is_locate: v.is_locate,
        version: v.version,
        done: v.done,
        need_upload: v.need_upload,
        publisher: v.publisher,
    }
}

fn map_storage_paths(v: SdkUserStoragePaths) -> UserStoragePaths {
    UserStoragePaths {
        user_root: v.user_root,
        db_path: v.db_path,
        kv_path: v.kv_path,
        queue_root: v.queue_root,
        normal_queue_path: v.normal_queue_path,
        file_queue_paths: v.file_queue_paths,
        media_root: v.media_root,
    }
}

#[derive(uniffi::Object)]
pub struct PrivchatClient {
    inner: InnerSdk,
    event_rx: Arc<AsyncMutex<tokio::sync::broadcast::Receiver<privchat_sdk::SdkEvent>>>,
    config: Arc<StdMutex<PrivchatConfig>>,
    app_in_background: Arc<AtomicBool>,
    typing_active_channels: Arc<AsyncMutex<HashSet<(u64, i32)>>>,
    typing_started_count: Arc<AtomicU64>,
    typing_stopped_count: Arc<AtomicU64>,
    send_queue_enabled: Arc<AtomicBool>,
    disabled_channel_queues: Arc<AsyncMutex<HashSet<(u64, i32)>>>,
    lifecycle_hook_registered: Arc<AtomicBool>,
    transport_disconnect_listener_started: Arc<AtomicBool>,
    on_connection_state_changed_registered: Arc<AtomicBool>,
    on_message_received_registered: Arc<AtomicBool>,
    on_reaction_changed_registered: Arc<AtomicBool>,
    on_typing_indicator_registered: Arc<AtomicBool>,
    video_process_hook_registered: Arc<AtomicBool>,
    event_poll_count: Arc<AtomicU64>,
}

#[uniffi::export]
impl PrivchatClient {
    #[uniffi::constructor]
    pub fn new(config: PrivchatConfig) -> Result<Self, PrivchatFfiError> {
        eprintln!("[FFI] PrivchatClient::new");
        let inner = InnerSdk::new(map_config(config.clone()));
        let event_rx = Arc::new(AsyncMutex::new(inner.subscribe_events()));
        let config = Arc::new(StdMutex::new(config));
        let app_in_background = Arc::new(AtomicBool::new(false));
        let typing_active_channels = Arc::new(AsyncMutex::new(HashSet::new()));
        let typing_started_count = Arc::new(AtomicU64::new(0));
        let typing_stopped_count = Arc::new(AtomicU64::new(0));
        let send_queue_enabled = Arc::new(AtomicBool::new(true));
        let disabled_channel_queues = Arc::new(AsyncMutex::new(HashSet::new()));
        let lifecycle_hook_registered = Arc::new(AtomicBool::new(false));
        let transport_disconnect_listener_started = Arc::new(AtomicBool::new(false));
        let on_connection_state_changed_registered = Arc::new(AtomicBool::new(false));
        let on_message_received_registered = Arc::new(AtomicBool::new(false));
        let on_reaction_changed_registered = Arc::new(AtomicBool::new(false));
        let on_typing_indicator_registered = Arc::new(AtomicBool::new(false));
        let video_process_hook_registered = Arc::new(AtomicBool::new(false));
        let event_poll_count = Arc::new(AtomicU64::new(0));
        Ok(Self {
            inner,
            event_rx,
            config,
            app_in_background,
            typing_active_channels,
            typing_started_count,
            typing_stopped_count,
            send_queue_enabled,
            disabled_channel_queues,
            lifecycle_hook_registered,
            transport_disconnect_listener_started,
            on_connection_state_changed_registered,
            on_message_received_registered,
            on_reaction_changed_registered,
            on_typing_indicator_registered,
            video_process_hook_registered,
            event_poll_count,
        })
    }

    async fn current_user_id(&self) -> Result<u64, PrivchatFfiError> {
        let snapshot = self
            .inner
            .session_snapshot()
            .await
            .map_err(PrivchatFfiError::from)?;
        let snap = snapshot.ok_or_else(|| PrivchatFfiError::SdkError {
            code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
            detail: "session is empty; login/authenticate required".to_string(),
        })?;
        Ok(snap.user_id)
    }

    async fn resolve_channel_type(&self, channel_id: u64) -> i32 {
        match self.get_channel_by_id(channel_id).await {
            Ok(Some(ch)) => ch.channel_type,
            _ => 1,
        }
    }

    async fn resolve_local_message_id_by_server_message_id(
        &self,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) -> Result<Option<u64>, PrivchatFfiError> {
        let items = self.list_messages(channel_id, channel_type, 500, 0).await?;
        Ok(items
            .into_iter()
            .find(|m| m.server_message_id == Some(server_message_id))
            .map(|m| m.message_id))
    }

    async fn resolve_channel_id_by_server_message_id(
        &self,
        server_message_id: u64,
    ) -> Result<u64, PrivchatFfiError> {
        let channels = self.list_channels(1, 200).await?;
        for ch in channels {
            let messages = self.list_messages(ch.channel_id, ch.channel_type, 1, 200).await?;
            if messages
                .iter()
                .any(|m| m.server_message_id == Some(server_message_id))
            {
                return Ok(ch.channel_id);
            }
        }
        Err(PrivchatFfiError::SdkError {
            code: privchat_protocol::ErrorCode::ResourceNotFound as u32,
            detail: format!("channel not found for server_message_id={server_message_id}"),
        })
    }

    pub async fn connect(&self) -> Result<(), PrivchatFfiError> {
        eprintln!("[FFI] connect_async: enter");
        let out = self.inner.connect().await.map_err(PrivchatFfiError::from);
        eprintln!("[FFI] connect_async: done ok={}", out.is_ok());
        out
    }

    pub async fn disconnect(&self) -> Result<(), PrivchatFfiError> {
        eprintln!("[FFI] disconnect_async: enter");
        let out = self
            .inner
            .disconnect()
            .await
            .map_err(PrivchatFfiError::from);
        eprintln!("[FFI] disconnect_async: done ok={}", out.is_ok());
        out
    }

    pub async fn is_connected(&self) -> Result<bool, PrivchatFfiError> {
        self.inner
            .is_connected()
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn connection_state(&self) -> Result<ConnectionState, PrivchatFfiError> {
        let state = self
            .inner
            .connection_state()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(map_connection_state(state))
    }

    pub async fn get_connection_state(&self) -> Result<ConnectionState, PrivchatFfiError> {
        self.connection_state().await
    }

    pub async fn connect_blocking(&self) -> Result<(), PrivchatFfiError> {
        self.connect().await
    }

    pub fn config(&self) -> PrivchatConfig {
        self.config
            .lock()
            .ok()
            .map(|v| v.clone())
            .unwrap_or(PrivchatConfig {
                endpoints: vec![],
                connection_timeout_secs: 30,
            })
    }

    pub fn server_config(&self) -> PrivchatConfig {
        self.config()
    }

    pub fn servers(&self) -> Vec<ServerEndpoint> {
        self.config().endpoints
    }

    pub fn connection_timeout(&self) -> u64 {
        self.config().connection_timeout_secs
    }

    pub fn add_server(&self, endpoint: ServerEndpoint) -> Result<(), PrivchatFfiError> {
        let mut cfg = self
            .config
            .lock()
            .map_err(|_| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: "config lock poisoned".to_string(),
            })?;
        cfg.endpoints.push(endpoint);
        Ok(())
    }

    pub async fn next_event(&self, timeout_ms: u64) -> Result<Option<SdkEvent>, PrivchatFfiError> {
        self.event_poll_count.fetch_add(1, Ordering::Relaxed);
        let mut rx = self.event_rx.lock().await;
        let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(evt)) => Ok(Some(map_sdk_event(evt))),
            Ok(Err(RecvError::Lagged(_))) => Ok(None),
            Ok(Err(RecvError::Closed)) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    pub async fn next_event_envelope(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SequencedSdkEvent>, PrivchatFfiError> {
        self.event_poll_count.fetch_add(1, Ordering::Relaxed);
        let before_seq = self.inner.last_event_sequence_id();
        let mut rx = self.event_rx.lock().await;
        let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Ok(_)) => {
                let replay = self.inner.events_since(before_seq, 1);
                Ok(replay.into_iter().next().map(map_sequenced_sdk_event))
            }
            Ok(Err(RecvError::Lagged(_))) => {
                let replay = self.inner.events_since(before_seq, 1);
                Ok(replay.into_iter().next().map(map_sequenced_sdk_event))
            }
            Ok(Err(RecvError::Closed)) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    pub fn event_stream_cursor(&self) -> u64 {
        self.inner.last_event_sequence_id()
    }

    pub fn recent_events(&self, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .recent_events(cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub fn events_since(&self, sequence_id: u64, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .events_since(sequence_id, cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub async fn ping(&self) -> Result<(), PrivchatFfiError> {
        self.inner.ping().await.map_err(PrivchatFfiError::from)
    }

    pub async fn login(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult, PrivchatFfiError> {
        eprintln!("[FFI] login_async: enter");
        let result = self
            .inner
            .login(username, password, device_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        eprintln!("[FFI] login_async: sdk login ok user_id={}", result.user_id);
        Ok(map_login(result))
    }

    pub async fn register(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult, PrivchatFfiError> {
        eprintln!("[FFI] register_async: enter");
        let result = self
            .inner
            .register(username, password, device_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        eprintln!(
            "[FFI] register_async: sdk register ok user_id={}",
            result.user_id
        );
        Ok(map_login(result))
    }

    pub async fn authenticate(
        &self,
        user_id: u64,
        token: String,
        device_id: String,
    ) -> Result<(), PrivchatFfiError> {
        eprintln!("[FFI] authenticate_async: enter user_id={user_id}");
        let out = self
            .inner
            .authenticate(user_id, token, device_id)
            .await
            .map_err(PrivchatFfiError::from);
        eprintln!("[FFI] authenticate_async: done ok={}", out.is_ok());
        out
    }

    pub async fn run_bootstrap_sync(&self) -> Result<(), PrivchatFfiError> {
        eprintln!("[FFI] run_bootstrap_sync_async: enter");
        let out = self
            .inner
            .run_bootstrap_sync()
            .await
            .map_err(PrivchatFfiError::from);
        eprintln!("[FFI] run_bootstrap_sync_async: done ok={}", out.is_ok());
        out
    }

    pub async fn run_bootstrap_sync_async(&self) -> Result<(), PrivchatFfiError> {
        self.run_bootstrap_sync().await
    }

    pub async fn initialize(&self) -> Result<(), PrivchatFfiError> {
        self.run_bootstrap_sync().await
    }

    pub async fn initialize_blocking(&self) -> Result<(), PrivchatFfiError> {
        self.run_bootstrap_sync().await
    }

    pub async fn run_bootstrap_sync_in_background(&self) -> Result<(), PrivchatFfiError> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _ = inner.run_bootstrap_sync().await;
        });
        Ok(())
    }

    pub async fn is_bootstrap_completed(&self) -> Result<bool, PrivchatFfiError> {
        self.inner
            .is_bootstrap_completed()
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn is_initialized(&self) -> Result<bool, PrivchatFfiError> {
        let state = self.connection_state().await?;
        Ok(!matches!(state, ConnectionState::Shutdown))
    }

    pub async fn is_shutting_down(&self) -> Result<bool, PrivchatFfiError> {
        let state = self.connection_state().await?;
        Ok(matches!(state, ConnectionState::Shutdown))
    }

    pub fn is_supervised_sync_running(&self) -> bool {
        self.inner.is_supervised_sync_running()
    }

    pub fn start_supervised_sync(&self, interval_secs: u64) -> Result<(), PrivchatFfiError> {
        self.inner
            .start_supervised_sync(interval_secs)
            .map_err(PrivchatFfiError::from)
    }

    pub fn stop_supervised_sync(&self) {
        self.inner.stop_supervised_sync()
    }

    pub async fn shutdown(&self) -> Result<(), PrivchatFfiError> {
        eprintln!("[FFI] shutdown_async: enter");
        self.inner.shutdown().await;
        eprintln!("[FFI] shutdown_async: done");
        Ok(())
    }

    pub async fn shutdown_blocking(&self) -> Result<(), PrivchatFfiError> {
        self.shutdown().await
    }

    pub async fn session_snapshot(&self) -> Result<Option<SessionSnapshot>, PrivchatFfiError> {
        let value = self
            .inner
            .session_snapshot()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(value.map(map_session))
    }

    pub async fn user_id(&self) -> Result<Option<u64>, PrivchatFfiError> {
        let snapshot = self.session_snapshot().await?;
        Ok(snapshot.map(|v| v.user_id))
    }

    pub async fn get_connection_summary(&self) -> Result<String, PrivchatFfiError> {
        let state = self.connection_state().await?;
        let snapshot = self.session_snapshot().await?;
        let state_text = match state {
            ConnectionState::New => "new",
            ConnectionState::Connected => "connected",
            ConnectionState::LoggedIn => "logged_in",
            ConnectionState::Authenticated => "authenticated",
            ConnectionState::Shutdown => "shutdown",
        };
        let user_id = snapshot.as_ref().map(|v| v.user_id).unwrap_or(0);
        let bootstrap_completed = snapshot
            .as_ref()
            .map(|v| v.bootstrap_completed)
            .unwrap_or(false);
        let app_in_background = self.app_in_background.load(Ordering::Relaxed);
        let supervised_sync_running = self.is_supervised_sync_running();
        let send_queue_enabled = self.send_queue_enabled.load(Ordering::Relaxed);
        let event_poll_count = self.event_poll_count.load(Ordering::Relaxed);
        let lifecycle_hook_registered = self.lifecycle_hook_registered.load(Ordering::Relaxed);
        let transport_disconnect_listener_started = self
            .transport_disconnect_listener_started
            .load(Ordering::Relaxed);
        let on_connection_state_changed_registered = self
            .on_connection_state_changed_registered
            .load(Ordering::Relaxed);
        let on_message_received_registered = self
            .on_message_received_registered
            .load(Ordering::Relaxed);
        let on_reaction_changed_registered = self
            .on_reaction_changed_registered
            .load(Ordering::Relaxed);
        let on_typing_indicator_registered = self
            .on_typing_indicator_registered
            .load(Ordering::Relaxed);
        let video_process_hook_registered = self
            .video_process_hook_registered
            .load(Ordering::Relaxed);
        Ok(format!(
            "{{\"state\":\"{state_text}\",\"user_id\":{user_id},\"bootstrap_completed\":{bootstrap_completed},\"app_in_background\":{app_in_background},\"supervised_sync_running\":{supervised_sync_running},\"send_queue_enabled\":{send_queue_enabled},\"event_poll_count\":{event_poll_count},\"lifecycle_hook_registered\":{lifecycle_hook_registered},\"transport_disconnect_listener_started\":{transport_disconnect_listener_started},\"on_connection_state_changed_registered\":{on_connection_state_changed_registered},\"on_message_received_registered\":{on_message_received_registered},\"on_reaction_changed_registered\":{on_reaction_changed_registered},\"on_typing_indicator_registered\":{on_typing_indicator_registered},\"video_process_hook_registered\":{video_process_hook_registered}}}"
        ))
    }

    pub fn subscribe_events(&self) -> bool {
        true
    }

    pub fn subscribe_network_status(&self) -> bool {
        true
    }

    pub async fn clear_local_state(&self) -> Result<(), PrivchatFfiError> {
        self.inner
            .clear_local_state()
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn logout(&self) -> Result<(), PrivchatFfiError> {
        let _ = self.auth_logout_remote().await;
        let _ = self.clear_local_state().await;
        self.disconnect().await
    }

    pub fn enter_background(&self) {
        self.app_in_background.store(true, Ordering::Relaxed);
        self.stop_supervised_sync();
        eprintln!("[FFI] app lifecycle: enter_background");
    }

    pub fn enter_foreground(&self) {
        self.app_in_background.store(false, Ordering::Relaxed);
        let _ = self.start_supervised_sync(self.heartbeat_interval());
        eprintln!("[FFI] app lifecycle: enter_foreground");
    }

    pub fn on_app_background(&self) {
        self.enter_background();
    }

    pub fn on_app_foreground(&self) {
        self.enter_foreground();
    }

    pub fn register_lifecycle_hook(&self) {
        self.lifecycle_hook_registered.store(true, Ordering::Relaxed);
        eprintln!("[FFI] lifecycle hook registered");
    }

    pub fn start_transport_disconnect_listener(&self) {
        self.transport_disconnect_listener_started
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] transport disconnect listener started (event-poll mode)");
    }

    pub async fn log_connection_state(&self) -> Result<String, PrivchatFfiError> {
        self.get_connection_summary().await
    }

    pub async fn needs_sync(&self) -> Result<bool, PrivchatFfiError> {
        let done = self.is_bootstrap_completed().await?;
        Ok(!done)
    }

    pub async fn sync_entities_in_background(
        &self,
        entity_type: String,
        scope: Option<String>,
    ) -> Result<(), PrivchatFfiError> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _ = inner.sync_entities(entity_type, scope).await;
        });
        Ok(())
    }

    pub async fn sync_messages(&self) -> Result<u64, PrivchatFfiError> {
        self.sync_all_channels().await
    }

    pub async fn sync_messages_in_background(&self) -> Result<(), PrivchatFfiError> {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _ = inner.sync_all_channels().await;
        });
        Ok(())
    }

    pub fn timezone_seconds(&self) -> i32 {
        0
    }

    pub fn timezone_minutes(&self) -> i32 {
        self.timezone_seconds() / 60
    }

    pub fn timezone_hours(&self) -> i32 {
        self.timezone_seconds() / 3600
    }

    pub fn timezone_local(&self) -> String {
        "UTC".to_string()
    }

    pub fn debug_mode(&self) -> bool {
        cfg!(debug_assertions)
    }

    pub fn heartbeat_interval(&self) -> u64 {
        30
    }

    pub fn file_api_base_url(&self) -> String {
        let cfg = self.config();
        cfg.endpoints
            .first()
            .map(|ep| {
                let scheme = if ep.use_tls { "https" } else { "http" };
                format!("{scheme}://{}:{}", ep.host, ep.port)
            })
            .unwrap_or_default()
    }

    pub fn image_send_max_edge(&self) -> u32 {
        4096
    }

    pub fn event_config(&self) -> String {
        serde_json::json!({
            "broadcast_capacity": 256,
            "polling_api": "next_event(timeout_ms)",
            "polling_envelope_api": "next_event_envelope(timeout_ms)",
            "event_poll_count": self.event_poll_count.load(Ordering::Relaxed),
            "sequence_cursor": self.event_stream_cursor(),
            "replay_api": "events_since(sequence_id, limit)",
            "history_limit": self.inner.event_history_limit()
        })
        .to_string()
    }

    pub fn queue_config(&self) -> String {
        serde_json::json!({
            "normal_queue": "single",
            "file_queue": "multi"
        })
        .to_string()
    }

    pub fn retry_config(&self) -> String {
        serde_json::json!({
            "message_retry": true,
            "strategy": "manual+queue"
        })
        .to_string()
    }

    pub fn http_client_config(&self) -> String {
        let (scheme, tls) = if let Some(ep) = self.config().endpoints.first() {
            match ep.protocol {
                TransportProtocol::Quic => ("quic", false),
                TransportProtocol::Tcp => ("tcp", false),
                TransportProtocol::WebSocket => {
                    if ep.use_tls {
                        ("wss", true)
                    } else {
                        ("ws", false)
                    }
                }
            }
        } else {
            ("tcp", false)
        };
        serde_json::json!({
            "connection_timeout_secs": self.connection_timeout(),
            "tls": tls,
            "scheme": scheme
        })
        .to_string()
    }

    pub async fn data_dir(&self) -> Result<String, PrivchatFfiError> {
        let paths = self.user_storage_paths().await?;
        Ok(paths.user_root)
    }

    pub async fn assets_dir(&self) -> Result<String, PrivchatFfiError> {
        let paths = self.user_storage_paths().await?;
        Ok(paths.media_root)
    }

    pub async fn storage(&self) -> Result<UserStoragePaths, PrivchatFfiError> {
        self.user_storage_paths().await
    }

    pub async fn sync_entities(
        &self,
        entity_type: String,
        scope: Option<String>,
    ) -> Result<u64, PrivchatFfiError> {
        let count = self
            .inner
            .sync_entities(entity_type, scope)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(count as u64)
    }

    pub async fn sync_channel(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<u64, PrivchatFfiError> {
        let count = self
            .inner
            .sync_channel(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(count as u64)
    }

    pub async fn sync_all_channels(&self) -> Result<u64, PrivchatFfiError> {
        let count = self
            .inner
            .sync_all_channels()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(count as u64)
    }

    pub async fn subscribe_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<Vec<PresenceStatus>, PrivchatFfiError> {
        let out = self
            .inner
            .subscribe_presence(user_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_presence_status).collect())
    }

    pub async fn unsubscribe_presence(&self, user_ids: Vec<u64>) -> Result<(), PrivchatFfiError> {
        self.inner
            .unsubscribe_presence(user_ids)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn fetch_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<Vec<PresenceStatus>, PrivchatFfiError> {
        let out = self
            .inner
            .fetch_presence(user_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_presence_status).collect())
    }

    pub async fn batch_get_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<Vec<PresenceStatus>, PrivchatFfiError> {
        self.fetch_presence(user_ids).await
    }

    pub async fn get_presence(&self, user_id: u64) -> Result<Option<PresenceStatus>, PrivchatFfiError> {
        let mut out = self.fetch_presence(vec![user_id]).await?;
        Ok(out.pop())
    }

    pub async fn clear_presence_cache(&self) -> Result<(), PrivchatFfiError> {
        Ok(())
    }

    pub async fn send_typing(
        &self,
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        action_type: TypingActionType,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .send_typing(
                channel_id,
                channel_type,
                is_typing,
                map_typing_action(action_type),
            )
            .await
            .map_err(PrivchatFfiError::from)?;
        let mut active = self.typing_active_channels.lock().await;
        if is_typing {
            active.insert((channel_id, channel_type));
            self.typing_started_count.fetch_add(1, Ordering::Relaxed);
        } else {
            active.remove(&(channel_id, channel_type));
            self.typing_stopped_count.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub async fn start_typing(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.send_typing(channel_id, channel_type, true, TypingActionType::Typing)
            .await
    }

    pub async fn start_typing_blocking(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.start_typing(channel_id, channel_type).await
    }

    pub async fn stop_typing(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.send_typing(channel_id, channel_type, false, TypingActionType::Typing)
            .await
    }

    pub async fn rpc_call(
        &self,
        route: String,
        body_json: String,
    ) -> Result<String, PrivchatFfiError> {
        self.inner
            .rpc_call(route, body_json)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn search_users(&self, query: String) -> Result<String, PrivchatFfiError> {
        let resp: AccountSearchResponse = rpc_call_typed(
                &self.inner,
                routes::account_search::QUERY,
                &AccountSearchQueryRequest {
                    query,
                    page: Some(1),
                    page_size: Some(50),
                    from_user_id: 0,
                },
            )
            .await?;
        json_encode(&resp, "search_users response")
    }

    pub async fn send_friend_request(
        &self,
        target_user_id: u64,
        message: Option<String>,
        source: Option<String>,
        source_id: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: FriendApplyResponse = rpc_call_typed(
                &self.inner,
                routes::friend::APPLY,
                &FriendApplyRequest {
                    target_user_id,
                    message,
                    source,
                    source_id,
                    from_user_id: 0,
                },
            )
            .await?;
        json_encode(&resp, "send_friend_request response")
    }

    pub async fn get_friend_pending_requests(&self) -> Result<String, PrivchatFfiError> {
        let resp: FriendPendingResponse = rpc_call_typed(
                &self.inner,
                routes::friend::PENDING,
                &FriendPendingRequest { user_id: 0 },
            )
            .await?;
        json_encode(&resp, "get_friend_pending_requests response")
    }

    pub async fn accept_friend_request(
        &self,
        from_user_id: u64,
        message: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: FriendAcceptResponse = rpc_call_typed(
                &self.inner,
                routes::friend::ACCEPT,
                &FriendAcceptRequest {
                    from_user_id,
                    message,
                    target_user_id: 0,
                },
            )
            .await?;
        json_encode(&resp, "accept_friend_request response")
    }

    pub async fn reject_friend_request(
        &self,
        from_user_id: u64,
        message: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: FriendRejectResponse = rpc_call_typed(
                &self.inner,
                routes::friend::REJECT,
                &FriendRejectRequest {
                    from_user_id,
                    target_user_id: user_id,
                    message,
                },
            )
            .await?;
        json_encode(&resp, "reject_friend_request response")
    }

    pub async fn get_or_create_direct_channel(
        &self,
        peer_user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: GetOrCreateDirectChannelResponse = rpc_call_typed(
                &self.inner,
                routes::channel::DIRECT_GET_OR_CREATE,
                &GetOrCreateDirectChannelRequest {
                    target_user_id: peer_user_id,
                    source: None,
                    source_id: None,
                    user_id: 0,
                },
            )
            .await?;
        json_encode(&resp, "get_or_create_direct_channel response")
    }

    pub async fn create_group(
        &self,
        name: String,
        description: Option<String>,
        member_ids: Option<Vec<u64>>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: GroupCreateResponse = rpc_call_typed(
                &self.inner,
                routes::group::CREATE,
                &GroupCreateRequest {
                    name,
                    description,
                    member_ids,
                    creator_id: 0,
                },
            )
            .await?;
        json_encode(&resp, "create_group response")
    }

    pub async fn get_group_info(&self, group_id: u64) -> Result<String, PrivchatFfiError> {
        let resp: GroupInfoResponse = rpc_call_typed(
                &self.inner,
                routes::group::INFO,
                &GroupInfoRequest { group_id, user_id: 0 },
            )
            .await?;
        let now = now_millis();
        let created_at = now;
        let updated_at = now;
        let _ = self
            .inner
            .upsert_group(SdkUpsertGroupInput {
                group_id: resp.group_id,
                name: Some(resp.name.clone()),
                avatar: resp.avatar_url.clone().unwrap_or_default(),
                owner_id: Some(resp.owner_id),
                is_dismissed: resp.is_archived.unwrap_or(false),
                created_at,
                updated_at,
            })
            .await;
        json_encode(&resp, "get_group_info response")
    }

    pub async fn fetch_group_members_remote(
        &self,
        group_id: u64,
        _page: Option<u32>,
        _page_size: Option<u32>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: GroupMemberListResponse = rpc_call_typed(
                &self.inner,
                routes::group_member::LIST,
                &GroupMemberListRequest {
                    group_id,
                    user_id: 0,
                },
            )
            .await?;
        let now = now_millis();
        for entry in &resp.members {
            if let Some(user_id) = entry.get("user_id").and_then(|v| v.as_u64()) {
                let role = entry.get("role").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let status = entry.get("status").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                let alias = entry.get("alias").and_then(|v| v.as_str()).map(|s| s.to_string());
                let is_muted = entry.get("is_muted").and_then(|v| v.as_bool()).unwrap_or(false);
                let joined_at = entry.get("joined_at").and_then(|v| v.as_i64()).unwrap_or(now);
                let updated_at = entry.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(now);
                let _ = self
                    .inner
                    .upsert_group_member(SdkUpsertGroupMemberInput {
                        group_id,
                        user_id,
                        role,
                        status,
                        alias,
                        is_muted,
                        joined_at,
                        updated_at,
                    })
                    .await;
            }
        }
        json_encode(&resp, "fetch_group_members_remote response")
    }

    pub async fn delete_friend(&self, friend_id: u64) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: FriendRemoveResponse = rpc_call_typed(
                &self.inner,
                routes::friend::DELETE,
                &FriendRemoveRequest {
                    friend_id,
                    user_id,
                },
            )
            .await?;
        self.inner
            .delete_friend(friend_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        json_encode(&resp, "delete_friend response")
    }

    pub async fn add_to_blacklist(
        &self,
        blocked_user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: BlacklistAddResponse = rpc_call_typed(
                &self.inner,
                routes::blacklist::ADD,
                &BlacklistAddRequest {
                    user_id,
                    blocked_user_id,
                },
            )
            .await?;
        let ts = now_millis();
        self.inner
            .upsert_blacklist_entry(SdkUpsertBlacklistInput {
                blocked_user_id,
                created_at: ts,
                updated_at: ts,
            })
            .await
            .map_err(PrivchatFfiError::from)?;
        json_encode(&resp, "add_to_blacklist response")
    }

    pub async fn remove_from_blacklist(
        &self,
        blocked_user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: BlacklistRemoveResponse = rpc_call_typed(
                &self.inner,
                routes::blacklist::REMOVE,
                &BlacklistRemoveRequest {
                    user_id,
                    blocked_user_id,
                },
            )
            .await?;
        self.inner
            .delete_blacklist_entry(blocked_user_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        json_encode(&resp, "remove_from_blacklist response")
    }

    pub async fn get_blacklist(&self) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: BlacklistListResponse = rpc_call_typed(
                &self.inner,
                routes::blacklist::LIST,
                &BlacklistListRequest { user_id },
            )
            .await?;
        let remote_ids: HashSet<u64> = resp.users.iter().map(|u| u.user_id).collect();
        let local_entries = self
            .inner
            .list_blacklist_entries(10_000, 0)
            .await
            .map_err(PrivchatFfiError::from)?;
        for item in local_entries {
            if !remote_ids.contains(&item.blocked_user_id) {
                let _ = self.inner.delete_blacklist_entry(item.blocked_user_id).await;
            }
        }
        let ts = now_millis();
        for blocked_user_id in remote_ids {
            let _ = self
                .inner
                .upsert_blacklist_entry(SdkUpsertBlacklistInput {
                    blocked_user_id,
                    created_at: ts,
                    updated_at: ts,
                })
                .await;
        }
        json_encode(&resp, "get_blacklist response")
    }

    pub async fn check_blacklist(
        &self,
        target_user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: BlacklistCheckResponse = rpc_call_typed(
                &self.inner,
                routes::blacklist::CHECK,
                &BlacklistCheckRequest {
                    user_id,
                    target_user_id,
                },
            )
            .await?;
        if resp.is_blocked {
            let ts = now_millis();
            let _ = self
                .inner
                .upsert_blacklist_entry(SdkUpsertBlacklistInput {
                    blocked_user_id: target_user_id,
                    created_at: ts,
                    updated_at: ts,
                })
                .await;
        } else {
            let _ = self.inner.delete_blacklist_entry(target_user_id).await;
        }
        json_encode(&resp, "check_blacklist response")
    }

    pub async fn mark_as_read(
        &self,
        channel_id: u64,
        server_message_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: MessageStatusReadResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ,
            &MessageStatusReadRequest {
                channel_id,
                message_id: server_message_id,
                user_id,
            },
        )
        .await?;
        let channel_type = self.resolve_channel_type(channel_id).await;
        if let Some(local_message_id) = self
            .resolve_local_message_id_by_server_message_id(channel_id, channel_type, server_message_id)
            .await?
        {
            let _ = self
                .set_message_read(local_message_id, channel_id, channel_type, true)
                .await;
        }
        json_encode(&resp, "mark_as_read response")
    }

    pub async fn mark_as_read_blocking(
        &self,
        channel_id: u64,
        server_message_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        self.mark_as_read(channel_id, server_message_id).await
    }

    pub async fn recall_message(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageRevokeResponse = rpc_call_typed(
            &self.inner,
            routes::message::REVOKE,
            &MessageRevokeRequest {
                server_message_id,
                channel_id,
                user_id: 0,
            },
        )
        .await?;
        let channel_type = self.resolve_channel_type(channel_id).await;
        if let Some(local_message_id) = self
            .resolve_local_message_id_by_server_message_id(channel_id, channel_type, server_message_id)
            .await?
        {
            let revoker = self.current_user_id().await.ok();
            let _ = self.set_message_revoke(local_message_id, true, revoker).await;
        }
        json_encode(&resp, "recall_message response")
    }

    pub async fn recall_message_blocking(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        self.recall_message(server_message_id, channel_id).await
    }

    pub async fn add_reaction(
        &self,
        server_message_id: u64,
        channel_id: Option<u64>,
        emoji: String,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReactionAddResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::ADD,
            &MessageReactionAddRequest {
                server_message_id,
                channel_id,
                emoji: emoji.clone(),
                user_id: 0,
            },
        )
        .await?;
        if let Some(cid) = channel_id {
            let channel_type = self.resolve_channel_type(cid).await;
            if let Some(local_message_id) = self
                .resolve_local_message_id_by_server_message_id(cid, channel_type, server_message_id)
                .await?
            {
                let now = now_millis();
                let uid = self.current_user_id().await?;
                let _ = self
                    .upsert_message_reaction(UpsertMessageReactionInput {
                        channel_id: cid,
                        channel_type,
                        uid,
                        name: format!("u{uid}"),
                        emoji,
                        message_id: local_message_id,
                        seq: now,
                        is_deleted: false,
                        created_at: now,
                    })
                    .await;
            }
        }
        json_encode(&resp, "add_reaction response")
    }

    pub async fn add_reaction_blocking(
        &self,
        server_message_id: u64,
        channel_id: Option<u64>,
        emoji: String,
    ) -> Result<String, PrivchatFfiError> {
        self.add_reaction(server_message_id, channel_id, emoji).await
    }

    pub async fn remove_reaction(
        &self,
        server_message_id: u64,
        emoji: String,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReactionRemoveResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::REMOVE,
            &MessageReactionRemoveRequest {
                server_message_id,
                emoji: emoji.clone(),
                user_id: 0,
            },
        )
        .await?;
        // Best-effort local reaction tombstone update for local-first consistency.
        // When channel/message cannot be resolved from local cache, remote result is still returned.
        let uid = self.current_user_id().await?;
        let channels = self.list_channels(500, 0).await?;
        for ch in channels {
            if let Some(local_message_id) = self
                .resolve_local_message_id_by_server_message_id(
                    ch.channel_id,
                    ch.channel_type,
                    server_message_id,
                )
                .await?
            {
                let now = now_millis();
                let _ = self
                    .upsert_message_reaction(UpsertMessageReactionInput {
                        channel_id: ch.channel_id,
                        channel_type: ch.channel_type,
                        uid,
                        name: format!("u{uid}"),
                        emoji: emoji.clone(),
                        message_id: local_message_id,
                        seq: now,
                        is_deleted: true,
                        created_at: now,
                    })
                    .await;
                break;
            }
        }
        json_encode(&resp, "remove_reaction response")
    }

    pub async fn list_reactions(
        &self,
        server_message_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReactionListResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::LIST,
            &MessageReactionListRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "list_reactions response")
    }

    pub async fn reaction_stats(
        &self,
        server_message_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReactionStatsResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::STATS,
            &MessageReactionStatsRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "reaction_stats response")
    }

    pub async fn reactions(&self, server_message_id: u64) -> Result<String, PrivchatFfiError> {
        self.list_reactions(server_message_id).await
    }

    pub async fn reactions_batch(
        &self,
        server_message_ids: Vec<u64>,
    ) -> Result<String, PrivchatFfiError> {
        #[derive(Serialize)]
        struct ReactionBatchItem {
            server_message_id: u64,
            reactions: serde_json::Value,
        }
        let mut items = Vec::with_capacity(server_message_ids.len());
        for id in server_message_ids {
            let value: MessageReactionListResponse = rpc_call_typed(
                &self.inner,
                routes::message_reaction::LIST,
                &MessageReactionListRequest {
                    server_message_id: id,
                    user_id: 0,
                },
            )
            .await?;
            items.push(ReactionBatchItem {
                server_message_id: id,
                reactions: serde_json::to_value(value.reactions).unwrap_or(serde_json::Value::Null),
            });
        }
        json_encode(&items, "reactions_batch response")
    }

    pub async fn pin_channel(
        &self,
        channel_id: u64,
        pinned: bool,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: ChannelPinResponse = rpc_call_typed(
            &self.inner,
            routes::channel::PIN,
            &ChannelPinRequest {
                user_id,
                channel_id,
                pinned,
            },
        )
        .await?;
        if let Ok(Some(ch)) = self.get_channel_by_id(channel_id).await {
            let _ = self
                .upsert_channel(UpsertChannelInput {
                    channel_id: ch.channel_id,
                    channel_type: ch.channel_type,
                    channel_name: ch.channel_name,
                    channel_remark: ch.channel_remark,
                    avatar: ch.avatar,
                    unread_count: ch.unread_count,
                    top: if pinned { 1 } else { 0 },
                    mute: ch.mute,
                    last_msg_timestamp: ch.last_msg_timestamp,
                    last_local_message_id: ch.last_local_message_id,
                    last_msg_content: ch.last_msg_content,
                })
                    .await;
        }
        json_encode(&resp, "pin_channel response")
    }

    pub async fn hide_channel(&self, channel_id: u64) -> Result<String, PrivchatFfiError> {
        let resp: ChannelHideResponse = rpc_call_typed(
            &self.inner,
            routes::channel::HIDE,
            &ChannelHideRequest {
                user_id: 0,
                channel_id,
            },
        )
        .await?;
        json_encode(&resp, "hide_channel response")
    }

    pub async fn mute_channel(
        &self,
        channel_id: u64,
        muted: bool,
    ) -> Result<String, PrivchatFfiError> {
        let resp: ChannelMuteResponse = rpc_call_typed(
            &self.inner,
            routes::channel::MUTE,
            &ChannelMuteRequest {
                user_id: 0,
                channel_id,
                muted,
            },
        )
        .await?;
        if let Ok(Some(ch)) = self.get_channel_by_id(channel_id).await {
            let _ = self
                .upsert_channel(UpsertChannelInput {
                    channel_id: ch.channel_id,
                    channel_type: ch.channel_type,
                    channel_name: ch.channel_name,
                    channel_remark: ch.channel_remark,
                    avatar: ch.avatar,
                    unread_count: ch.unread_count,
                    top: ch.top,
                    mute: if muted { 1 } else { 0 },
                    last_msg_timestamp: ch.last_msg_timestamp,
                    last_local_message_id: ch.last_local_message_id,
                    last_msg_content: ch.last_msg_content,
                })
                    .await;
        }
        json_encode(&resp, "mute_channel response")
    }

    pub async fn update_device_push_state(
        &self,
        device_id: String,
        apns_armed: bool,
        push_token: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: DevicePushUpdateResponse = rpc_call_typed(
            &self.inner,
            routes::device::PUSH_UPDATE,
            &DevicePushUpdateRequest {
                device_id,
                apns_armed,
                push_token,
            },
        )
        .await?;
        json_encode(&resp, "update_device_push_state response")
    }

    pub async fn get_device_push_status(
        &self,
        device_id: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: DevicePushStatusResponse = rpc_call_typed(
            &self.inner,
            routes::device::PUSH_STATUS,
            &DevicePushStatusRequest { device_id },
        )
        .await?;
        json_encode(&resp, "get_device_push_status response")
    }

    pub async fn get_messages_remote(
        &self,
        channel_id: u64,
        before_server_message_id: Option<u64>,
        limit: Option<u32>,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: MessageHistoryResponse = rpc_call_typed(
            &self.inner,
            routes::message_history::GET,
            &MessageHistoryGetRequest {
                user_id,
                channel_id,
                before_server_message_id,
                limit,
            },
        )
        .await?;
        json_encode(&resp, "get_messages_remote response")
    }

    pub async fn message_read_list(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        json_encode(&resp, "message_read_list response")
    }

    pub async fn message_read_stats(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageReadStatsResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_STATS,
            &MessageReadStatsRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        json_encode(&resp, "message_read_stats response")
    }

    pub async fn check_friend(&self, friend_id: u64) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: FriendCheckResponse = rpc_call_typed(
                &self.inner,
                routes::friend::CHECK,
                &FriendCheckRequest { friend_id, user_id },
            )
            .await?;
        if resp.is_friend {
            let ts = now_millis();
            let _ = self
                .inner
                .upsert_friend(SdkUpsertFriendInput {
                    user_id: friend_id,
                    tags: None,
                    is_pinned: false,
                    created_at: ts,
                    updated_at: ts,
                })
                .await;
        } else {
            let _ = self.inner.delete_friend(friend_id).await;
        }
        json_encode(&resp, "check_friend response")
    }

    pub async fn get_profile(&self) -> Result<String, PrivchatFfiError> {
        let resp: AccountProfileGetResponse = rpc_call_typed(
            &self.inner,
            routes::account_profile::GET,
            &AccountProfileGetRequest { user_id: 0 },
        )
        .await?;
        let out = json_encode(&resp, "get_profile response")?;
        if let Some(user) = parse_profile_user(&out) {
            let _ = self.inner.upsert_user(user).await;
        }
        Ok(out)
    }

    pub async fn update_profile(&self, profile_json: String) -> Result<String, PrivchatFfiError> {
        let mut req: AccountProfileUpdateRequest =
            json_decode(&profile_json, "update_profile request")?;
        req.user_id = 0;
        let resp: AccountProfileUpdateResponse =
            rpc_call_typed(&self.inner, routes::account_profile::UPDATE, &req).await?;
        let out = json_encode(&resp, "update_profile response")?;
        if let Some(user) = parse_profile_user(&out) {
            let _ = self.inner.upsert_user(user).await;
        }
        Ok(out)
    }

    pub async fn get_privacy_settings(&self) -> Result<String, PrivchatFfiError> {
        let resp: AccountPrivacyGetResponse = rpc_call_typed(
            &self.inner,
            routes::privacy::GET,
            &AccountPrivacyGetRequest { user_id: 0 },
        )
        .await?;
        json_encode(&resp, "get_privacy_settings response")
    }

    pub async fn update_privacy_settings(
        &self,
        settings_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: AccountPrivacyUpdateRequest =
            json_decode(&settings_json, "update_privacy_settings request")?;
        req.user_id = 0;
        let resp: AccountPrivacyUpdateResponse =
            rpc_call_typed(&self.inner, routes::privacy::UPDATE, &req).await?;
        json_encode(&resp, "update_privacy_settings response")
    }

    pub async fn qrcode_generate(
        &self,
        qr_type: String,
        payload: String,
        expire_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: QRCodeGenerateResponse = rpc_call_typed(
            &self.inner,
            routes::qrcode::GENERATE,
            &QRCodeGenerateRequest {
                qr_type,
                target_id: payload,
                expire_seconds,
                max_usage: None,
                metadata: None,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "qrcode_generate response")
    }

    pub async fn qrcode_resolve(
        &self,
        qr_key: String,
        token: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: QRCodeResolveResponse = rpc_call_typed(
            &self.inner,
            routes::qrcode::RESOLVE,
            &QRCodeResolveRequest {
                qr_key,
                token,
                scanner_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "qrcode_resolve response")
    }

    pub async fn qrcode_list(&self, qr_type: Option<String>) -> Result<String, PrivchatFfiError> {
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::qrcode::LIST,
            &QRCodeListRequest { qr_type, user_id: 0 },
        )
        .await?;
        json_encode(&resp, "qrcode_list response")
    }

    pub async fn user_qrcode_get(&self) -> Result<String, PrivchatFfiError> {
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::GET,
            &UserQRCodeGetRequest { user_id: 0 },
        )
        .await?;
        json_encode(&resp, "user_qrcode_get response")
    }

    pub async fn search_user_by_qrcode(
        &self,
        qr_key: String,
        token: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: AccountSearchResponse = rpc_call_typed(
            &self.inner,
            routes::account_search::BY_QRCODE,
            &AccountSearchByQRCodeRequest {
                qr_key,
                token,
                searcher_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "search_user_by_qrcode response")
    }

    pub async fn account_user_detail_remote(
        &self,
        user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: AccountUserDetailResponse = rpc_call_typed(
            &self.inner,
            routes::account_user::DETAIL,
            &AccountUserDetailRequest {
                target_user_id: user_id,
                source: "profile".to_string(),
                source_id: "ffi".to_string(),
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "account_user_detail_remote response")
    }

    pub async fn account_user_share_card_remote(
        &self,
        user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let receiver_id = self.current_user_id().await?;
        let resp: AccountUserShareCardResponse = rpc_call_typed(
            &self.inner,
            routes::account_user::SHARE_CARD,
            &AccountUserShareCardRequest {
                target_user_id: user_id,
                receiver_id,
                expire_seconds: None,
                sharer_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "account_user_share_card_remote response")
    }

    pub async fn account_user_update_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: AccountUserUpdateRequest =
            json_decode(&payload_json, "account_user_update_remote request")?;
        req.user_id = 0;
        let resp: AccountUserUpdateResponse =
            rpc_call_typed(&self.inner, routes::account_user::UPDATE, &req).await?;
        json_encode(&resp, "account_user_update_remote response")
    }

    pub async fn auth_logout_remote(&self) -> Result<String, PrivchatFfiError> {
        let resp: AuthLogoutResponse =
            rpc_call_typed(&self.inner, routes::auth::LOGOUT, &AuthLogoutRequest {}).await?;
        json_encode(&resp, "auth_logout_remote response")
    }

    pub async fn auth_refresh_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: AuthRefreshRequest = json_decode(&payload_json, "auth_refresh_remote request")?;
        let resp: AuthRefreshResponse =
            rpc_call_typed(&self.inner, routes::auth::REFRESH, &req).await?;
        json_encode(&resp, "auth_refresh_remote response")
    }

    pub async fn qrcode_refresh(
        &self,
        qr_key: String,
        expire_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let _ = expire_seconds;
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::qrcode::REFRESH,
            &QRCodeRefreshRequest {
                old_qr_key: qr_key,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "qrcode_refresh response")
    }

    pub async fn qrcode_revoke(&self, qr_key: String) -> Result<String, PrivchatFfiError> {
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::qrcode::REVOKE,
            &QRCodeRevokeRequest { qr_key, user_id: 0 },
        )
        .await?;
        json_encode(&resp, "qrcode_revoke response")
    }

    pub async fn user_qrcode_generate(
        &self,
        expire_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::GENERATE,
            &UserQRCodeGenerateRequest {
                expire_seconds,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "user_qrcode_generate response")
    }

    pub async fn user_qrcode_refresh(
        &self,
        expire_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let _ = expire_seconds;
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::REFRESH,
            &UserQRCodeRefreshRequest { user_id: 0 },
        )
        .await?;
        json_encode(&resp, "user_qrcode_refresh response")
    }

    pub async fn message_unread_count_remote(
        &self,
        channel_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let resp: MessageStatusCountResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::COUNT,
            &MessageStatusCountRequest {
                channel_id: Some(channel_id),
            },
        )
        .await?;
        json_encode(&resp, "message_unread_count_remote response")
    }

    pub async fn group_add_members_remote(
        &self,
        group_id: u64,
        user_ids: Vec<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let mut last_resp: Option<GroupMemberAddResponse> = None;
        for uid in &user_ids {
            let resp: GroupMemberAddResponse = rpc_call_typed(
                &self.inner,
                routes::group_member::ADD,
                &GroupMemberAddRequest {
                    group_id,
                    user_id: *uid,
                    role: None,
                    inviter_id: 0,
                },
            )
            .await?;
            last_resp = Some(resp);
        }
        let now = now_millis();
        for user_id in user_ids {
            let _ = self
                .inner
                .upsert_group_member(SdkUpsertGroupMemberInput {
                    group_id,
                    user_id,
                    role: 0,
                    status: 0,
                    alias: None,
                    is_muted: false,
                    joined_at: now,
                    updated_at: now,
                })
                .await;
        }
        json_encode(&last_resp.unwrap_or(true), "group_add_members_remote response")
    }

    pub async fn group_remove_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let operator_id = self.current_user_id().await?;
        let resp: GroupMemberRemoveResponse = rpc_call_typed(
            &self.inner,
            routes::group_member::REMOVE,
            &GroupMemberRemoveRequest {
                group_id,
                user_id,
                operator_id,
            },
        )
        .await?;
        let _ = self.inner.delete_group_member(group_id, user_id).await;
        json_encode(&resp, "group_remove_member_remote response")
    }

    pub async fn remove_group_member(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        self.group_remove_member_remote(group_id, user_id).await
    }

    pub async fn group_leave_remote(&self, group_id: u64) -> Result<String, PrivchatFfiError> {
        let resp: GroupMemberLeaveResponse = rpc_call_typed(
            &self.inner,
            routes::group_member::LEAVE,
            &GroupMemberLeaveRequest { group_id, user_id: 0 },
        )
        .await?;
        if let Ok(current_uid) = self.current_user_id().await {
            let _ = self.inner.delete_group_member(group_id, current_uid).await;
        }
        json_encode(&resp, "group_leave_remote response")
    }

    pub async fn leave_group(&self, group_id: u64) -> Result<String, PrivchatFfiError> {
        self.group_leave_remote(group_id).await
    }

    pub async fn invite_to_group(
        &self,
        group_id: u64,
        member_ids: Vec<u64>,
    ) -> Result<String, PrivchatFfiError> {
        self.group_add_members_remote(group_id, member_ids).await
    }

    pub async fn group_mute_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
        duration_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let operator_id = self.current_user_id().await?;
        let resp: GroupMemberMuteResponse = rpc_call_typed(
            &self.inner,
            routes::group_member::MUTE,
            &GroupMemberMuteRequest {
                group_id,
                operator_id,
                user_id,
                mute_duration: duration_seconds.unwrap_or(0),
            },
        )
        .await?;
        let now = now_millis();
        let _ = self
            .inner
            .upsert_group_member(SdkUpsertGroupMemberInput {
                group_id,
                user_id,
                role: 0,
                status: 0,
                alias: None,
                is_muted: true,
                joined_at: now,
                updated_at: now,
            })
            .await;
        json_encode(&resp, "group_mute_member_remote response")
    }

    pub async fn group_unmute_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let operator_id = self.current_user_id().await?;
        let resp: GroupMemberUnmuteResponse = rpc_call_typed(
            &self.inner,
            routes::group_member::UNMUTE,
            &GroupMemberUnmuteRequest {
                group_id,
                operator_id,
                user_id,
            },
        )
        .await?;
        let now = now_millis();
        let _ = self
            .inner
            .upsert_group_member(SdkUpsertGroupMemberInput {
                group_id,
                user_id,
                role: 0,
                status: 0,
                alias: None,
                is_muted: false,
                joined_at: now,
                updated_at: now,
            })
            .await;
        json_encode(&resp, "group_unmute_member_remote response")
    }

    pub async fn group_transfer_owner_remote(
        &self,
        group_id: u64,
        target_user_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let current_owner_id = self.current_user_id().await?;
        let resp: GroupTransferOwnerResponse = rpc_call_typed(
            &self.inner,
            routes::group_role::TRANSFER_OWNER,
            &GroupTransferOwnerRequest {
                group_id,
                current_owner_id,
                new_owner_id: target_user_id,
            },
        )
        .await?;
        let now = now_millis();
        let existing = self.get_group_by_id(group_id).await.ok().flatten();
        let _ = self
            .inner
            .upsert_group(SdkUpsertGroupInput {
                group_id,
                name: existing.as_ref().and_then(|g| g.name.clone()),
                avatar: existing
                    .as_ref()
                    .map(|g| g.avatar.clone())
                    .unwrap_or_default(),
                owner_id: Some(target_user_id),
                is_dismissed: existing.as_ref().map(|g| g.is_dismissed).unwrap_or(false),
                created_at: existing.as_ref().map(|g| g.created_at).unwrap_or(now),
                updated_at: now,
            })
            .await;
        json_encode(&resp, "group_transfer_owner_remote response")
    }

    pub async fn group_set_role_remote(
        &self,
        group_id: u64,
        user_id: u64,
        role: String,
    ) -> Result<String, PrivchatFfiError> {
        let role_code = parse_group_role_to_code(&role);
        let operator_id = self.current_user_id().await?;
        let resp: GroupRoleSetResponse = rpc_call_typed(
            &self.inner,
            routes::group_role::SET,
            &GroupRoleSetRequest {
                group_id,
                operator_id,
                user_id,
                role: role.clone(),
            },
        )
        .await?;
        let now = now_millis();
        let _ = self
            .inner
            .upsert_group_member(SdkUpsertGroupMemberInput {
                group_id,
                user_id,
                role: role_code,
                status: 0,
                alias: None,
                is_muted: false,
                joined_at: now,
                updated_at: now,
            })
            .await;
        json_encode(&resp, "group_set_role_remote response")
    }

    pub async fn group_get_settings_remote(
        &self,
        group_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        let user_id = self.current_user_id().await?;
        let resp: GroupSettingsGetResponse = rpc_call_typed(
            &self.inner,
            routes::group_settings::GET,
            &GroupSettingsGetRequest { group_id, user_id },
        )
        .await?;
        let out = json_encode(&resp, "group_get_settings_remote response")?;
        let _ = self
            .kv_put(group_settings_key(group_id), out.clone().into_bytes())
            .await;
        Ok(out)
    }

    pub async fn group_update_settings_remote(
        &self,
        settings_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: GroupSettingsUpdateRequest =
            json_decode(&settings_json, "group_update_settings_remote request")?;
        req.operator_id = self.current_user_id().await?;
        let resp: GroupSettingsUpdateResponse =
            rpc_call_typed(&self.inner, routes::group_settings::UPDATE, &req).await?;
        if req.group_id > 0 {
            let _ = self
                .kv_put(group_settings_key(req.group_id), settings_json.into_bytes())
                .await;
        }
        json_encode(&resp, "group_update_settings_remote response")
    }

    pub async fn group_mute_all_remote(
        &self,
        group_id: u64,
        enabled: bool,
    ) -> Result<String, PrivchatFfiError> {
        let operator_id = self.current_user_id().await?;
        let resp: serde_json::Value = rpc_call_typed(
            &self.inner,
            routes::group_settings::MUTE_ALL,
            &GroupMuteAllRequest {
                group_id,
                operator_id,
                mute_all: enabled,
            },
        )
        .await?;
        let out = json_encode(&resp, "group_mute_all_remote response")?;
        let key = group_settings_key(group_id);
        let raw = self.kv_get(key.clone()).await?;
        let mut map: serde_json::Map<String, serde_json::Value> = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        map.insert("group_id".to_string(), serde_json::json!(group_id));
        map.insert("mute_all".to_string(), serde_json::json!(enabled));
        let _ = self
            .kv_put(key, serde_json::Value::Object(map).to_string().into_bytes())
            .await;
        Ok(out)
    }

    pub async fn group_approval_list_remote(
        &self,
        group_id: u64,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> Result<String, PrivchatFfiError> {
        let _ = page;
        let _ = page_size;
        let operator_id = self.current_user_id().await?;
        let resp: GroupApprovalListResponse = rpc_call_typed(
            &self.inner,
            routes::group_approval::LIST,
            &GroupApprovalListRequest {
                group_id,
                operator_id,
            },
        )
        .await?;
        json_encode(&resp, "group_approval_list_remote response")
    }

    pub async fn group_approval_handle_remote(
        &self,
        approval_id: u64,
        approved: bool,
        reason: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let operator_id = self.current_user_id().await?;
        let resp: GroupApprovalHandleResponse = rpc_call_typed(
            &self.inner,
            routes::group_approval::HANDLE,
            &GroupApprovalHandleRequest {
                approval_id,
                operator_id,
                approved,
                reject_reason: reason,
            },
        )
        .await?;
        json_encode(&resp, "group_approval_handle_remote response")
    }

    pub async fn group_qrcode_generate_remote(
        &self,
        group_id: u64,
        expire_seconds: Option<u64>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: GroupQRCodeGenerateResponse = rpc_call_typed(
            &self.inner,
            routes::group_qrcode::GENERATE,
            &GroupQRCodeGenerateRequest {
                group_id,
                expire_seconds,
                operator_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "group_qrcode_generate_remote response")
    }

    pub async fn group_qrcode_join_remote(
        &self,
        qr_key: String,
        token: Option<String>,
    ) -> Result<String, PrivchatFfiError> {
        let resp: GroupQRCodeJoinResponse = rpc_call_typed(
            &self.inner,
            routes::group_qrcode::JOIN,
            &GroupQRCodeJoinRequest {
                qr_key,
                token,
                message: None,
                user_id: 0,
            },
        )
        .await?;
        json_encode(&resp, "group_qrcode_join_remote response")
    }

    pub async fn channel_broadcast_create_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: ChannelBroadcastCreateRequest =
            json_decode(&payload_json, "channel_broadcast_create_remote request")?;
        let resp: ChannelBroadcastCreateResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::CREATE, &req).await?;
        json_encode(&resp, "channel_broadcast_create_remote response")
    }

    pub async fn channel_broadcast_subscribe_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: ChannelBroadcastSubscribeRequest =
            json_decode(&payload_json, "channel_broadcast_subscribe_remote request")?;
        req.user_id = 0;
        let resp: ChannelBroadcastSubscribeResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::SUBSCRIBE, &req).await?;
        json_encode(&resp, "channel_broadcast_subscribe_remote response")
    }

    pub async fn channel_broadcast_list_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: ChannelBroadcastListRequest =
            json_decode(&payload_json, "channel_broadcast_list_remote request")?;
        let resp: ChannelBroadcastListResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::LIST, &req).await?;
        json_encode(&resp, "channel_broadcast_list_remote response")
    }

    pub async fn channel_content_publish_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: ChannelContentPublishRequest =
            json_decode(&payload_json, "channel_content_publish_remote request")?;
        let resp: ChannelContentPublishResponse =
            rpc_call_typed(&self.inner, routes::channel_content::PUBLISH, &req).await?;
        json_encode(&resp, "channel_content_publish_remote response")
    }

    pub async fn channel_content_list_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: ChannelContentListRequest =
            json_decode(&payload_json, "channel_content_list_remote request")?;
        let resp: ChannelContentListResponse =
            rpc_call_typed(&self.inner, routes::channel_content::LIST, &req).await?;
        json_encode(&resp, "channel_content_list_remote response")
    }

    pub async fn sticker_package_list_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: StickerPackageListRequest =
            json_decode(&payload_json, "sticker_package_list_remote request")?;
        let resp: StickerPackageListResponse =
            rpc_call_typed(&self.inner, routes::sticker::PACKAGE_LIST, &req).await?;
        json_encode(&resp, "sticker_package_list_remote response")
    }

    pub async fn sticker_package_detail_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: StickerPackageDetailRequest =
            json_decode(&payload_json, "sticker_package_detail_remote request")?;
        let resp: StickerPackageDetailResponse =
            rpc_call_typed(&self.inner, routes::sticker::PACKAGE_DETAIL, &req).await?;
        json_encode(&resp, "sticker_package_detail_remote response")
    }

    pub async fn sync_submit_remote(&self, payload_json: String) -> Result<String, PrivchatFfiError> {
        let req: ClientSubmitRequest = json_decode(&payload_json, "sync_submit_remote request")?;
        let resp: ClientSubmitResponse = rpc_call_typed(&self.inner, routes::sync::SUBMIT, &req).await?;
        json_encode(&resp, "sync_submit_remote response")
    }

    pub async fn entity_sync_remote(&self, payload_json: String) -> Result<String, PrivchatFfiError> {
        let req: SyncEntitiesRequest = json_decode(&payload_json, "entity_sync_remote request")?;
        let resp: SyncEntitiesResponse =
            rpc_call_typed(&self.inner, routes::entity::SYNC_ENTITIES, &req).await?;
        json_encode(&resp, "entity_sync_remote response")
    }

    pub async fn sync_get_difference_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: GetDifferenceRequest =
            json_decode(&payload_json, "sync_get_difference_remote request")?;
        let resp: GetDifferenceResponse =
            rpc_call_typed(&self.inner, routes::sync::GET_DIFFERENCE, &req).await?;
        json_encode(&resp, "sync_get_difference_remote response")
    }

    pub async fn sync_get_channel_pts_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: GetChannelPtsRequest =
            json_decode(&payload_json, "sync_get_channel_pts_remote request")?;
        let resp: GetChannelPtsResponse =
            rpc_call_typed(&self.inner, routes::sync::GET_CHANNEL_PTS, &req).await?;
        json_encode(&resp, "sync_get_channel_pts_remote response")
    }

    pub async fn sync_batch_get_channel_pts_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let req: BatchGetChannelPtsRequest =
            json_decode(&payload_json, "sync_batch_get_channel_pts_remote request")?;
        let resp: BatchGetChannelPtsResponse =
            rpc_call_typed(&self.inner, routes::sync::BATCH_GET_CHANNEL_PTS, &req).await?;
        json_encode(&resp, "sync_batch_get_channel_pts_remote response")
    }

    pub async fn file_request_upload_token_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: FileRequestUploadTokenRequest =
            json_decode(&payload_json, "file_request_upload_token_remote request")?;
        req.user_id = 0;
        let resp: FileRequestUploadTokenResponse =
            rpc_call_typed(&self.inner, routes::file::REQUEST_UPLOAD_TOKEN, &req).await?;
        json_encode(&resp, "file_request_upload_token_remote response")
    }

    pub async fn file_upload_callback_remote(
        &self,
        payload_json: String,
    ) -> Result<String, PrivchatFfiError> {
        let mut req: FileUploadCallbackRequest =
            json_decode(&payload_json, "file_upload_callback_remote request")?;
        req.user_id = 0;
        let resp: FileUploadCallbackResponse =
            rpc_call_typed(&self.inner, routes::file::UPLOAD_CALLBACK, &req).await?;
        json_encode(&resp, "file_upload_callback_remote response")
    }

    pub async fn enqueue_outbound_message(
        &self,
        message_id: u64,
        payload: Vec<u8>,
    ) -> Result<u64, PrivchatFfiError> {
        if !self.send_queue_enabled.load(Ordering::Relaxed) {
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: "send queue disabled".to_string(),
            });
        }
        if let Some(msg) = self.get_message_by_id(message_id).await? {
            let disabled = self.disabled_channel_queues.lock().await;
            if disabled.contains(&(msg.channel_id, msg.channel_type)) {
                return Err(PrivchatFfiError::SdkError {
                    code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                    detail: format!(
                        "channel send queue disabled: channel_id={}, channel_type={}",
                        msg.channel_id, msg.channel_type
                    ),
                });
            }
        }
        self.inner
            .enqueue_outbound_message(message_id, payload)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn send_queue_set_enabled(&self, enabled: bool) -> Result<(), PrivchatFfiError> {
        self.send_queue_enabled.store(enabled, Ordering::Relaxed);
        Ok(())
    }

    pub async fn channel_send_queue_set_enabled(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<(), PrivchatFfiError> {
        let mut disabled = self.disabled_channel_queues.lock().await;
        if enabled {
            disabled.remove(&(channel_id, channel_type));
        } else {
            disabled.insert((channel_id, channel_type));
        }
        Ok(())
    }

    pub async fn enqueue_text(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
    ) -> Result<u64, PrivchatFfiError> {
        let message_id = self
            .create_local_message(NewMessage {
                channel_id,
                channel_type,
                from_uid,
                message_type: 1,
                content,
                searchable_word: String::new(),
                setting: 0,
                extra: String::new(),
            })
            .await?;
        self.enqueue_outbound_message(message_id, Vec::new()).await?;
        Ok(message_id)
    }

    pub async fn send_message(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
    ) -> Result<u64, PrivchatFfiError> {
        self.enqueue_text(channel_id, channel_type, from_uid, content)
            .await
    }

    pub async fn send_message_blocking(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
    ) -> Result<u64, PrivchatFfiError> {
        self.send_message(channel_id, channel_type, from_uid, content)
            .await
    }

    pub async fn send_message_with_input(&self, input: NewMessage) -> Result<u64, PrivchatFfiError> {
        let message_id = self.create_local_message(input).await?;
        self.enqueue_outbound_message(message_id, Vec::new()).await?;
        Ok(message_id)
    }

    pub async fn send_message_with_options(
        &self,
        input: NewMessage,
        _options_json: String,
    ) -> Result<u64, PrivchatFfiError> {
        self.send_message_with_input(input).await
    }

    pub async fn peek_outbound_messages(
        &self,
        limit: u64,
    ) -> Result<Vec<QueueMessage>, PrivchatFfiError> {
        let items = self
            .inner
            .peek_outbound_messages(limit as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(items.into_iter().map(map_queue_message).collect())
    }

    pub async fn ack_outbound_messages(
        &self,
        message_ids: Vec<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        let removed = self
            .inner
            .ack_outbound_messages(message_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(removed as u64)
    }

    pub async fn enqueue_outbound_file(
        &self,
        message_id: u64,
        route_key: String,
        payload: Vec<u8>,
    ) -> Result<FileQueueRef, PrivchatFfiError> {
        if !self.send_queue_enabled.load(Ordering::Relaxed) {
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: "send queue disabled".to_string(),
            });
        }
        if let Some(msg) = self.get_message_by_id(message_id).await? {
            let disabled = self.disabled_channel_queues.lock().await;
            if disabled.contains(&(msg.channel_id, msg.channel_type)) {
                return Err(PrivchatFfiError::SdkError {
                    code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                    detail: format!(
                        "channel send queue disabled: channel_id={}, channel_type={}",
                        msg.channel_id, msg.channel_type
                    ),
                });
            }
        }
        let out = self
            .inner
            .enqueue_outbound_file(message_id, route_key, payload)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(map_file_queue_ref(out))
    }

    pub async fn peek_outbound_files(
        &self,
        queue_index: u64,
        limit: u64,
    ) -> Result<Vec<QueueMessage>, PrivchatFfiError> {
        let items = self
            .inner
            .peek_outbound_files(queue_index as usize, limit as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(items.into_iter().map(map_queue_message).collect())
    }

    pub async fn ack_outbound_files(
        &self,
        queue_index: u64,
        message_ids: Vec<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        let removed = self
            .inner
            .ack_outbound_files(queue_index as usize, message_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(removed as u64)
    }

    pub async fn create_local_message(&self, input: NewMessage) -> Result<u64, PrivchatFfiError> {
        self.inner
            .create_local_message(map_new_message(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_message_by_id(
        &self,
        message_id: u64,
    ) -> Result<Option<StoredMessage>, PrivchatFfiError> {
        let out = self
            .inner
            .get_message_by_id(message_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_message))
    }

    pub async fn list_messages(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        let out = self
            .inner
            .list_messages(channel_id, channel_type, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_message).collect())
    }

    pub async fn get_messages(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        self.list_messages(channel_id, channel_type, limit, offset)
            .await
    }

    pub async fn upsert_channel(&self, input: UpsertChannelInput) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_channel(map_upsert_channel(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_channel_by_id(
        &self,
        channel_id: u64,
    ) -> Result<Option<StoredChannel>, PrivchatFfiError> {
        let out = self
            .inner
            .get_channel_by_id(channel_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_channel))
    }

    pub async fn list_channels(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredChannel>, PrivchatFfiError> {
        let out = self
            .inner
            .list_channels(limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_channel).collect())
    }

    pub async fn get_channels(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredChannel>, PrivchatFfiError> {
        self.list_channels(limit, offset).await
    }

    pub async fn upsert_channel_extra(
        &self,
        input: UpsertChannelExtraInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_channel_extra(map_upsert_channel_extra(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_channel_extra(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<StoredChannelExtra>, PrivchatFfiError> {
        let out = self
            .inner
            .get_channel_extra(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_channel_extra))
    }

    pub async fn mark_message_sent(
        &self,
        message_id: u64,
        server_message_id: u64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_message_sent(message_id, server_message_id)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn update_message_status(
        &self,
        message_id: u64,
        status: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .update_message_status(message_id, status)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn set_message_read(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_message_read(message_id, channel_id, channel_type, is_read)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn set_message_revoke(
        &self,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_message_revoke(message_id, revoked, revoker)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn edit_message(
        &self,
        message_id: u64,
        content: String,
        edited_at: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .edit_message(message_id, content, edited_at)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn edit_message_blocking(
        &self,
        message_id: u64,
        content: String,
        edited_at: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.edit_message(message_id, content, edited_at).await
    }

    pub async fn set_message_pinned(
        &self,
        message_id: u64,
        is_pinned: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_message_pinned(message_id, is_pinned)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_message_extra(
        &self,
        message_id: u64,
    ) -> Result<Option<StoredMessageExtra>, PrivchatFfiError> {
        let out = self
            .inner
            .get_message_extra(message_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_message_extra))
    }

    pub async fn mark_channel_read(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_channel_read(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_channel_unread_count(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32, PrivchatFfiError> {
        self.inner
            .get_channel_unread_count(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_total_unread_count(
        &self,
        exclude_muted: bool,
    ) -> Result<i32, PrivchatFfiError> {
        self.inner
            .get_total_unread_count(exclude_muted)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn channel_unread_stats(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32, PrivchatFfiError> {
        self.get_channel_unread_count(channel_id, channel_type).await
    }

    pub async fn get_channel_sync_state(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<String, PrivchatFfiError> {
        let unread = self.get_channel_unread_count(channel_id, channel_type).await?;
        Ok(format!(
            "{{\"channel_id\":{channel_id},\"channel_type\":{channel_type},\"unread\":{unread}}}"
        ))
    }

    pub async fn get_channel_list_entries(
        &self,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<StoredChannel>, PrivchatFfiError> {
        self.list_channels(page, page_size).await
    }

    pub async fn get_earliest_id(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<u64>, PrivchatFfiError> {
        let list = self.list_messages(channel_id, channel_type, 1, 1).await?;
        Ok(list.first().map(|m| m.message_id))
    }

    pub async fn set_channel_notification_mode(
        &self,
        channel_id: u64,
        channel_type: i32,
        mode: i32,
    ) -> Result<(), PrivchatFfiError> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get(key.clone()).await?;
        let mut map: serde_json::Map<String, serde_json::Value> = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        map.insert("notification_mode".to_string(), serde_json::json!(mode));
        self.kv_put(key, serde_json::Value::Object(map).to_string().into_bytes())
            .await
    }

    pub async fn channel_notification_mode(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32, PrivchatFfiError> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get(key).await?;
        let value = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("notification_mode").cloned())
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        Ok(value as i32)
    }

    pub async fn set_channel_favourite(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<(), PrivchatFfiError> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get(key.clone()).await?;
        let mut map: serde_json::Map<String, serde_json::Value> = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        map.insert("favourite".to_string(), serde_json::json!(enabled));
        self.kv_put(key, serde_json::Value::Object(map).to_string().into_bytes())
            .await
    }

    pub async fn set_channel_low_priority(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<(), PrivchatFfiError> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get(key.clone()).await?;
        let mut map: serde_json::Map<String, serde_json::Value> = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        map.insert("low_priority".to_string(), serde_json::json!(enabled));
        self.kv_put(key, serde_json::Value::Object(map).to_string().into_bytes())
            .await
    }

    pub async fn channel_tags(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Vec<String>, PrivchatFfiError> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get(key).await?;
        let tags = raw
            .and_then(|b| String::from_utf8(b).ok())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("tags").cloned())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        Ok(tags)
    }

    pub async fn upsert_user(&self, input: UpsertUserInput) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_user(map_upsert_user(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_user_by_id(
        &self,
        user_id: u64,
    ) -> Result<Option<StoredUser>, PrivchatFfiError> {
        let out = self
            .inner
            .get_user_by_id(user_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_user))
    }

    pub async fn list_users_by_ids(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<Vec<StoredUser>, PrivchatFfiError> {
        let out = self
            .inner
            .list_users_by_ids(user_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_user).collect())
    }

    pub async fn upsert_friend(&self, input: UpsertFriendInput) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_friend(map_upsert_friend(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_friends(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredFriend>, PrivchatFfiError> {
        let out = self
            .inner
            .list_friends(limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_friend).collect())
    }

    pub async fn get_friends(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredFriend>, PrivchatFfiError> {
        self.list_friends(limit, offset).await
    }

    pub async fn upsert_blacklist_entry(
        &self,
        input: UpsertBlacklistInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_blacklist_entry(map_upsert_blacklist(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn delete_blacklist_entry(&self, blocked_user_id: u64) -> Result<(), PrivchatFfiError> {
        self.inner
            .delete_blacklist_entry(blocked_user_id)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_blacklist_entries(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredBlacklistEntry>, PrivchatFfiError> {
        let out = self
            .inner
            .list_blacklist_entries(limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_blacklist).collect())
    }

    pub async fn upsert_group(&self, input: UpsertGroupInput) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_group(map_upsert_group(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_group_by_id(
        &self,
        group_id: u64,
    ) -> Result<Option<StoredGroup>, PrivchatFfiError> {
        let out = self
            .inner
            .get_group_by_id(group_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.map(map_stored_group))
    }

    pub async fn list_groups(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredGroup>, PrivchatFfiError> {
        let out = self
            .inner
            .list_groups(limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_group).collect())
    }

    pub async fn get_groups(
        &self,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredGroup>, PrivchatFfiError> {
        self.list_groups(limit, offset).await
    }

    pub async fn upsert_group_member(
        &self,
        input: UpsertGroupMemberInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_group_member(map_upsert_group_member(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_group_members(
        &self,
        group_id: u64,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredGroupMember>, PrivchatFfiError> {
        let out = self
            .inner
            .list_group_members(group_id, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_group_member).collect())
    }

    pub async fn get_group_members(
        &self,
        group_id: u64,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredGroupMember>, PrivchatFfiError> {
        self.list_group_members(group_id, limit, offset).await
    }

    pub async fn upsert_channel_member(
        &self,
        input: UpsertChannelMemberInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_channel_member(map_upsert_channel_member(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_channel_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredChannelMember>, PrivchatFfiError> {
        let out = self
            .inner
            .list_channel_members(channel_id, channel_type, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_channel_member).collect())
    }

    pub async fn delete_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .delete_channel_member(channel_id, channel_type, member_uid)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn remove_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<(), PrivchatFfiError> {
        self.delete_channel_member(channel_id, channel_type, member_uid)
            .await
    }

    pub async fn upsert_message_reaction(
        &self,
        input: UpsertMessageReactionInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_message_reaction(map_upsert_message_reaction(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_message_reactions(
        &self,
        message_id: u64,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredMessageReaction>, PrivchatFfiError> {
        let out = self
            .inner
            .list_message_reactions(message_id, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_message_reaction).collect())
    }

    pub async fn record_mention(&self, input: MentionInput) -> Result<u64, PrivchatFfiError> {
        self.inner
            .record_mention(map_mention_input(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_unread_mention_count(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32, PrivchatFfiError> {
        self.inner
            .get_unread_mention_count(channel_id, channel_type, user_id)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_unread_mention_message_ids(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: u64,
    ) -> Result<Vec<u64>, PrivchatFfiError> {
        self.inner
            .list_unread_mention_message_ids(channel_id, channel_type, user_id, limit as usize)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn mark_mention_read(
        &self,
        message_id: u64,
        user_id: u64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_mention_read(message_id, user_id)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn mark_all_mentions_read(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_all_mentions_read(channel_id, channel_type, user_id)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_all_unread_mention_counts(
        &self,
        user_id: u64,
    ) -> Result<Vec<UnreadMentionCount>, PrivchatFfiError> {
        let out = self
            .inner
            .get_all_unread_mention_counts(user_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_unread_mention_count).collect())
    }

    pub async fn upsert_reminder(
        &self,
        input: UpsertReminderInput,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_reminder(map_upsert_reminder(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn list_pending_reminders(
        &self,
        uid: u64,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredReminder>, PrivchatFfiError> {
        let out = self
            .inner
            .list_pending_reminders(uid, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_reminder).collect())
    }

    pub async fn mark_reminder_done(
        &self,
        reminder_id: u64,
        done: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_reminder_done(reminder_id, done)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn kv_put(&self, key: String, value: Vec<u8>) -> Result<(), PrivchatFfiError> {
        self.inner
            .kv_put(key, value)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn kv_get(&self, key: String) -> Result<Option<Vec<u8>>, PrivchatFfiError> {
        self.inner.kv_get(key).await.map_err(PrivchatFfiError::from)
    }

    pub async fn user_storage_paths(&self) -> Result<UserStoragePaths, PrivchatFfiError> {
        let out = self
            .inner
            .user_storage_paths()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(map_storage_paths(out))
    }

    pub async fn add_channel_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uids: Vec<u64>,
    ) -> Result<(), PrivchatFfiError> {
        for uid in member_uids {
            self.upsert_channel_member(UpsertChannelMemberInput {
                channel_id,
                channel_type,
                member_uid: uid,
                member_name: String::new(),
                member_remark: String::new(),
                member_avatar: String::new(),
                member_invite_uid: 0,
                role: 0,
                status: 0,
                is_deleted: false,
                robot: 0,
                version: 0,
                created_at: 0,
                updated_at: 0,
                extra: String::new(),
                forbidden_expiration_time: 0,
                member_avatar_cache_key: String::new(),
            })
            .await?;
        }
        Ok(())
    }

    pub fn builder(&self) -> String {
        "privchat-rust".to_string()
    }

    pub fn build(&self) -> String {
        self.builder()
    }

    pub async fn dm_peer_user_id(&self, channel_id: u64) -> Result<Option<u64>, PrivchatFfiError> {
        let me = self.current_user_id().await?;
        // Try common channel types and return the first non-self member.
        for channel_type in [1_i32, 2_i32, 3_i32] {
            let members = self
                .list_channel_members(channel_id, channel_type, 200, 0)
                .await?;
            if let Some(uid) = members
                .into_iter()
                .find(|m| m.member_uid != me && !m.is_deleted)
                .map(|m| m.member_uid)
            {
                return Ok(Some(uid));
            }
        }
        Ok(None)
    }

    pub async fn get_all_user_settings(&self) -> Result<String, PrivchatFfiError> {
        let raw = self.kv_get(USER_SETTINGS_KEY.to_string()).await?;
        if let Some(buf) = raw {
            let value = String::from_utf8(buf).map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("invalid user settings payload: {e}"),
            })?;
            let _: serde_json::Value =
                serde_json::from_str(&value).map_err(|e| PrivchatFfiError::SdkError {
                    code: privchat_protocol::ErrorCode::InternalError as u32,
                    detail: format!("invalid user settings json: {e}"),
                })?;
            return Ok(value);
        }
        Ok("{}".to_string())
    }

    pub async fn get_user_setting(&self, key: String) -> Result<Option<String>, PrivchatFfiError> {
        let all = self.get_all_user_settings().await?;
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&all)
            .map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("invalid settings json: {e}"),
            })?;
        Ok(map
            .get(&key)
            .map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())))
    }

    pub async fn set_user_setting(
        &self,
        key: String,
        value: String,
    ) -> Result<(), PrivchatFfiError> {
        let all = self.get_all_user_settings().await?;
        let mut map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&all)
            .map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("invalid settings json: {e}"),
            })?;
        map.insert(key, serde_json::Value::String(value));
        let payload = serde_json::Value::Object(map).to_string().into_bytes();
        self.kv_put(USER_SETTINGS_KEY.to_string(), payload).await
    }

    pub async fn get_presence_stats(&self) -> Result<String, PrivchatFfiError> {
        let friends = self.list_friends(500, 0).await?;
        let user_ids: Vec<u64> = friends.into_iter().map(|f| f.user_id).collect();
        if user_ids.is_empty() {
            return Ok("{\"online\":0,\"offline\":0,\"total\":0}".to_string());
        }
        let statuses = self.fetch_presence(user_ids).await?;
        let total = statuses.len() as u64;
        let online = statuses
            .iter()
            .filter(|s| s.status.eq_ignore_ascii_case("online"))
            .count() as u64;
        let offline = total.saturating_sub(online);
        Ok(serde_json::json!({
            "online": online,
            "offline": offline,
            "total": total
        })
        .to_string())
    }

    pub async fn get_typing_stats(&self) -> Result<String, PrivchatFfiError> {
        let active = self.typing_active_channels.lock().await;
        let active_count = active.len() as u64;
        let started_count = self.typing_started_count.load(Ordering::Relaxed);
        let stopped_count = self.typing_stopped_count.load(Ordering::Relaxed);
        let active_channels: Vec<serde_json::Value> = active
            .iter()
            .map(|(channel_id, channel_type)| {
                serde_json::json!({
                    "channel_id": channel_id,
                    "channel_type": channel_type
                })
            })
            .collect();
        Ok(serde_json::json!({
            "typing": active_count,
            "active_channels": active_channels,
            "started_count": started_count,
            "stopped_count": stopped_count
        })
        .to_string())
    }

    pub async fn join_group_by_qrcode(&self, qr_key: String) -> Result<String, PrivchatFfiError> {
        self.group_qrcode_join_remote(qr_key, None).await
    }

    pub async fn leave_channel(&self, channel_id: u64) -> Result<String, PrivchatFfiError> {
        self.hide_channel(channel_id).await
    }

    pub async fn list_my_devices(&self) -> Result<String, PrivchatFfiError> {
        let snapshot = self.session_snapshot().await?;
        let Some(s) = snapshot else {
            return Ok("[]".to_string());
        };
        Ok(serde_json::json!([{
            "device_id": s.device_id,
            "device_name": "current-device",
            "is_current": true,
            "app_in_background": self.app_in_background.load(Ordering::Relaxed)
        }])
        .to_string())
    }

    pub async fn mark_fully_read_at(
        &self,
        channel_id: u64,
        server_message_id: u64,
    ) -> Result<String, PrivchatFfiError> {
        // Keep remote read-receipt behavior compatible, and also best-effort
        // update local unread state for local-first UX.
        let out = self.mark_as_read(channel_id, server_message_id).await?;
        let _ = self.mark_channel_read(channel_id, 1).await;
        Ok(out)
    }

    pub fn on_connection_state_changed(&self) {
        self.on_connection_state_changed_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] on_connection_state_changed callback bridge active");
    }

    pub fn on_message_received(&self) {
        self.on_message_received_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] on_message_received callback bridge active");
    }

    pub fn on_reaction_changed(&self) {
        self.on_reaction_changed_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] on_reaction_changed callback bridge active");
    }

    pub fn on_typing_indicator(&self) {
        self.on_typing_indicator_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] on_typing_indicator callback bridge active");
    }

    pub async fn own_last_read(&self, channel_id: u64) -> Result<u64, PrivchatFfiError> {
        // Prefer channel extra browse_to as local "last read" cursor.
        if let Ok(Some(extra)) = self.get_channel_extra(channel_id, 1).await {
            if extra.browse_to > 0 {
                return Ok(extra.browse_to);
            }
        }
        // Fallback to the newest server message id in local messages.
        let list = self.list_messages(channel_id, 1, 1, 50).await?;
        Ok(list
            .into_iter()
            .filter_map(|m| m.server_message_id)
            .max()
            .unwrap_or(0))
    }

    pub async fn is_event_read_by(
        &self,
        server_message_id: u64,
        user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        let channel_id = self.resolve_channel_id_by_server_message_id(server_message_id).await?;
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        Ok(resp.readers.into_iter().any(|entry| {
            entry
                .get("user_id")
                .or_else(|| entry.get("uid"))
                .and_then(|v| v.as_u64())
                == Some(user_id)
        }))
    }

    pub async fn seen_by_for_event(&self, server_message_id: u64) -> Result<String, PrivchatFfiError> {
        let channel_id = self.resolve_channel_id_by_server_message_id(server_message_id).await?;
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        json_encode(&resp.readers, "seen_by_for_event response")
    }

    pub async fn paginate_back(
        &self,
        channel_id: u64,
        channel_type: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        self.list_messages(channel_id, channel_type, page, page_size).await
    }

    pub async fn paginate_forward(
        &self,
        channel_id: u64,
        channel_type: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        self.list_messages(channel_id, channel_type, page, page_size).await
    }

    pub async fn retry_message(&self, message_id: u64) -> Result<u64, PrivchatFfiError> {
        let msg = self
            .get_message_by_id(message_id)
            .await?
            .ok_or_else(|| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: format!("message not found: {message_id}"),
            })?;
        let payload = serde_json::json!({
            "message_id": msg.message_id,
            "channel_id": msg.channel_id,
            "channel_type": msg.channel_type,
            "from_uid": msg.from_uid,
            "message_type": msg.message_type,
            "content": msg.content,
            "extra": msg.extra,
        })
        .to_string()
        .into_bytes();
        self.enqueue_outbound_message(message_id, payload).await
    }

    pub async fn search_channel(&self, keyword: String) -> Result<Vec<StoredChannel>, PrivchatFfiError> {
        let needle = keyword.to_lowercase();
        let items = self.list_channels(1, 500).await?;
        Ok(items
            .into_iter()
            .filter(|c| {
                c.channel_name.to_lowercase().contains(&needle)
                    || c.channel_remark.to_lowercase().contains(&needle)
                    || c.last_msg_content.to_lowercase().contains(&needle)
            })
            .collect())
    }

    pub async fn search_messages(
        &self,
        channel_id: u64,
        channel_type: i32,
        keyword: String,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        let needle = keyword.to_lowercase();
        let items = self.list_messages(channel_id, channel_type, 1, 1000).await?;
        Ok(items
            .into_iter()
            .filter(|m| m.content.to_lowercase().contains(&needle))
            .collect())
    }

    pub async fn send_attachment_bytes(
        &self,
        message_id: u64,
        route_key: String,
        payload: Vec<u8>,
    ) -> Result<FileQueueRef, PrivchatFfiError> {
        self.enqueue_outbound_file(message_id, route_key, payload).await
    }

    pub async fn send_attachment_from_path(
        &self,
        message_id: u64,
        route_key: String,
        path: String,
    ) -> Result<FileQueueRef, PrivchatFfiError> {
        let payload = std::fs::read(path).map_err(|e| PrivchatFfiError::SdkError {
            code: privchat_protocol::ErrorCode::InternalError as u32,
            detail: format!("read attachment failed: {e}"),
        })?;
        self.enqueue_outbound_file(message_id, route_key, payload).await
    }

    pub async fn download_attachment_to_path(
        &self,
        source_path: String,
        target_path: String,
    ) -> Result<String, PrivchatFfiError> {
        let data = std::fs::read(&source_path).map_err(|e| PrivchatFfiError::SdkError {
            code: privchat_protocol::ErrorCode::InternalError as u32,
            detail: format!("read source attachment failed: {e}"),
        })?;
        if let Some(parent) = std::path::Path::new(&target_path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("create target dir failed: {e}"),
            })?;
        }
        std::fs::write(&target_path, data).map_err(|e| PrivchatFfiError::SdkError {
            code: privchat_protocol::ErrorCode::InternalError as u32,
            detail: format!("write target attachment failed: {e}"),
        })?;
        Ok(target_path)
    }

    pub async fn download_attachment_to_cache(
        &self,
        source_path: String,
        file_name: String,
    ) -> Result<String, PrivchatFfiError> {
        let base = self.assets_dir().await?;
        let target = format!("{base}/cache/{file_name}");
        self.download_attachment_to_path(source_path, target).await
    }

    pub async fn download_attachment_to_message_dir(
        &self,
        source_path: String,
        message_id: u64,
        file_name: String,
    ) -> Result<String, PrivchatFfiError> {
        let base = self.assets_dir().await?;
        let target = format!("{base}/messages/{message_id}/{file_name}");
        self.download_attachment_to_path(source_path, target).await
    }

    pub fn set_video_process_hook(&self) {
        self.video_process_hook_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] set_video_process_hook called (no-op in rust core)");
    }

    pub fn to_client_endpoint(&self) -> Option<String> {
        self.config()
            .endpoints
            .first()
            .map(|v| {
                let scheme = match v.protocol {
                    TransportProtocol::Quic => "quic",
                    TransportProtocol::Tcp => "tcp",
                    TransportProtocol::WebSocket => {
                        if v.use_tls { "wss" } else { "ws" }
                    }
                };
                let mut endpoint = format!("{scheme}://{}:{}", v.host, v.port);
                if let Some(path) = v.path.as_ref() {
                    if !path.is_empty() {
                        if path.starts_with('/') {
                            endpoint.push_str(path);
                        } else {
                            endpoint.push('/');
                            endpoint.push_str(path);
                        }
                    }
                }
                endpoint
            })
    }
}

#[uniffi::export]
pub fn sdk_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        parse_read_list_entries, parse_read_list_user_ids, PrivchatClient, PrivchatConfig,
        ServerEndpoint, SdkEvent, TransportProtocol,
    };

    #[test]
    fn parse_read_list_user_ids_supports_top_level_array() {
        let raw = r#"[{"user_id":1},{"uid":2},{"user_id":3}]"#;
        assert_eq!(parse_read_list_user_ids(raw), vec![1, 2, 3]);
    }

    #[test]
    fn parse_read_list_user_ids_supports_wrapped_shapes() {
        let raw_data = r#"{"data":[{"user_id":11}]}"#;
        let raw_items = r#"{"items":[{"uid":22}]}"#;
        let raw_nested = r#"{"result":{"items":[{"user_id":33}]}}"#;
        assert_eq!(parse_read_list_user_ids(raw_data), vec![11]);
        assert_eq!(parse_read_list_user_ids(raw_items), vec![22]);
        assert_eq!(parse_read_list_user_ids(raw_nested), vec![33]);
    }

    #[test]
    fn parse_read_list_entries_invalid_json_returns_empty() {
        let raw = "not-json";
        assert!(parse_read_list_entries(raw).is_empty());
        assert!(parse_read_list_user_ids(raw).is_empty());
    }

    fn test_config() -> PrivchatConfig {
        PrivchatConfig {
            endpoints: vec![ServerEndpoint {
                protocol: TransportProtocol::Tcp,
                host: "127.0.0.1".to_string(),
                port: 9001,
                path: None,
                use_tls: false,
            }],
            connection_timeout_secs: 1,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn event_replay_and_cursor_work() {
        let client = PrivchatClient::new(test_config()).expect("create client");
        client.shutdown().await.expect("shutdown should succeed");
        let cursor = client.event_stream_cursor();
        assert!(cursor > 0, "cursor should advance after shutdown events");

        let replay = client.events_since(0, 32);
        assert!(!replay.is_empty(), "replay should include lifecycle events");
        assert!(
            replay
                .iter()
                .any(|e| matches!(e.event, SdkEvent::ShutdownCompleted)),
            "replay should include ShutdownCompleted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn next_event_envelope_is_available() {
        let client = Arc::new(PrivchatClient::new(test_config()).expect("create client"));
        let client_for_shutdown = client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let _ = client_for_shutdown.shutdown().await;
        });
        let evt = client
            .next_event_envelope(200)
            .await
            .expect("poll should not fail");
        assert!(evt.is_some(), "envelope polling should return at least one event during shutdown");
    }
}

uniffi::setup_scaffolding!();

// Compatibility exports for Kotlin Multiplatform runtime templates.
// Our current API does not use pointer-returning Rust futures, but the generated
// runtime still references these symbols during linking.
#[repr(C)]
struct RustBuffer {
    capacity: i64,
    len: i64,
    data: *mut u8,
}

#[repr(C)]
struct UniffiRustCallStatus {
    code: i8,
    error_buf: RustBuffer,
}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_poll_pointer(
    _handle: i64,
    _callback: extern "C" fn(i64, i8),
    _callback_data: i64,
) {
}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_cancel_pointer(_handle: i64) {}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_free_pointer(_handle: i64) {}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_complete_pointer(
    _handle: i64,
    out_status: *mut UniffiRustCallStatus,
) -> *mut core::ffi::c_void {
    if !out_status.is_null() {
        // non-zero means call error in UniFFI status convention.
        unsafe {
            (*out_status).code = 1;
            (*out_status).error_buf = RustBuffer {
                capacity: 0,
                len: 0,
                data: core::ptr::null_mut(),
            };
        }
    }
    core::ptr::null_mut()
}
