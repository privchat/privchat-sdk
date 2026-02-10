use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use bytes::Bytes;
use msgtrans::protocol::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig};
use msgtrans::transport::client::{TransportClient, TransportClientBuilder};
use msgtrans::transport::TransportOptions;
use privchat_protocol::presence::{
    GetOnlineStatusRequest, GetOnlineStatusResponse, OnlineStatusInfo, SubscribePresenceRequest,
    SubscribePresenceResponse, TypingActionType as ProtoTypingActionType, TypingIndicatorRequest,
    UnsubscribePresenceRequest,
};
use privchat_protocol::rpc::auth::{AuthLoginRequest, AuthResponse, UserRegisterRequest};
use privchat_protocol::rpc::routes;
use privchat_protocol::{
    decode_message, encode_message, AuthType, AuthorizationRequest, AuthorizationResponse,
    ClientInfo, DeviceInfo, DeviceType, ErrorCode, MessageType, RpcRequest, RpcResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::timeout;

mod local_store;
mod runtime;
mod storage_actor;
mod task;
use runtime::runtime_provider::RuntimeProvider;
use storage_actor::StorageHandle;
use task::task_registry::TaskRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TransportProtocol {
    Quic,
    Tcp,
    WebSocket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEndpoint {
    pub protocol: TransportProtocol,
    pub host: String,
    pub port: u16,
    pub path: Option<String>,
    pub use_tls: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivchatConfig {
    pub endpoints: Vec<ServerEndpoint>,
    pub connection_timeout_secs: u64,
}

impl Default for PrivchatConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![ServerEndpoint {
                protocol: TransportProtocol::Tcp,
                host: "127.0.0.1".to_string(),
                port: 9001,
                path: None,
                use_tls: false,
            }],
            connection_timeout_secs: 10,
        }
    }
}

impl PrivchatConfig {
    pub fn from_server_urls<I, S>(urls: I, connection_timeout_secs: u64) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut endpoints = Vec::new();
        for url in urls {
            if let Some(endpoint) = parse_server_url(url.as_ref()) {
                endpoints.push(endpoint);
            }
        }
        if endpoints.is_empty() {
            return Self {
                connection_timeout_secs,
                ..Self::default()
            };
        }
        Self {
            endpoints,
            connection_timeout_secs,
        }
    }
}

fn parse_server_url(url: &str) -> Option<ServerEndpoint> {
    if url.starts_with("quic://") {
        parse_url_parts(url, "quic://", TransportProtocol::Quic, false)
    } else if url.starts_with("tcp://") {
        parse_url_parts(url, "tcp://", TransportProtocol::Tcp, false)
    } else if url.starts_with("ws://") {
        parse_url_parts(url, "ws://", TransportProtocol::WebSocket, false)
    } else if url.starts_with("wss://") {
        parse_url_parts(url, "wss://", TransportProtocol::WebSocket, true)
    } else {
        None
    }
}

fn parse_url_parts(
    url: &str,
    prefix: &str,
    protocol: TransportProtocol,
    use_tls: bool,
) -> Option<ServerEndpoint> {
    let remainder = url.strip_prefix(prefix)?;
    let (host_port, path) = if let Some(slash_pos) = remainder.find('/') {
        let host_port = &remainder[..slash_pos];
        let path = &remainder[slash_pos..];
        (host_port, Some(path.to_string()))
    } else {
        (remainder, None)
    };
    let (host, port) = parse_host_port(host_port)?;
    Some(ServerEndpoint {
        protocol,
        host,
        port,
        path,
        use_tls,
    })
}

fn parse_host_port(host_port: &str) -> Option<(String, u16)> {
    if let Some(colon_pos) = host_port.rfind(':') {
        let host = &host_port[..colon_pos];
        let port_str = &host_port[colon_pos + 1..];
        if let Ok(port) = port_str.parse::<u16>() {
            return Some((host.to_string(), port));
        }
    }
    Some((host_port.to_string(), 9001))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResult {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub refresh_token: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub bootstrap_completed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectionState {
    New,
    Connected,
    LoggedIn,
    Authenticated,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SequencedSdkEvent {
    pub sequence_id: u64,
    pub timestamp_ms: i64,
    pub event: SdkEvent,
}

const DEFAULT_EVENT_HISTORY_LIMIT: usize = 1024;
const USER_SETTINGS_KEY: &str = "__user_settings_json__";

fn emit_sequenced_event(
    event_tx: &broadcast::Sender<SdkEvent>,
    event_history: &StdMutex<VecDeque<SequencedSdkEvent>>,
    event_seq: &AtomicU64,
    history_limit: usize,
    event: SdkEvent,
) {
    let sequence_id = event_seq.fetch_add(1, Ordering::AcqRel) + 1;
    let envelope = SequencedSdkEvent {
        sequence_id,
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        event: event.clone(),
    };
    {
        let mut locked = event_history.lock().expect("event history poisoned");
        locked.push_back(envelope);
        while locked.len() > history_limit {
            let _ = locked.pop_front();
        }
    }
    let _ = event_tx.send(event);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueMessage {
    pub message_id: u64,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileQueueRef {
    pub queue_index: usize,
    pub message_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceStatus {
    pub user_id: u64,
    pub status: String,
    pub last_seen: i64,
    pub online_devices: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypingActionType {
    Typing,
    Recording,
    UploadingPhoto,
    UploadingVideo,
    UploadingFile,
    ChoosingSticker,
}

impl TypingActionType {
    fn into_proto(self) -> ProtoTypingActionType {
        match self {
            TypingActionType::Typing => ProtoTypingActionType::Typing,
            TypingActionType::Recording => ProtoTypingActionType::Recording,
            TypingActionType::UploadingPhoto => ProtoTypingActionType::UploadingPhoto,
            TypingActionType::UploadingVideo => ProtoTypingActionType::UploadingVideo,
            TypingActionType::UploadingFile => ProtoTypingActionType::UploadingFile,
            TypingActionType::ChoosingSticker => ProtoTypingActionType::ChoosingSticker,
        }
    }
}

fn map_presence_status(value: OnlineStatusInfo) -> PresenceStatus {
    PresenceStatus {
        user_id: value.user_id,
        status: value.status.as_str().to_string(),
        last_seen: value.last_seen,
        online_devices: value
            .online_devices
            .into_iter()
            .map(|d| d.as_str().to_string())
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertChannelExtraInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub browse_to: u64,
    pub keep_pts: u64,
    pub keep_offset_y: i32,
    pub draft: String,
    pub draft_updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertFriendInput {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFriend {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertBlacklistInput {
    pub blocked_user_id: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredBlacklistEntry {
    pub blocked_user_id: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertGroupInput {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGroup {
    pub group_id: u64,
    pub name: Option<String>,
    pub avatar: String,
    pub owner_id: Option<u64>,
    pub is_dismissed: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MentionInput {
    pub message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub mentioned_user_id: u64,
    pub sender_id: u64,
    pub is_mention_all: bool,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMention {
    pub id: u64,
    pub message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub mentioned_user_id: u64,
    pub sender_id: u64,
    pub is_mention_all: bool,
    pub created_at: i64,
    pub is_read: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadMentionCount {
    pub channel_id: u64,
    pub channel_type: i32,
    pub unread_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserStoragePaths {
    pub user_root: String,
    pub db_path: String,
    pub kv_path: String,
    pub queue_root: String,
    pub normal_queue_path: String,
    pub file_queue_paths: Vec<String>,
    pub media_root: String,
}

#[derive(thiserror::Error, Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("sdk not connected")]
    NotConnected,
    #[error("auth failed: {0}")]
    Auth(String),
    #[error("actor closed")]
    ActorClosed,
    #[error("sdk shutdown")]
    Shutdown,
    #[error("invalid state: {0}")]
    InvalidState(String),
}

impl Error {
    pub fn protocol_code(&self) -> u32 {
        match self {
            Error::Transport(_) => ErrorCode::NetworkError as u32,
            Error::Serialization(_) => ErrorCode::DecodingError as u32,
            Error::Storage(_) => ErrorCode::DatabaseError as u32,
            Error::NotConnected => ErrorCode::SessionNotFound as u32,
            Error::Auth(_) => ErrorCode::InvalidToken as u32,
            Error::ActorClosed => ErrorCode::SystemBusy as u32,
            Error::Shutdown => ErrorCode::ServiceUnavailable as u32,
            Error::InvalidState(_) => ErrorCode::OperationNotAllowed as u32,
        }
    }
}

type Result<T> = std::result::Result<T, Error>;

enum Command {
    Connect {
        resp: oneshot::Sender<Result<()>>,
    },
    Disconnect {
        resp: oneshot::Sender<Result<()>>,
    },
    IsConnected {
        resp: oneshot::Sender<Result<bool>>,
    },
    GetConnectionState {
        resp: oneshot::Sender<Result<ConnectionState>>,
    },
    Ping {
        resp: oneshot::Sender<Result<()>>,
    },
    Register {
        username: String,
        password: String,
        device_id: String,
        resp: oneshot::Sender<Result<LoginResult>>,
    },
    Login {
        username: String,
        password: String,
        device_id: String,
        resp: oneshot::Sender<Result<LoginResult>>,
    },
    Authenticate {
        user_id: u64,
        token: String,
        device_id: String,
        resp: oneshot::Sender<Result<()>>,
    },
    SyncEntities {
        entity_type: String,
        scope: Option<String>,
        resp: oneshot::Sender<Result<usize>>,
    },
    SyncChannel {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<usize>>,
    },
    SyncAllChannels {
        resp: oneshot::Sender<Result<usize>>,
    },
    SubscribePresence {
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<Vec<PresenceStatus>>>,
    },
    UnsubscribePresence {
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<()>>,
    },
    FetchPresence {
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<Vec<PresenceStatus>>>,
    },
    SendTyping {
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        action_type: TypingActionType,
        resp: oneshot::Sender<Result<()>>,
    },
    RpcCall {
        route: String,
        body_json: String,
        resp: oneshot::Sender<Result<String>>,
    },
    RunBootstrapSync {
        resp: oneshot::Sender<Result<()>>,
    },
    IsBootstrapCompleted {
        resp: oneshot::Sender<Result<bool>>,
    },
    GetSessionSnapshot {
        resp: oneshot::Sender<Result<Option<SessionSnapshot>>>,
    },
    ClearLocalState {
        resp: oneshot::Sender<Result<()>>,
    },
    EnqueueOutboundMessage {
        message_id: u64,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<u64>>,
    },
    PeekOutboundMessages {
        limit: usize,
        resp: oneshot::Sender<Result<Vec<QueueMessage>>>,
    },
    AckOutboundMessages {
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    EnqueueOutboundFile {
        message_id: u64,
        route_key: String,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<FileQueueRef>>,
    },
    PeekOutboundFiles {
        queue_index: usize,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<QueueMessage>>>,
    },
    AckOutboundFiles {
        queue_index: usize,
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    CreateLocalMessage {
        input: NewMessage,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetMessageById {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessage>>>,
    },
    ListMessages {
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessage>>>,
    },
    UpsertChannel {
        input: UpsertChannelInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelById {
        channel_id: u64,
        resp: oneshot::Sender<Result<Option<StoredChannel>>>,
    },
    ListChannels {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannel>>>,
    },
    UpsertChannelExtra {
        input: UpsertChannelExtraInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelExtra {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<Option<StoredChannelExtra>>>,
    },
    MarkMessageSent {
        message_id: u64,
        server_message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateMessageStatus {
        message_id: u64,
        status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRead {
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRevoke {
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
        resp: oneshot::Sender<Result<()>>,
    },
    EditMessage {
        message_id: u64,
        content: String,
        edited_at: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessagePinned {
        message_id: u64,
        is_pinned: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    GetMessageExtra {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessageExtra>>>,
    },
    MarkChannelRead {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelUnreadCount {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<i32>>,
    },
    GetTotalUnreadCount {
        exclude_muted: bool,
        resp: oneshot::Sender<Result<i32>>,
    },
    UpsertUser {
        input: UpsertUserInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetUserById {
        user_id: u64,
        resp: oneshot::Sender<Result<Option<StoredUser>>>,
    },
    ListUsersByIds {
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<Vec<StoredUser>>>,
    },
    UpsertFriend {
        input: UpsertFriendInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteFriend {
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListFriends {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredFriend>>>,
    },
    UpsertBlacklistEntry {
        input: UpsertBlacklistInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteBlacklistEntry {
        blocked_user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListBlacklistEntries {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredBlacklistEntry>>>,
    },
    UpsertGroup {
        input: UpsertGroupInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetGroupById {
        group_id: u64,
        resp: oneshot::Sender<Result<Option<StoredGroup>>>,
    },
    ListGroups {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroup>>>,
    },
    UpsertGroupMember {
        input: UpsertGroupMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteGroupMember {
        group_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListGroupMembers {
        group_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroupMember>>>,
    },
    UpsertChannelMember {
        input: UpsertChannelMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListChannelMembers {
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannelMember>>>,
    },
    DeleteChannelMember {
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpsertMessageReaction {
        input: UpsertMessageReactionInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListMessageReactions {
        message_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessageReaction>>>,
    },
    RecordMention {
        input: MentionInput,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetUnreadMentionCount {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<i32>>,
    },
    ListUnreadMentionMessageIds {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<u64>>>,
    },
    MarkMentionRead {
        message_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    MarkAllMentionsRead {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    GetAllUnreadMentionCounts {
        user_id: u64,
        resp: oneshot::Sender<Result<Vec<UnreadMentionCount>>>,
    },
    UpsertReminder {
        input: UpsertReminderInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListPendingReminders {
        uid: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredReminder>>>,
    },
    MarkReminderDone {
        reminder_id: u64,
        done: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    KvPut {
        key: String,
        value: Vec<u8>,
        resp: oneshot::Sender<Result<()>>,
    },
    KvGet {
        key: String,
        resp: oneshot::Sender<Result<Option<Vec<u8>>>>,
    },
    GetUserStoragePaths {
        resp: oneshot::Sender<Result<UserStoragePaths>>,
    },
    Shutdown {
        resp: oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SessionState {
    New,
    Connected,
    LoggedIn,
    Authenticated,
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Connect,
    Login,
    Authenticate,
    Shutdown,
}

impl SessionState {
    fn can(self, action: Action) -> std::result::Result<SessionState, Error> {
        match (self, action) {
            (SessionState::Shutdown, _) => Err(Error::Shutdown),
            (SessionState::New, Action::Connect) => Ok(SessionState::Connected),
            (SessionState::Connected, Action::Connect) => Ok(SessionState::Connected),
            (SessionState::LoggedIn, Action::Connect) => Ok(SessionState::Connected),
            (SessionState::Authenticated, Action::Connect) => Ok(SessionState::Connected),

            (SessionState::Connected, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::LoggedIn, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::Authenticated, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::New, Action::Login) => Err(Error::InvalidState(
                "login requires connect first".to_string(),
            )),

            (SessionState::LoggedIn, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::Authenticated, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::Connected, Action::Authenticate) => Err(Error::InvalidState(
                "authenticate requires login first".to_string(),
            )),
            (SessionState::New, Action::Authenticate) => Err(Error::InvalidState(
                "authenticate requires connect and login first".to_string(),
            )),

            (_, Action::Shutdown) => Ok(SessionState::Shutdown),
        }
    }

    fn as_connection_state(self) -> ConnectionState {
        match self {
            SessionState::New => ConnectionState::New,
            SessionState::Connected => ConnectionState::Connected,
            SessionState::LoggedIn => ConnectionState::LoggedIn,
            SessionState::Authenticated => ConnectionState::Authenticated,
            SessionState::Shutdown => ConnectionState::Shutdown,
        }
    }
}

struct State {
    config: PrivchatConfig,
    transport: Option<TransportClient>,
    session_state: SessionState,
    bootstrap_completed: bool,
    snowflake: Arc<snowflake_me::Snowflake>,
    storage: StorageHandle,
    current_uid: Option<String>,
}

impl State {
    fn json_get_u64(value: &serde_json::Value, keys: &[&str]) -> Option<u64> {
        for key in keys {
            if let Some(v) = value.get(*key) {
                if let Some(n) = v.as_u64() {
                    return Some(n);
                }
                if let Some(n) = v.as_i64() {
                    if n >= 0 {
                        return Some(n as u64);
                    }
                }
                if let Some(s) = v.as_str() {
                    if let Ok(n) = s.parse::<u64>() {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    fn json_get_i64(value: &serde_json::Value, keys: &[&str]) -> Option<i64> {
        for key in keys {
            if let Some(v) = value.get(*key) {
                if let Some(n) = v.as_i64() {
                    return Some(n);
                }
                if let Some(n) = v.as_u64() {
                    return Some(n as i64);
                }
                if let Some(s) = v.as_str() {
                    if let Ok(n) = s.parse::<i64>() {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    fn json_get_i32(value: &serde_json::Value, keys: &[&str]) -> Option<i32> {
        Self::json_get_i64(value, keys).map(|v| v as i32)
    }

    fn json_get_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
        for key in keys {
            if let Some(v) = value.get(*key) {
                if let Some(b) = v.as_bool() {
                    return Some(b);
                }
                if let Some(n) = v.as_i64() {
                    return Some(n != 0);
                }
                if let Some(s) = v.as_str() {
                    match s {
                        "1" | "true" | "TRUE" => return Some(true),
                        "0" | "false" | "FALSE" => return Some(false),
                        _ => {}
                    }
                }
            }
        }
        None
    }

    fn json_get_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
        for key in keys {
            if let Some(v) = value.get(*key) {
                if let Some(s) = v.as_str() {
                    return Some(s.to_string());
                }
                if !v.is_null() {
                    return Some(v.to_string().trim_matches('"').to_string());
                }
            }
        }
        None
    }

    fn parse_entity_id_u64(entity_id: &str) -> Option<u64> {
        if let Ok(v) = entity_id.parse::<u64>() {
            return Some(v);
        }
        for token in entity_id.split([':', '/', '|', ',']) {
            if let Ok(v) = token.trim().parse::<u64>() {
                return Some(v);
            }
        }
        None
    }

    fn parse_two_ids(entity_id: &str) -> Option<(u64, u64)> {
        let parts: Vec<u64> = entity_id
            .split([':', '/', '|', ','])
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();
        if parts.len() >= 2 {
            return Some((parts[0], parts[1]));
        }
        None
    }

    fn resolve_group_id_from_scope(scope: Option<&str>) -> Option<u64> {
        let scope = scope?;
        if let Ok(v) = scope.parse::<u64>() {
            return Some(v);
        }
        for token in scope.split([':', '/', '|', ',']) {
            if let Ok(v) = token.trim().parse::<u64>() {
                return Some(v);
            }
        }
        None
    }

    async fn apply_sync_entities(
        &mut self,
        entity_type: &str,
        scope: Option<&str>,
        items: &[privchat_protocol::rpc::sync::SyncEntityItem],
    ) -> Result<()> {
        let uid = self.current_uid.clone().ok_or_else(|| {
            Error::InvalidState("current user is not set; login/authenticate required".to_string())
        })?;
        let now_ms = chrono::Utc::now().timestamp_millis();

        match entity_type {
            "friend" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let user_id = Self::json_get_u64(&payload, &["user_id", "uid"])
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if user_id == 0 {
                        continue;
                    }
                    if item.deleted {
                        self.storage.delete_friend(uid.clone(), user_id).await?;
                        continue;
                    }
                    self.storage
                        .upsert_friend(
                            uid.clone(),
                            UpsertFriendInput {
                                user_id,
                                tags: Self::json_get_string(&payload, &["tags"]),
                                is_pinned: Self::json_get_bool(&payload, &["is_pinned", "pinned"])
                                    .unwrap_or(false),
                                created_at: Self::json_get_i64(&payload, &["created_at"])
                                    .unwrap_or(now_ms),
                                updated_at: Self::json_get_i64(
                                    &payload,
                                    &["updated_at", "version"],
                                )
                                .unwrap_or(item.version as i64),
                            },
                        )
                        .await?;
                }
            }
            "user_block" | "blacklist" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let blocked_user_id = Self::json_get_u64(
                        &payload,
                        &["blocked_user_id", "user_id", "uid"],
                    )
                    .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                    .unwrap_or(0);
                    if blocked_user_id == 0 {
                        continue;
                    }
                    if item.deleted {
                        self.storage
                            .delete_blacklist_entry(uid.clone(), blocked_user_id)
                            .await?;
                        continue;
                    }
                    self.storage
                        .upsert_blacklist_entry(
                            uid.clone(),
                            UpsertBlacklistInput {
                                blocked_user_id,
                                created_at: Self::json_get_i64(&payload, &["created_at"])
                                    .unwrap_or(now_ms),
                                updated_at: Self::json_get_i64(
                                    &payload,
                                    &["updated_at", "version"],
                                )
                                .unwrap_or(item.version as i64),
                            },
                        )
                        .await?;
                }
            }
            "user" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let user_id = Self::json_get_u64(&payload, &["user_id", "uid"])
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if user_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_user(
                            uid.clone(),
                            UpsertUserInput {
                                user_id,
                                username: Self::json_get_string(&payload, &["username"]),
                                nickname: Self::json_get_string(&payload, &["nickname", "name"]),
                                alias: Self::json_get_string(&payload, &["alias"]),
                                avatar: Self::json_get_string(&payload, &["avatar"])
                                    .unwrap_or_default(),
                                user_type: Self::json_get_i32(&payload, &["user_type", "type"])
                                    .unwrap_or(0),
                                is_deleted: item.deleted
                                    || Self::json_get_bool(&payload, &["is_deleted"])
                                        .unwrap_or(false),
                                channel_id: Self::json_get_string(&payload, &["channel_id"])
                                    .unwrap_or_default(),
                                updated_at: Self::json_get_i64(
                                    &payload,
                                    &["updated_at", "version"],
                                )
                                .unwrap_or(item.version as i64),
                            },
                        )
                        .await?;
                }
            }
            "group" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let group_id = Self::json_get_u64(&payload, &["group_id"])
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if group_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_group(
                            uid.clone(),
                            UpsertGroupInput {
                                group_id,
                                name: Self::json_get_string(&payload, &["name", "group_name"]),
                                avatar: Self::json_get_string(&payload, &["avatar"])
                                    .unwrap_or_default(),
                                owner_id: Self::json_get_u64(&payload, &["owner_id", "owner"]),
                                is_dismissed: item.deleted
                                    || Self::json_get_bool(&payload, &["is_dismissed"])
                                        .unwrap_or(false),
                                created_at: Self::json_get_i64(&payload, &["created_at"])
                                    .unwrap_or(now_ms),
                                updated_at: Self::json_get_i64(
                                    &payload,
                                    &["updated_at", "version"],
                                )
                                .unwrap_or(item.version as i64),
                            },
                        )
                        .await?;
                }
            }
            "group_member" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let group_id = Self::json_get_u64(&payload, &["group_id"])
                        .or_else(|| Self::resolve_group_id_from_scope(scope))
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.0))
                        .unwrap_or(0);
                    let user_id = Self::json_get_u64(&payload, &["user_id", "uid"])
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.1))
                        .unwrap_or(0);
                    if group_id == 0 || user_id == 0 {
                        continue;
                    }
                    if item.deleted {
                        self.storage
                            .delete_group_member(uid.clone(), group_id, user_id)
                            .await?;
                        continue;
                    }
                    self.storage
                        .upsert_group_member(
                            uid.clone(),
                            UpsertGroupMemberInput {
                                group_id,
                                user_id,
                                role: Self::json_get_i32(&payload, &["role"]).unwrap_or(2),
                                status: Self::json_get_i32(&payload, &["status"]).unwrap_or(0),
                                alias: Self::json_get_string(&payload, &["alias"]),
                                is_muted: Self::json_get_bool(&payload, &["is_muted"])
                                    .unwrap_or(false),
                                joined_at: Self::json_get_i64(&payload, &["joined_at"])
                                    .unwrap_or(now_ms),
                                updated_at: Self::json_get_i64(
                                    &payload,
                                    &["updated_at", "version"],
                                )
                                .unwrap_or(item.version as i64),
                            },
                        )
                        .await?;
                }
            }
            "channel" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if channel_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_channel(
                            uid.clone(),
                            UpsertChannelInput {
                                channel_id,
                                channel_type: Self::json_get_i32(
                                    &payload,
                                    &["channel_type", "type"],
                                )
                                .unwrap_or(0),
                                channel_name: Self::json_get_string(
                                    &payload,
                                    &["channel_name", "name"],
                                )
                                .unwrap_or_default(),
                                channel_remark: Self::json_get_string(&payload, &["channel_remark"])
                                    .unwrap_or_default(),
                                avatar: Self::json_get_string(&payload, &["avatar"])
                                    .unwrap_or_default(),
                                unread_count: Self::json_get_i32(&payload, &["unread_count"])
                                    .unwrap_or(0),
                                top: Self::json_get_i32(&payload, &["top"]).unwrap_or(0),
                                mute: Self::json_get_i32(&payload, &["mute"]).unwrap_or(0),
                                last_msg_timestamp: Self::json_get_i64(
                                    &payload,
                                    &["last_msg_timestamp", "updated_at"],
                                )
                                .unwrap_or(now_ms),
                                last_local_message_id: 0,
                                last_msg_content: Self::json_get_string(
                                    &payload,
                                    &["last_msg_content"],
                                )
                                .unwrap_or_default(),
                            },
                        )
                        .await?;
                }
            }
            "channel_member" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.0))
                        .unwrap_or(0);
                    let member_uid = Self::json_get_u64(&payload, &["member_uid", "user_id", "uid"])
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.1))
                        .unwrap_or(0);
                    if channel_id == 0 || member_uid == 0 {
                        continue;
                    }
                    let channel_type = Self::json_get_i32(&payload, &["channel_type"]).unwrap_or(0);
                    if item.deleted {
                        self.storage
                            .delete_channel_member(uid.clone(), channel_id, channel_type, member_uid)
                            .await?;
                        continue;
                    }
                    self.storage
                        .upsert_channel_member(
                            uid.clone(),
                            UpsertChannelMemberInput {
                                channel_id,
                                channel_type,
                                member_uid,
                                member_name: Self::json_get_string(&payload, &["member_name", "name"])
                                    .unwrap_or_default(),
                                member_remark: Self::json_get_string(
                                    &payload,
                                    &["member_remark", "remark"],
                                )
                                .unwrap_or_default(),
                                member_avatar: Self::json_get_string(
                                    &payload,
                                    &["member_avatar", "avatar"],
                                )
                                .unwrap_or_default(),
                                member_invite_uid: Self::json_get_u64(
                                    &payload,
                                    &["member_invite_uid", "inviter_uid"],
                                )
                                .unwrap_or(0),
                                role: Self::json_get_i32(&payload, &["role"]).unwrap_or(0),
                                status: Self::json_get_i32(&payload, &["status"]).unwrap_or(0),
                                is_deleted: false,
                                robot: Self::json_get_i32(&payload, &["robot"]).unwrap_or(0),
                                version: Self::json_get_i64(&payload, &["version"])
                                    .unwrap_or(item.version as i64),
                                created_at: Self::json_get_i64(&payload, &["created_at"])
                                    .unwrap_or(now_ms),
                                updated_at: Self::json_get_i64(&payload, &["updated_at"])
                                    .unwrap_or(now_ms),
                                extra: Self::json_get_string(&payload, &["extra"])
                                    .unwrap_or_default(),
                                forbidden_expiration_time: Self::json_get_i64(
                                    &payload,
                                    &["forbidden_expiration_time"],
                                )
                                .unwrap_or(0),
                                member_avatar_cache_key: Self::json_get_string(
                                    &payload,
                                    &["member_avatar_cache_key"],
                                )
                                .unwrap_or_default(),
                            },
                        )
                        .await?;
                }
            }
            "user_settings" => {
                let payload = serde_json::json!({
                    "items": items,
                    "updated_at": now_ms
                })
                .to_string()
                .into_bytes();
                self.storage
                    .kv_put(uid, USER_SETTINGS_KEY.to_string(), payload)
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }

    fn current_uid_required(&self) -> Result<String> {
        let uid = self.current_uid.clone().ok_or_else(|| {
            Error::InvalidState("current user is not set; login/authenticate required".to_string())
        })?;
        if !self.bootstrap_completed {
            return Err(Error::InvalidState(
                "run_bootstrap_sync required before local-first operations".to_string(),
            ));
        }
        Ok(uid)
    }

    async fn resolve_target(host: &str, port: u16) -> Result<String> {
        let direct = format!("{host}:{port}");
        if direct.parse::<std::net::SocketAddr>().is_ok() {
            return Ok(direct);
        }
        let mut addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|e| Error::Transport(format!("dns resolve failed: {e}")))?;
        let addr = addrs
            .next()
            .ok_or_else(|| Error::Transport(format!("dns resolve empty: {host}")))?;
        Ok(addr.to_string())
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.connection_timeout_secs.max(1))
    }

    fn next_local_message_id(&self) -> Result<u64> {
        self.snowflake
            .next_id()
            .map_err(|e| Error::Storage(format!("generate local_message_id failed: {e:?}")))
    }

    fn connect_timeout_total(&self) -> Duration {
        let per = self.config.connection_timeout_secs.max(1);
        let endpoints = self.config.endpoints.len().max(1) as u64;
        // Allow sequential fallback across endpoints plus a small scheduling buffer.
        Duration::from_secs(per.saturating_mul(endpoints).saturating_add(2))
    }

    async fn connect(&mut self) -> Result<()> {
        eprintln!(
            "[SDK.actor] connect: enter (has_transport={}, endpoints={})",
            self.transport.is_some(),
            self.config.endpoints.len()
        );
        if let Some(transport) = self.transport.as_ref() {
            if transport.is_connected().await {
                eprintln!("[SDK.actor] connect: already connected");
                return Ok(());
            }
            eprintln!("[SDK.actor] connect: found stale transport, rebuilding");
            self.transport = None;
        }
        let endpoints = self.config.endpoints.clone();
        let mut last_err = None;
        for ep in endpoints {
            match ep.protocol {
                TransportProtocol::WebSocket => {
                    eprintln!(
                        "[SDK.actor] connect: trying {:?} {}:{} tls={}",
                        ep.protocol, ep.host, ep.port, ep.use_tls
                    );
                }
                _ => {
                    eprintln!(
                        "[SDK.actor] connect: trying {:?} {}:{}",
                        ep.protocol, ep.host, ep.port
                    );
                }
            }
            match timeout(self.timeout(), self.connect_one(&ep)).await {
                Ok(Ok(c)) => {
                    self.transport = Some(c);
                    eprintln!("[SDK.actor] connect: success");
                    return Ok(());
                }
                Ok(Err(e)) => {
                    eprintln!("[SDK.actor] connect: endpoint failed: {e}");
                    last_err = Some(e);
                }
                Err(_) => {
                    let e = Error::Transport(format!(
                        "endpoint {:?} {}:{} timeout",
                        ep.protocol, ep.host, ep.port
                    ));
                    eprintln!("[SDK.actor] connect: endpoint failed: {e}");
                    last_err = Some(e);
                }
            }
        }
        eprintln!("[SDK.actor] connect: all endpoints failed");
        Err(last_err.unwrap_or_else(|| Error::Transport("no endpoint".into())))
    }

    async fn disconnect(&mut self) -> Result<()> {
        if let Some(transport) = self.transport.as_ref() {
            transport
                .disconnect()
                .await
                .map_err(|e| Error::Transport(format!("disconnect: {e}")))?;
        }
        self.transport = None;
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        match self.transport.as_ref() {
            Some(transport) => transport.is_connected().await,
            None => false,
        }
    }

    async fn connect_one(&self, ep: &ServerEndpoint) -> Result<TransportClient> {
        eprintln!("[SDK.actor] connect_one: begin");
        let timeout = self.timeout();
        let target = Self::resolve_target(&ep.host, ep.port).await?;
        let mut client = match ep.protocol {
            TransportProtocol::Quic => {
                let cfg = QuicClientConfig::new(&target)
                    .map_err(|e| Error::Transport(format!("quic config: {e}")))?
                    .with_connect_timeout(timeout);
                TransportClientBuilder::new()
                    .with_protocol(cfg)
                    .connect_timeout(timeout)
                    .build()
                    .await
                    .map_err(|e| Error::Transport(format!("quic build: {e}")))?
            }
            TransportProtocol::Tcp => {
                let cfg = TcpClientConfig::new(&target)
                    .map_err(|e| Error::Transport(format!("tcp config: {e}")))?
                    .with_connect_timeout(timeout);
                TransportClientBuilder::new()
                    .with_protocol(cfg)
                    .connect_timeout(timeout)
                    .build()
                    .await
                    .map_err(|e| Error::Transport(format!("tcp build: {e}")))?
            }
            TransportProtocol::WebSocket => {
                let path = ep.path.as_deref().unwrap_or("/");
                let url = if ep.use_tls {
                    format!("wss://{}:{}{}", ep.host, ep.port, path)
                } else {
                    format!("ws://{}:{}{}", ep.host, ep.port, path)
                };
                let cfg = WebSocketClientConfig::new(&url)
                    .map_err(|e| Error::Transport(format!("ws config: {e}")))?
                    .with_connect_timeout(timeout)
                    .with_verify_tls(ep.use_tls);
                TransportClientBuilder::new()
                    .with_protocol(cfg)
                    .connect_timeout(timeout)
                    .build()
                    .await
                    .map_err(|e| Error::Transport(format!("ws build: {e}")))?
            }
        };
        client
            .connect()
            .await
            .map_err(|e| Error::Transport(format!("connect: {e}")))?;
        eprintln!("[SDK.actor] connect_one: connected");
        Ok(client)
    }

    async fn login(
        &mut self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        eprintln!("[SDK.actor] login: enter");
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = RpcRequest {
            route: "account/auth/login".to_string(),
            body: serde_json::to_value(AuthLoginRequest {
                username,
                password,
                device_id: device_id.clone(),
                device_info: None,
            })
            .map_err(|e| Error::Serialization(format!("encode login body: {e}")))?,
        };

        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode rpc request: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc login: {e}")))?;

        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode rpc response: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty rpc data".into()))?;
        let auth: AuthResponse = serde_json::from_value(body)
            .map_err(|e| Error::Serialization(format!("decode auth response: {e}")))?;

        let out = LoginResult {
            user_id: auth.user_id,
            token: auth.token,
            device_id: auth.device_id,
            refresh_token: auth.refresh_token,
            expires_at: auth.expires_at,
        };
        let uid = out.user_id.to_string();
        self.storage.save_login(uid.clone(), out.clone()).await?;
        self.current_uid = Some(uid);
        eprintln!("[SDK.actor] login: success user_id={}", out.user_id);
        Ok(out)
    }

    async fn register(
        &mut self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = RpcRequest {
            route: routes::account_user::REGISTER.to_string(),
            body: serde_json::to_value(UserRegisterRequest {
                username,
                password,
                nickname: None,
                phone: None,
                email: None,
                device_id: device_id.clone(),
                device_info: None,
            })
            .map_err(|e| Error::Serialization(format!("encode register body: {e}")))?,
        };
        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode register request: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc register: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode register response: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty register data".into()))?;
        let auth: AuthResponse = serde_json::from_value(body)
            .map_err(|e| Error::Serialization(format!("decode register auth response: {e}")))?;
        let out = LoginResult {
            user_id: auth.user_id,
            token: auth.token,
            device_id: auth.device_id,
            refresh_token: auth.refresh_token,
            expires_at: auth.expires_at,
        };
        let uid = out.user_id.to_string();
        self.storage.save_login(uid.clone(), out.clone()).await?;
        self.current_uid = Some(uid);
        Ok(out)
    }

    async fn authenticate(&mut self, user_id: u64, token: String, device_id: String) -> Result<()> {
        eprintln!("[SDK.actor] authenticate: enter user_id={user_id}");
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let token_for_persist = token.clone();
        let device_id_for_persist = device_id.clone();
        let req = AuthorizationRequest {
            auth_type: AuthType::JWT,
            auth_token: token,
            client_info: ClientInfo {
                client_type: "privchat-rust".to_string(),
                version: "0.1.0".to_string(),
                os: std::env::consts::OS.to_string(),
                os_version: std::env::consts::OS.to_string(),
                device_model: None,
                app_package: Some("io.privchat.rust".to_string()),
            },
            device_info: DeviceInfo {
                device_id,
                device_type: DeviceType::from_str(std::env::consts::OS),
                app_id: "io.privchat.rust".to_string(),
                push_token: None,
                push_channel: None,
                device_name: "privchat-rust".to_string(),
                device_model: None,
                os_version: Some(std::env::consts::OS.to_string()),
                app_version: Some("0.1.0".to_string()),
                manufacturer: None,
                device_fingerprint: None,
            },
            protocol_version: "1.0".to_string(),
            properties: HashMap::from([
                ("user_id".to_string(), user_id.to_string()),
                (
                    "client_timestamp".to_string(),
                    chrono::Utc::now().timestamp_millis().to_string(),
                ),
            ]),
        };
        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode auth request: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::AuthorizationRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("auth request: {e}")))?;
        let auth_resp: AuthorizationResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode auth response: {e}")))?;
        if !auth_resp.success {
            return Err(Error::Auth(
                auth_resp
                    .error_message
                    .unwrap_or_else(|| "authorization failed".to_string()),
            ));
        }
        let uid = user_id.to_string();
        self.storage
            .save_login(
                uid.clone(),
                LoginResult {
                    user_id,
                    token: token_for_persist,
                    device_id: device_id_for_persist,
                    refresh_token: None,
                    expires_at: String::new(),
                },
            )
            .await?;
        self.current_uid = Some(uid);
        eprintln!("[SDK.actor] authenticate: success");
        Ok(())
    }

    async fn sync_entities(&mut self, entity_type: String, scope: Option<String>) -> Result<usize> {
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let entity_type_for_apply = entity_type.clone();
        let scope_for_apply = scope.clone();
        let req = privchat_protocol::rpc::sync::SyncEntitiesRequest {
            entity_type,
            since_version: Some(0),
            scope,
            limit: Some(200),
        };
        let request = RpcRequest {
            route: privchat_protocol::rpc::routes::entity::SYNC_ENTITIES.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode sync_entities body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode sync_entities rpc: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc sync_entities: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode sync_entities rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty sync_entities data".into()))?;
        let response: privchat_protocol::rpc::sync::SyncEntitiesResponse =
            serde_json::from_value(body)
                .map_err(|e| Error::Serialization(format!("decode sync_entities response: {e}")))?;
        self.apply_sync_entities(
            entity_type_for_apply.as_str(),
            scope_for_apply.as_deref(),
            &response.items,
        )
        .await?;
        Ok(response.items.len())
    }

    async fn run_bootstrap_sync(&mut self) -> Result<()> {
        if self.session_state != SessionState::Authenticated {
            return Err(Error::InvalidState(
                "run_bootstrap_sync requires authenticated state".to_string(),
            ));
        }

        // Keep order aligned with legacy SDK bootstrap strategy.
        let order = ["friend", "group", "channel", "user_settings"];
        for entity_type in order {
            let _ = self.sync_entities(entity_type.to_string(), None).await?;
        }
        self.bootstrap_completed = true;
        if let Some(uid) = &self.current_uid {
            self.storage
                .set_bootstrap_completed(uid.clone(), true)
                .await?;
        }
        Ok(())
    }

    async fn sync_channel(&mut self, channel_id: u64, channel_type: i32) -> Result<usize> {
        // Keep compatibility with legacy API surface:
        // channel-targeted sync is represented as a scoped entity sync.
        let scope = Some(format!("{channel_type}:{channel_id}"));
        self.sync_entities("channel".to_string(), scope).await
    }

    async fn sync_all_channels(&mut self) -> Result<usize> {
        self.sync_entities("channel".to_string(), None).await
    }

    async fn subscribe_presence(&mut self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = SubscribePresenceRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::SUBSCRIBE.to_string(),
            body: serde_json::to_value(req).map_err(|e| {
                Error::Serialization(format!("encode subscribe_presence body: {e}"))
            })?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode subscribe_presence rpc: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc subscribe_presence: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode subscribe_presence rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty subscribe_presence data".into()))?;
        let response: SubscribePresenceResponse = serde_json::from_value(body).map_err(|e| {
            Error::Serialization(format!("decode subscribe_presence response: {e}"))
        })?;
        let mut out: Vec<PresenceStatus> = response
            .initial_statuses
            .into_values()
            .map(map_presence_status)
            .collect();
        out.sort_by_key(|v| v.user_id);
        Ok(out)
    }

    async fn unsubscribe_presence(&mut self, user_ids: Vec<u64>) -> Result<()> {
        if user_ids.is_empty() {
            return Ok(());
        }
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = UnsubscribePresenceRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::UNSUBSCRIBE.to_string(),
            body: serde_json::to_value(req).map_err(|e| {
                Error::Serialization(format!("encode unsubscribe_presence body: {e}"))
            })?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode unsubscribe_presence rpc: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc unsubscribe_presence: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode unsubscribe_presence rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        Ok(())
    }

    async fn fetch_presence(&mut self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = GetOnlineStatusRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::STATUS_GET.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode fetch_presence body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode fetch_presence rpc: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc fetch_presence: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode fetch_presence rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty fetch_presence data".into()))?;
        let response: GetOnlineStatusResponse = serde_json::from_value(body)
            .map_err(|e| Error::Serialization(format!("decode fetch_presence response: {e}")))?;
        let mut out: Vec<PresenceStatus> = response
            .statuses
            .into_values()
            .map(map_presence_status)
            .collect();
        out.sort_by_key(|v| v.user_id);
        Ok(out)
    }

    async fn send_typing(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        action_type: TypingActionType,
    ) -> Result<()> {
        let channel_type = u8::try_from(channel_type)
            .map_err(|_| Error::InvalidState(format!("invalid channel_type: {channel_type}")))?;
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let req = TypingIndicatorRequest {
            channel_id,
            channel_type,
            is_typing,
            action_type: action_type.into_proto(),
        };
        let request = RpcRequest {
            route: routes::presence::TYPING.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode send_typing body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode send_typing rpc: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc send_typing: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode send_typing rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        Ok(())
    }

    async fn rpc_call_json(
        &mut self,
        route: String,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let timeout = self.timeout();
        let transport = self.transport.as_mut().ok_or(Error::NotConnected)?;
        let request = RpcRequest { route, body };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode rpc request: {e}")))?;
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::RpcRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(payload), opt)
            .await
            .map_err(|e| Error::Transport(format!("rpc call failed: {e}")))?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode rpc response: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        Ok(rpc_resp.data.unwrap_or(serde_json::Value::Null))
    }
}

#[derive(Clone)]
pub struct PrivchatSdk {
    tx: mpsc::Sender<Command>,
    event_tx: broadcast::Sender<SdkEvent>,
    event_seq: Arc<AtomicU64>,
    event_history: Arc<StdMutex<VecDeque<SequencedSdkEvent>>>,
    event_history_limit: usize,
    _runtime_provider: RuntimeProvider,
    task_registry: TaskRegistry,
    shutting_down: Arc<AtomicBool>,
    supervised_sync_running: Arc<AtomicBool>,
}

impl PrivchatSdk {
    pub fn new(config: PrivchatConfig) -> Self {
        Self::with_runtime(config, RuntimeProvider::new_owned())
    }

    pub fn with_runtime(config: PrivchatConfig, runtime_provider: RuntimeProvider) -> Self {
        let (tx, mut rx) = mpsc::channel::<Command>(64);
        let (event_tx, _) = broadcast::channel::<SdkEvent>(256);
        let actor_event_tx = event_tx.clone();
        let event_seq = Arc::new(AtomicU64::new(0));
        let actor_event_seq = event_seq.clone();
        let event_history = Arc::new(StdMutex::new(VecDeque::new()));
        let actor_event_history = event_history.clone();
        let event_history_limit = DEFAULT_EVENT_HISTORY_LIMIT;
        let task_registry = TaskRegistry::new();
        let actor_task = runtime_provider.spawn(async move {
            eprintln!("[SDK.actor] loop: started");
            let storage = match StorageHandle::start() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[SDK.actor] storage init failed: {e}");
                    return;
                }
            };
            let machine_id: u16 = (std::process::id() as u16) & 0x1f;
            let data_center_id: u16 = (chrono::Utc::now().timestamp_millis() as u16) & 0x1f;
            let snowflake = match snowflake_me::Snowflake::builder()
                .machine_id(&|| Ok(machine_id))
                .data_center_id(&|| Ok(data_center_id))
                .finalize()
            {
                Ok(v) => Arc::new(v),
                Err(e) => {
                    eprintln!("[SDK.actor] snowflake init failed: {e:?}");
                    return;
                }
            };
            let current_uid = storage.load_current_uid().await.ok().flatten();
            let saved = if let Some(uid) = &current_uid {
                storage.load_session(uid.clone()).await.ok().flatten()
            } else {
                None
            };
            let mut state = State {
                config,
                transport: None,
                session_state: SessionState::New,
                bootstrap_completed: saved
                    .as_ref()
                    .map(|s| s.bootstrap_completed)
                    .unwrap_or(false),
                snowflake,
                storage: storage.clone(),
                current_uid,
            };
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    Command::Connect { resp } => {
                        eprintln!("[SDK.actor] loop: cmd connect");
                        let from_state = state.session_state.as_connection_state();
                        let result = match state.session_state.can(Action::Connect) {
                            Ok(next_state) => {
                                match timeout(state.connect_timeout_total(), state.connect()).await
                                {
                                    Ok(r) => {
                                        if r.is_ok() {
                                            state.session_state = next_state;
                                        }
                                        r
                                    }
                                    Err(_) => Err(Error::Transport("connect timeout".to_string())),
                                }
                            }
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::Disconnect { resp } => {
                        eprintln!("[SDK.actor] loop: cmd disconnect");
                        let from_state = state.session_state.as_connection_state();
                        let result = state.disconnect().await;
                        if result.is_ok() && state.session_state != SessionState::Shutdown {
                            state.session_state = SessionState::New;
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::IsConnected { resp } => {
                        let _ = resp.send(Ok(state.is_connected().await));
                    }
                    Command::GetConnectionState { resp } => {
                        let _ = resp.send(Ok(state.session_state.as_connection_state()));
                    }
                    Command::Ping { resp } => {
                        let result = if state.is_connected().await {
                            Ok(())
                        } else {
                            Err(Error::NotConnected)
                        };
                        let _ = resp.send(result);
                    }
                    Command::Login {
                        username,
                        password,
                        device_id,
                        resp,
                    } => {
                        eprintln!("[SDK.actor] loop: cmd login");
                        let from_state = state.session_state.as_connection_state();
                        let result = match state.session_state.can(Action::Login) {
                            Ok(next_state) => match timeout(
                                Duration::from_secs(20),
                                state.login(username, password, device_id),
                            )
                            .await
                            {
                                Ok(r) => {
                                    if r.is_ok() {
                                        state.session_state = next_state;
                                    }
                                    r
                                }
                                Err(_) => Err(Error::Transport("login timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::Register {
                        username,
                        password,
                        device_id,
                        resp,
                    } => {
                        eprintln!("[SDK.actor] loop: cmd register");
                        let from_state = state.session_state.as_connection_state();
                        let result = match state.session_state.can(Action::Login) {
                            Ok(next_state) => match timeout(
                                Duration::from_secs(20),
                                state.register(username, password, device_id),
                            )
                            .await
                            {
                                Ok(r) => {
                                    if r.is_ok() {
                                        state.session_state = next_state;
                                    }
                                    r
                                }
                                Err(_) => Err(Error::Transport("register timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::Authenticate {
                        user_id,
                        token,
                        device_id,
                        resp,
                    } => {
                        eprintln!("[SDK.actor] loop: cmd authenticate");
                        let from_state = state.session_state.as_connection_state();
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(next_state) => match timeout(
                                Duration::from_secs(20),
                                state.authenticate(user_id, token, device_id),
                            )
                            .await
                            {
                                Ok(r) => {
                                    if r.is_ok() {
                                        state.session_state = next_state;
                                    }
                                    r
                                }
                                Err(_) => Err(Error::Transport("authenticate timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::SyncEntities {
                        entity_type,
                        scope,
                        resp,
                    } => {
                        eprintln!("[SDK.actor] loop: cmd sync_entities");
                        let result = if state.session_state != SessionState::Authenticated {
                            Err(Error::InvalidState(
                                "sync_entities requires authenticated state".to_string(),
                            ))
                        } else {
                            match timeout(
                                Duration::from_secs(30),
                                state.sync_entities(entity_type, scope),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => {
                                    Err(Error::Transport("sync_entities timeout".to_string()))
                                }
                            }
                        };
                        let _ = resp.send(result);
                    }
                    Command::SyncChannel {
                        channel_id,
                        channel_type,
                        resp,
                    } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(30),
                                state.sync_channel(channel_id, channel_type),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(Error::Transport("sync_channel timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SyncAllChannels { resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => {
                                match timeout(Duration::from_secs(30), state.sync_all_channels())
                                    .await
                                {
                                    Ok(r) => r,
                                    Err(_) => Err(Error::Transport(
                                        "sync_all_channels timeout".to_string(),
                                    )),
                                }
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SubscribePresence { user_ids, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(15),
                                state.subscribe_presence(user_ids),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => {
                                    Err(Error::Transport("subscribe_presence timeout".to_string()))
                                }
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UnsubscribePresence { user_ids, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(15),
                                state.unsubscribe_presence(user_ids),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(Error::Transport(
                                    "unsubscribe_presence timeout".to_string(),
                                )),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::FetchPresence { user_ids, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(15),
                                state.fetch_presence(user_ids),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => {
                                    Err(Error::Transport("fetch_presence timeout".to_string()))
                                }
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SendTyping {
                        channel_id,
                        channel_type,
                        is_typing,
                        action_type,
                        resp,
                    } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(10),
                                state.send_typing(channel_id, channel_type, is_typing, action_type),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(Error::Transport("send_typing timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RpcCall {
                        route,
                        body_json,
                        resp,
                    } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => {
                                let parsed_body = serde_json::from_str::<serde_json::Value>(
                                    &body_json,
                                )
                                .map_err(|e| {
                                    Error::Serialization(format!("parse rpc body json: {e}"))
                                });
                                match parsed_body {
                                    Ok(body) => match timeout(
                                        Duration::from_secs(20),
                                        state.rpc_call_json(route, body),
                                    )
                                    .await
                                    {
                                        Ok(Ok(value)) => {
                                            serde_json::to_string(&value).map_err(|e| {
                                                Error::Serialization(format!(
                                                    "encode rpc response json: {e}"
                                                ))
                                            })
                                        }
                                        Ok(Err(e)) => Err(e),
                                        Err(_) => {
                                            Err(Error::Transport("rpc_call timeout".to_string()))
                                        }
                                    },
                                    Err(e) => Err(e),
                                }
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RunBootstrapSync { resp } => {
                        eprintln!("[SDK.actor] loop: cmd run_bootstrap_sync");
                        let result = match timeout(
                            Duration::from_secs(60),
                            state.run_bootstrap_sync(),
                        )
                        .await
                        {
                            Ok(r) => r,
                            Err(_) => {
                                Err(Error::Transport("run_bootstrap_sync timeout".to_string()))
                            }
                        };
                        if result.is_ok() {
                            if let Some(uid) = &state.current_uid {
                                if let Ok(user_id) = uid.parse::<u64>() {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::BootstrapCompleted { user_id },
                                    );
                                }
                            }
                        }
                        let _ = resp.send(result);
                    }
                    Command::IsBootstrapCompleted { resp } => {
                        let _ = resp.send(Ok(state.bootstrap_completed));
                    }
                    Command::GetSessionSnapshot { resp } => {
                        let result = if let Some(uid) = &state.current_uid {
                            state.storage.load_session(uid.clone()).await
                        } else {
                            Ok(None)
                        };
                        let _ = resp.send(result);
                    }
                    Command::ClearLocalState { resp } => {
                        let from_state = state.session_state.as_connection_state();
                        state.bootstrap_completed = false;
                        state.session_state = SessionState::Connected;
                        let result = if let Some(uid) = &state.current_uid {
                            let clear = state.storage.clear_session(uid.clone()).await;
                            let clear_uid = state.storage.clear_current_uid().await;
                            state.current_uid = None;
                            clear.and(clear_uid)
                        } else {
                            Ok(())
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged {
                                    from: from_state,
                                    to: state.session_state.as_connection_state(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::EnqueueOutboundMessage {
                        message_id,
                        payload,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => match state.next_local_message_id() {
                                Ok(local_message_id) => {
                                    match state
                                        .storage
                                        .update_local_message_id(
                                            uid.clone(),
                                            message_id,
                                            local_message_id,
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            state
                                                .storage
                                                .normal_queue_push(uid, message_id, payload)
                                                .await
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(e) => Err(e),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::PeekOutboundMessages { limit, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .normal_queue_peek(uid, limit)
                                    .await
                                    .map(|items| {
                                        items
                                            .into_iter()
                                            .map(|(message_id, payload)| QueueMessage {
                                                message_id,
                                                payload,
                                            })
                                            .collect()
                                    })
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::AckOutboundMessages { message_ids, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.normal_queue_ack(uid, message_ids).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::EnqueueOutboundFile {
                        message_id,
                        route_key,
                        payload,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => match state.next_local_message_id() {
                                Ok(local_message_id) => {
                                    match state
                                        .storage
                                        .update_local_message_id(
                                            uid.clone(),
                                            message_id,
                                            local_message_id,
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            let q = state
                                                .storage
                                                .select_file_queue(uid.clone(), route_key)
                                                .await;
                                            match q {
                                                Ok(queue_index) => state
                                                    .storage
                                                    .file_queue_push(
                                                        uid,
                                                        queue_index,
                                                        message_id,
                                                        payload,
                                                    )
                                                    .await
                                                    .map(|message_id| FileQueueRef {
                                                        queue_index,
                                                        message_id,
                                                    }),
                                                Err(e) => Err(e),
                                            }
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(e) => Err(e),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::PeekOutboundFiles {
                        queue_index,
                        limit,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state
                                .storage
                                .file_queue_peek(uid, queue_index, limit)
                                .await
                                .map(|items| {
                                    items
                                        .into_iter()
                                        .map(|(message_id, payload)| QueueMessage {
                                            message_id,
                                            payload,
                                        })
                                        .collect()
                                }),
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::AckOutboundFiles {
                        queue_index,
                        message_ids,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .file_queue_ack(uid, queue_index, message_ids)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::CreateLocalMessage { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.create_local_message(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetMessageById { message_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_message_by_id(uid, message_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListMessages {
                        channel_id,
                        channel_type,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_messages(uid, channel_id, channel_type, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannel { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_channel(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetChannelById { channel_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_channel_by_id(uid, channel_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListChannels {
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.list_channels(uid, limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannelExtra { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_channel_extra(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetChannelExtra {
                        channel_id,
                        channel_type,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .get_channel_extra(uid, channel_id, channel_type)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::MarkMessageSent {
                        message_id,
                        server_message_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .mark_message_sent(uid, message_id, server_message_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpdateMessageStatus {
                        message_id,
                        status,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .update_message_status(uid, message_id, status)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SetMessageRead {
                        message_id,
                        channel_id,
                        channel_type,
                        is_read,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .set_message_read(
                                        uid,
                                        message_id,
                                        channel_id,
                                        channel_type,
                                        is_read,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SetMessageRevoke {
                        message_id,
                        revoked,
                        revoker,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .set_message_revoke(uid, message_id, revoked, revoker)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::EditMessage {
                        message_id,
                        content,
                        edited_at,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .edit_message(uid, message_id, &content, edited_at)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SetMessagePinned {
                        message_id,
                        is_pinned,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .set_message_pinned(uid, message_id, is_pinned)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetMessageExtra { message_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_message_extra(uid, message_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::MarkChannelRead {
                        channel_id,
                        channel_type,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .mark_channel_read(uid, channel_id, channel_type)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetChannelUnreadCount {
                        channel_id,
                        channel_type,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .get_channel_unread_count(uid, channel_id, channel_type)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetTotalUnreadCount {
                        exclude_muted,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .get_total_unread_count(uid, exclude_muted)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertUser { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_user(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetUserById { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_user_by_id(uid, user_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListUsersByIds { user_ids, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.list_users_by_ids(uid, user_ids).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertFriend { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_friend(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteFriend { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.delete_friend(uid, user_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListFriends {
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.list_friends(uid, limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertBlacklistEntry { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_blacklist_entry(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteBlacklistEntry {
                        blocked_user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.delete_blacklist_entry(uid, blocked_user_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListBlacklistEntries {
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.list_blacklist_entries(uid, limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertGroup { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_group(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetGroupById { group_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_group_by_id(uid, group_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListGroups {
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.list_groups(uid, limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertGroupMember { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_group_member(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteGroupMember {
                        group_id,
                        user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.delete_group_member(uid, group_id, user_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListGroupMembers {
                        group_id,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_group_members(uid, group_id, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannelMember { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_channel_member(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListChannelMembers {
                        channel_id,
                        channel_type,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_channel_members(
                                        uid,
                                        channel_id,
                                        channel_type,
                                        limit,
                                        offset,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteChannelMember {
                        channel_id,
                        channel_type,
                        member_uid,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .delete_channel_member(
                                        uid,
                                        channel_id,
                                        channel_type,
                                        member_uid,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertMessageReaction { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_message_reaction(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListMessageReactions {
                        message_id,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_message_reactions(uid, message_id, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RecordMention { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.record_mention(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetUnreadMentionCount {
                        channel_id,
                        channel_type,
                        user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .get_unread_mention_count(
                                        uid,
                                        channel_id,
                                        channel_type,
                                        user_id,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListUnreadMentionMessageIds {
                        channel_id,
                        channel_type,
                        user_id,
                        limit,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_unread_mention_message_ids(
                                        uid,
                                        channel_id,
                                        channel_type,
                                        user_id,
                                        limit,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::MarkMentionRead {
                        message_id,
                        user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .mark_mention_read(uid, message_id, user_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::MarkAllMentionsRead {
                        channel_id,
                        channel_type,
                        user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .mark_all_mentions_read(uid, channel_id, channel_type, user_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetAllUnreadMentionCounts { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .get_all_unread_mention_counts(uid, user_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertReminder { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.upsert_reminder(uid, input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListPendingReminders {
                        uid: reminder_uid,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .list_pending_reminders(uid, reminder_uid, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::MarkReminderDone {
                        reminder_id,
                        done,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => {
                                state
                                    .storage
                                    .mark_reminder_done(uid, reminder_id, done)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::KvPut { key, value, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.kv_put(uid, key, value).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::KvGet { key, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.kv_get(uid, key).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetUserStoragePaths { resp } => {
                        let result = match state.current_uid_required() {
                            Ok(uid) => state.storage.get_storage_paths(uid).await.map(|paths| {
                                UserStoragePaths {
                                    user_root: paths.user_root.display().to_string(),
                                    db_path: paths.db_path.display().to_string(),
                                    kv_path: paths.kv_path.display().to_string(),
                                    queue_root: paths.queue_root.display().to_string(),
                                    normal_queue_path: paths
                                        .normal_queue_path
                                        .display()
                                        .to_string(),
                                    file_queue_paths: paths
                                        .file_queue_paths
                                        .iter()
                                        .map(|v| v.display().to_string())
                                        .collect(),
                                    media_root: paths.media_root.display().to_string(),
                                }
                            }),
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::Shutdown { resp } => {
                        eprintln!("[SDK.actor] loop: cmd shutdown");
                        emit_sequenced_event(
                            &actor_event_tx,
                            &actor_event_history,
                            &actor_event_seq,
                            event_history_limit,
                            SdkEvent::ShutdownStarted,
                        );
                        let from_state = state.session_state.as_connection_state();
                        if let Ok(next_state) = state.session_state.can(Action::Shutdown) {
                            state.session_state = next_state;
                        }
                        emit_sequenced_event(
                            &actor_event_tx,
                            &actor_event_history,
                            &actor_event_seq,
                            event_history_limit,
                            SdkEvent::ConnectionStateChanged {
                                from: from_state,
                                to: state.session_state.as_connection_state(),
                            },
                        );
                        state.transport = None;
                        state.storage.shutdown();
                        emit_sequenced_event(
                            &actor_event_tx,
                            &actor_event_history,
                            &actor_event_seq,
                            event_history_limit,
                            SdkEvent::ShutdownCompleted,
                        );
                        let _ = resp.send(());
                        break;
                    }
                }
            }
            eprintln!("[SDK.actor] loop: receiver closed");
        });
        task_registry.track(actor_task);

        Self {
            tx,
            event_tx,
            event_seq,
            event_history,
            event_history_limit,
            _runtime_provider: runtime_provider,
            task_registry,
            shutting_down: Arc::new(AtomicBool::new(false)),
            supervised_sync_running: Arc::new(AtomicBool::new(false)),
        }
    }

    fn ensure_running(&self) -> Result<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(Error::Shutdown);
        }
        Ok(())
    }

    pub async fn connect(&self) -> Result<()> {
        self.ensure_running()?;
        eprintln!("[SDK.api] connect: send");
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Connect { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        let out = resp_rx.await.map_err(|_| Error::ActorClosed)?;
        eprintln!("[SDK.api] connect: recv");
        out
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Disconnect { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn is_connected(&self) -> Result<bool> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::IsConnected { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn connection_state(&self) -> Result<ConnectionState> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetConnectionState { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<SdkEvent> {
        self.event_tx.subscribe()
    }

    pub fn last_event_sequence_id(&self) -> u64 {
        self.event_seq.load(Ordering::Acquire)
    }

    pub fn event_history_limit(&self) -> usize {
        self.event_history_limit
    }

    pub fn recent_events(&self, limit: usize) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .rev()
            .take(capped)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn events_since(&self, from_sequence_id: u64, limit: usize) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .filter(|evt| evt.sequence_id > from_sequence_id)
            .take(capped)
            .cloned()
            .collect()
    }

    pub async fn ping(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Ping { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn login(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        self.ensure_running()?;
        eprintln!("[SDK.api] login: send");
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Login {
                username,
                password,
                device_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        let out = resp_rx.await.map_err(|_| Error::ActorClosed)?;
        eprintln!("[SDK.api] login: recv");
        out
    }

    pub async fn register(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Register {
                username,
                password,
                device_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn authenticate(&self, user_id: u64, token: String, device_id: String) -> Result<()> {
        self.ensure_running()?;
        eprintln!("[SDK.api] authenticate: send");
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Authenticate {
                user_id,
                token,
                device_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        let out = resp_rx.await.map_err(|_| Error::ActorClosed)?;
        eprintln!("[SDK.api] authenticate: recv");
        out
    }

    pub async fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.supervised_sync_running.store(false, Ordering::Release);
        let (resp_tx, resp_rx) = oneshot::channel();
        let _ = self.tx.send(Command::Shutdown { resp: resp_tx }).await;
        let _ = resp_rx.await;
        self.task_registry.shutdown().await;
    }

    pub async fn sync_entities(&self, entity_type: String, scope: Option<String>) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SyncEntities {
                entity_type,
                scope,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn sync_channel(&self, channel_id: u64, channel_type: i32) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SyncChannel {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn sync_all_channels(&self) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SyncAllChannels { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn subscribe_presence(&self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SubscribePresence {
                user_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn unsubscribe_presence(&self, user_ids: Vec<u64>) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UnsubscribePresence {
                user_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn fetch_presence(&self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::FetchPresence {
                user_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn send_typing(
        &self,
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        action_type: TypingActionType,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SendTyping {
                channel_id,
                channel_type,
                is_typing,
                action_type,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn rpc_call(&self, route: String, body_json: String) -> Result<String> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RpcCall {
                route,
                body_json,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn is_bootstrap_completed(&self) -> Result<bool> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::IsBootstrapCompleted { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn run_bootstrap_sync(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RunBootstrapSync { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn session_snapshot(&self) -> Result<Option<SessionSnapshot>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetSessionSnapshot { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn clear_local_state(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ClearLocalState { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub fn is_supervised_sync_running(&self) -> bool {
        self.supervised_sync_running.load(Ordering::Acquire)
    }

    pub fn start_supervised_sync(&self, interval_secs: u64) -> Result<()> {
        self.ensure_running()?;
        if self
            .supervised_sync_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::InvalidState(
                "supervised sync already running".to_string(),
            ));
        }

        let sdk = self.clone();
        let running = self.supervised_sync_running.clone();
        let interval = Duration::from_secs(interval_secs.max(5));
        let handle = self._runtime_provider.spawn(async move {
            while running.load(Ordering::Acquire) && !sdk.shutting_down.load(Ordering::Acquire) {
                let _ = sdk.sync_all_channels().await;
                tokio::time::sleep(interval).await;
            }
            running.store(false, Ordering::Release);
        });
        let _ = self.task_registry.track(handle);
        Ok(())
    }

    pub fn stop_supervised_sync(&self) {
        self.supervised_sync_running.store(false, Ordering::Release);
    }

    pub async fn enqueue_outbound_message(&self, message_id: u64, payload: Vec<u8>) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EnqueueOutboundMessage {
                message_id,
                payload,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn peek_outbound_messages(&self, limit: usize) -> Result<Vec<QueueMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::PeekOutboundMessages {
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn ack_outbound_messages(&self, message_ids: Vec<u64>) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::AckOutboundMessages {
                message_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn enqueue_outbound_file(
        &self,
        message_id: u64,
        route_key: String,
        payload: Vec<u8>,
    ) -> Result<FileQueueRef> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EnqueueOutboundFile {
                message_id,
                route_key,
                payload,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn peek_outbound_files(
        &self,
        queue_index: usize,
        limit: usize,
    ) -> Result<Vec<QueueMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::PeekOutboundFiles {
                queue_index,
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn ack_outbound_files(
        &self,
        queue_index: usize,
        message_ids: Vec<u64>,
    ) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::AckOutboundFiles {
                queue_index,
                message_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn create_local_message(&self, input: NewMessage) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::CreateLocalMessage {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_by_id(&self, message_id: u64) -> Result<Option<StoredMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetMessageById {
                message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_messages(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListMessages {
                channel_id,
                channel_type,
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel(&self, input: UpsertChannelInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertChannel {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_by_id(&self, channel_id: u64) -> Result<Option<StoredChannel>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetChannelById {
                channel_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channels(&self, limit: usize, offset: usize) -> Result<Vec<StoredChannel>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListChannels {
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_extra(&self, input: UpsertChannelExtraInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertChannelExtra {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_extra(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<StoredChannelExtra>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetChannelExtra {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_message_sent(&self, message_id: u64, server_message_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkMessageSent {
                message_id,
                server_message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn update_message_status(&self, message_id: u64, status: i32) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpdateMessageStatus {
                message_id,
                status,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_read(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetMessageRead {
                message_id,
                channel_id,
                channel_type,
                is_read,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_revoke(
        &self,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetMessageRevoke {
                message_id,
                revoked,
                revoker,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn edit_message(
        &self,
        message_id: u64,
        content: String,
        edited_at: i32,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EditMessage {
                message_id,
                content,
                edited_at,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_pinned(&self, message_id: u64, is_pinned: bool) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetMessagePinned {
                message_id,
                is_pinned,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_extra(&self, message_id: u64) -> Result<Option<StoredMessageExtra>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetMessageExtra {
                message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_channel_read(&self, channel_id: u64, channel_type: i32) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkChannelRead {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_unread_count(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetChannelUnreadCount {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_total_unread_count(&self, exclude_muted: bool) -> Result<i32> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetTotalUnreadCount {
                exclude_muted,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_user(&self, input: UpsertUserInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertUser {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_user_by_id(&self, user_id: u64) -> Result<Option<StoredUser>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetUserById {
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_users_by_ids(&self, user_ids: Vec<u64>) -> Result<Vec<StoredUser>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListUsersByIds {
                user_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_friend(&self, input: UpsertFriendInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertFriend {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_friend(&self, user_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteFriend {
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_friends(&self, limit: usize, offset: usize) -> Result<Vec<StoredFriend>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListFriends {
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_blacklist_entry(&self, input: UpsertBlacklistInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertBlacklistEntry {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_blacklist_entry(&self, blocked_user_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteBlacklistEntry {
                blocked_user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_blacklist_entries(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredBlacklistEntry>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListBlacklistEntries {
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group(&self, input: UpsertGroupInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertGroup {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_group_by_id(&self, group_id: u64) -> Result<Option<StoredGroup>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetGroupById {
                group_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_groups(&self, limit: usize, offset: usize) -> Result<Vec<StoredGroup>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListGroups {
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group_member(&self, input: UpsertGroupMemberInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertGroupMember {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_member(&self, input: UpsertChannelMemberInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertChannelMember {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channel_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannelMember>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListChannelMembers {
                channel_id,
                channel_type,
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteChannelMember {
                channel_id,
                channel_type,
                member_uid,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_group_members(
        &self,
        group_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroupMember>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListGroupMembers {
                group_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_group_member(&self, group_id: u64, user_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteGroupMember {
                group_id,
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_message_reaction(&self, input: UpsertMessageReactionInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertMessageReaction {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_message_reactions(
        &self,
        message_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessageReaction>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListMessageReactions {
                message_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn record_mention(&self, input: MentionInput) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RecordMention {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_unread_mention_count(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetUnreadMentionCount {
                channel_id,
                channel_type,
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_unread_mention_message_ids(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
    ) -> Result<Vec<u64>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListUnreadMentionMessageIds {
                channel_id,
                channel_type,
                user_id,
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_mention_read(&self, message_id: u64, user_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkMentionRead {
                message_id,
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_all_mentions_read(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkAllMentionsRead {
                channel_id,
                channel_type,
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_all_unread_mention_counts(
        &self,
        user_id: u64,
    ) -> Result<Vec<UnreadMentionCount>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetAllUnreadMentionCounts {
                user_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_reminder(&self, input: UpsertReminderInput) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpsertReminder {
                input,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_pending_reminders(
        &self,
        uid: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredReminder>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListPendingReminders {
                uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_reminder_done(&self, reminder_id: u64, done: bool) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkReminderDone {
                reminder_id,
                done,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_put(&self, key: String, value: Vec<u8>) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::KvPut {
                key,
                value,
                resp: resp_tx,
            })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_get(&self, key: String) -> Result<Option<Vec<u8>>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::KvGet { key, resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn user_storage_paths(&self) -> Result<UserStoragePaths> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetUserStoragePaths { resp: resp_tx })
            .await
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, ConnectionState, Error, PrivchatConfig, SessionState, SdkEvent};

    #[test]
    fn state_machine_enforces_legacy_flow() {
        assert!(matches!(
            SessionState::New.can(Action::Login),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            SessionState::Connected.can(Action::Authenticate),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            SessionState::LoggedIn.can(Action::Authenticate),
            Ok(SessionState::Authenticated)
        ));
    }

    #[test]
    fn shutdown_blocks_all_actions() {
        assert!(matches!(
            SessionState::Shutdown.can(Action::Connect),
            Err(Error::Shutdown)
        ));
        assert!(matches!(
            SessionState::Shutdown.can(Action::Login),
            Err(Error::Shutdown)
        ));
        assert!(matches!(
            SessionState::Shutdown.can(Action::Authenticate),
            Err(Error::Shutdown)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_emits_events() {
        let sdk = super::PrivchatSdk::new(PrivchatConfig::default());
        let mut rx = sdk.subscribe_events();
        sdk.shutdown().await;

        let mut got_started = false;
        let mut got_completed = false;
        let mut got_state_change = false;
        for _ in 0..8 {
            let next = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv()).await;
            let Ok(Ok(evt)) = next else {
                break;
            };
            match evt {
                SdkEvent::ShutdownStarted => got_started = true,
                SdkEvent::ShutdownCompleted => got_completed = true,
                SdkEvent::ConnectionStateChanged { to, .. } => {
                    if to == ConnectionState::Shutdown {
                        got_state_change = true;
                    }
                }
                _ => {}
            }
            if got_started && got_completed && got_state_change {
                break;
            }
        }

        assert!(got_started, "missing ShutdownStarted event");
        assert!(got_state_change, "missing ConnectionStateChanged(... -> Shutdown)");
        assert!(got_completed, "missing ShutdownCompleted event");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn event_history_replay_is_ordered() {
        let sdk = super::PrivchatSdk::new(PrivchatConfig::default());
        sdk.shutdown().await;

        let events = sdk.recent_events(16);
        assert!(events.len() >= 3, "expected shutdown events in replay");
        for w in events.windows(2) {
            assert!(
                w[0].sequence_id < w[1].sequence_id,
                "event sequence should be strictly increasing"
            );
        }
        assert!(
            events.iter().any(|e| matches!(e.event, SdkEvent::ShutdownStarted)),
            "replay missing ShutdownStarted"
        );
        assert!(
            events.iter().any(|e| matches!(e.event, SdkEvent::ShutdownCompleted)),
            "replay missing ShutdownCompleted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_is_idempotent_and_rejects_new_work() {
        let sdk = super::PrivchatSdk::new(PrivchatConfig::default());
        sdk.shutdown().await;
        sdk.shutdown().await;

        let err = sdk.connect().await.expect_err("connect should fail after shutdown");
        assert!(matches!(err, Error::Shutdown));
    }
}
