use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use bytes::Bytes;
use msgtrans::protocol::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig};
use msgtrans::transport::client::{TransportClient, TransportClientBuilder};
use msgtrans::transport::TransportOptions;
use msgtrans::ClientEvent;
use privchat_protocol::presence::{
    GetOnlineStatusRequest, GetOnlineStatusResponse, OnlineStatusInfo, SubscribePresenceRequest,
    SubscribePresenceResponse, TypingActionType as ProtoTypingActionType, TypingIndicatorRequest,
    UnsubscribePresenceRequest,
};
use privchat_protocol::rpc::auth::{AuthLoginRequest, AuthResponse, UserRegisterRequest};
use privchat_protocol::rpc::file::upload::{
    FileRequestUploadTokenRequest, FileRequestUploadTokenResponse, FileUploadCallbackRequest,
};
use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::sync::SyncEntityItem;
use privchat_protocol::rpc::sync::{ClientSubmitRequest, ClientSubmitResponse, ServerDecision};
use privchat_protocol::{
    decode_message, encode_message, AuthType, AuthorizationRequest, AuthorizationResponse,
    ClientInfo, ContentMessageType, DeviceInfo, DeviceType, ErrorCode, MessageType, PingRequest,
    PongResponse, PushBatchRequest, PushMessageRequest, RpcRequest, RpcResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{interval, timeout, MissedTickBehavior};

mod local_store;
pub mod error_codes;
mod receive_pipeline;
mod runtime;
mod storage_actor;
mod sync_commit_applier;
mod task;
use receive_pipeline::ReceivePipeline;
use runtime::runtime_provider::RuntimeProvider;
use storage_actor::StorageHandle;
use sync_commit_applier::SyncCommitApplier;
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
    pub data_dir: String,
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
            data_dir: String::new(),
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
                data_dir: String::new(),
                ..Self::default()
            };
        }
        Self {
            endpoints,
            connection_timeout_secs,
            data_dir: String::new(),
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum NetworkHint {
    Unknown,
    Offline,
    Wifi,
    Cellular,
    Ethernet,
}

impl NetworkHint {
    fn is_online(self) -> bool {
        !matches!(self, NetworkHint::Offline)
    }
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
    SyncEntitiesApplied {
        entity_type: String,
        scope: Option<String>,
        queued: usize,
        applied: usize,
        dropped_duplicates: usize,
    },
    SyncEntityChanged {
        entity_type: String,
        entity_id: String,
        deleted: bool,
    },
    SyncChannelApplied {
        channel_id: u64,
        channel_type: i32,
        applied: usize,
    },
    SyncAllChannelsApplied {
        applied: usize,
    },
    NetworkHintChanged {
        from: NetworkHint,
        to: NetworkHint,
    },
    OutboundQueueUpdated {
        kind: String,
        action: String,
        message_id: Option<u64>,
        queue_index: Option<usize>,
    },
    TimelineUpdated {
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        reason: String,
    },
    ReadReceiptUpdated {
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        is_read: bool,
    },
    MessageSendStatusChanged {
        message_id: u64,
        status: i32,
        server_message_id: Option<u64>,
    },
    TypingSent {
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
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
const USER_SETTINGS_ITEM_PREFIX: &str = "entity_sync:user_settings:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaProcessOp {
    Thumbnail,
    Compress,
}

pub type VideoProcessHook = Arc<
    dyn Fn(
            MediaProcessOp,
            &std::path::Path,
            &std::path::Path,
            &std::path::Path,
        ) -> std::result::Result<bool, String>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
struct UploadedFileInfo {
    file_id: String,
    storage_source_id: u32,
}

#[derive(Debug, Clone, Serialize)]
struct MediaMeta {
    source: MediaSourceMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail: Option<MediaThumbnailMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    processing: Option<MediaProcessingMeta>,
}

#[derive(Debug, Clone, Serialize)]
struct MediaSourceMeta {
    original_filename: String,
    mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct MediaThumbnailMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MediaProcessingMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<i64>,
}

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

async fn stop_inbound_task(task: &mut Option<tokio::task::JoinHandle<()>>) {
    if let Some(handle) = task.take() {
        handle.abort();
        let _ = handle.await;
    }
}

async fn start_inbound_task(
    state: &State,
    actor_tx: mpsc::Sender<Command>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    stop_inbound_task(task).await;
    let Some(transport) = state.transport.as_ref() else {
        return;
    };
    let mut event_rx = transport.subscribe_events();
    *task = Some(tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            match event {
                ClientEvent::MessageReceived(context) => {
                    if actor_tx
                        .send(Command::InboundFrame {
                            biz_type: context.biz_type,
                            data: context.data.clone(),
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                ClientEvent::Disconnected { .. } => {
                    let _ = actor_tx.send(Command::InboundDisconnected).await;
                    break;
                }
                _ => {}
            }
        }
    }));
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
pub struct UpsertRemoteMessageInput {
    pub server_message_id: u64,
    pub local_message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub timestamp: i64,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub pts: i64,
    pub setting: i32,
    pub order_seq: i64,
    pub searchable_word: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAccountSummary {
    pub uid: String,
    pub created_at: i64,
    pub last_login_at: i64,
    pub is_active: bool,
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
    pub fn sdk_code(&self) -> u32 {
        match self {
            Error::Transport(_) => error_codes::TRANSPORT_FAILURE,
            Error::Serialization(_) => error_codes::SERIALIZATION_FAILURE,
            Error::Storage(_) => error_codes::STORAGE_FAILURE,
            Error::NotConnected => error_codes::NETWORK_DISCONNECTED,
            Error::Auth(_) => error_codes::AUTH_FAILURE,
            Error::ActorClosed => error_codes::ACTOR_CLOSED,
            Error::Shutdown => error_codes::SHUTDOWN,
            Error::InvalidState(_) => error_codes::INVALID_STATE,
        }
    }

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
const NETWORK_DISCONNECTED_MESSAGE: &str = "网络已断开，请检查网络连接后再试。";
const OUTBOUND_DRAIN_BATCH_SIZE: usize = 20;

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
    SetNetworkHint {
        hint: NetworkHint,
        resp: oneshot::Sender<Result<()>>,
    },
    InboundFrame {
        biz_type: u8,
        data: Vec<u8>,
    },
    InboundDisconnected,
    SetVideoProcessHook {
        hook: Option<VideoProcessHook>,
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
    KvScanPrefix {
        prefix: String,
        resp: oneshot::Sender<Result<Vec<(String, Vec<u8>)>>>,
    },
    GetUserStoragePaths {
        resp: oneshot::Sender<Result<UserStoragePaths>>,
    },
    ListLocalAccounts {
        resp: oneshot::Sender<Result<Vec<LocalAccountSummary>>>,
    },
    SetCurrentUid {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    WipeCurrentUserFull {
        resp: oneshot::Sender<Result<()>>,
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

            (SessionState::Connected, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::LoggedIn, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::Authenticated, Action::Authenticate) => Ok(SessionState::Authenticated),
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
    should_auto_reconnect: bool,
    network_hint: NetworkHint,
    receive_pipeline: ReceivePipeline,
    last_sync_queued: usize,
    last_sync_dropped_duplicates: usize,
    last_sync_entity_events: Vec<SdkEvent>,
    video_process_hook: Option<VideoProcessHook>,
    last_tmp_cleanup_day: Option<String>,
    pending_events: Vec<SdkEvent>,
}

impl State {
    fn sync_version_key(entity_type: &str, scope: Option<&str>) -> String {
        let scope_part = scope.unwrap_or("*");
        format!("__sync_version__:{entity_type}:{scope_part}")
    }

    async fn load_sync_since_version(&self, entity_type: &str, scope: Option<&str>) -> Option<u64> {
        let key = Self::sync_version_key(entity_type, scope);
        let raw = self.storage.kv_get(key).await.ok().flatten()?;
        let text = String::from_utf8(raw).ok()?;
        text.trim().parse::<u64>().ok()
    }

    async fn save_sync_next_version(
        &self,
        entity_type: &str,
        scope: Option<&str>,
        version: u64,
    ) -> Result<()> {
        let key = Self::sync_version_key(entity_type, scope);
        self.storage
            .kv_put(key, version.to_string().into_bytes())
            .await
    }

    fn should_persist_sync_cursor(entity_type: &str, scope: Option<&str>) -> bool {
        if scope.is_none() {
            return true;
        }
        !matches!(entity_type, "user" | "group" | "channel")
    }

    fn apply_transport_health(
        &mut self,
        is_connected: bool,
    ) -> Option<(ConnectionState, ConnectionState)> {
        if is_connected {
            return None;
        }
        match self.session_state {
            SessionState::Connected | SessionState::LoggedIn | SessionState::Authenticated => {
                let from = self.session_state.as_connection_state();
                self.session_state = SessionState::New;
                self.transport = None;
                Some((from, self.session_state.as_connection_state()))
            }
            _ => None,
        }
    }

    fn network_disconnected_error(&self) -> Error {
        Error::Transport(NETWORK_DISCONNECTED_MESSAGE.to_string())
    }

    fn push_connection_transition_event(
        &mut self,
        transition: Option<(ConnectionState, ConnectionState)>,
    ) {
        if let Some((from, to)) = transition {
            self.pending_events
                .push(SdkEvent::ConnectionStateChanged { from, to });
        }
    }

    fn take_pending_events(&mut self) -> Vec<SdkEvent> {
        std::mem::take(&mut self.pending_events)
    }

    async fn request_bytes(
        &mut self,
        payload: Bytes,
        biz_type: u8,
        timeout: Duration,
        context: &str,
    ) -> Result<Bytes> {
        let transport = match self.transport.as_mut() {
            Some(t) => t,
            None => {
                let transition = self.apply_transport_health(false);
                self.push_connection_transition_event(transition);
                return Err(self.network_disconnected_error());
            }
        };
        let opt = TransportOptions::new()
            .with_biz_type(biz_type)
            .with_timeout(timeout);
        match transport.request_with_options(payload, opt).await {
            Ok(raw) => Ok(raw),
            Err(e) => {
                eprintln!("[SDK.actor] {context} transport error: {e}");
                let transition = self.apply_transport_health(false);
                self.push_connection_transition_event(transition);
                Err(self.network_disconnected_error())
            }
        }
    }

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

    fn parse_protocol_channel_type(value: &serde_json::Value, keys: &[&str]) -> Option<i32> {
        match Self::json_get_i32(value, keys) {
            Some(0) => Some(1),
            Some(1) => Some(1),
            Some(2) => Some(2),
            Some(3) => Some(3),
            _ => None,
        }
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

    fn parse_channel_scope(scope: Option<&str>) -> Option<(i32, u64)> {
        let scope = scope?;
        let parts: Vec<u64> = scope
            .split([':', '/', '|', ','])
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .collect();
        if parts.len() < 2 {
            return None;
        }
        let channel_type_raw = i32::try_from(parts[0]).ok()?;
        let channel_type = if channel_type_raw == 0 {
            1
        } else {
            channel_type_raw
        };
        let channel_id = parts[1];
        Some((channel_type, channel_id))
    }

    fn parse_message_type(value: &serde_json::Value) -> i32 {
        if let Some(kind) = Self::json_get_i32(value, &["message_type", "type"]) {
            return kind;
        }
        if let Some(kind) = Self::json_get_string(value, &["message_type", "type"]) {
            let normalized = kind.to_ascii_lowercase();
            let mapped = match normalized.as_str() {
                "text" => ContentMessageType::Text.as_u32(),
                "image" => ContentMessageType::Image.as_u32(),
                "file" => ContentMessageType::File.as_u32(),
                "voice" => ContentMessageType::Voice.as_u32(),
                "video" => ContentMessageType::Video.as_u32(),
                "system" => ContentMessageType::System.as_u32(),
                "audio" => ContentMessageType::Audio.as_u32(),
                "location" => ContentMessageType::Location.as_u32(),
                "contact_card" => ContentMessageType::ContactCard.as_u32(),
                "sticker" => ContentMessageType::Sticker.as_u32(),
                "forward" => ContentMessageType::Forward.as_u32(),
                _ => ContentMessageType::Text.as_u32(),
            };
            return i32::try_from(mapped).unwrap_or(0);
        }
        i32::try_from(ContentMessageType::Text.as_u32()).unwrap_or(0)
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

    fn is_unsupported_entity_error(err: &Error) -> bool {
        match err {
            Error::Auth(msg) => msg.contains("不支持的 entity_type"),
            _ => false,
        }
    }

    fn push_payload_content(payload: &[u8]) -> String {
        if payload.is_empty() {
            return String::new();
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) {
            if let Some(content) = Self::json_get_string(&value, &["content", "text", "body"]) {
                return content;
            }
            return value.to_string();
        }
        String::from_utf8_lossy(payload).to_string()
    }

    fn push_message_to_sync_item(push: PushMessageRequest) -> SyncEntityItem {
        let payload = serde_json::json!({
            "server_message_id": push.server_message_id,
            "local_message_id": push.local_message_id,
            "channel_id": push.channel_id,
            "channel_type": push.channel_type,
            "timestamp": push.timestamp as i64,
            "from_uid": push.from_uid,
            "message_type": push.message_type as i64,
            "content": Self::push_payload_content(&push.payload),
            "status": 2,
            "pts": push.message_seq as i64,
            "setting": if push.setting.need_receipt { 1 } else { 0 },
            "order_seq": push.message_seq as i64,
            "topic": push.topic,
            "stream_no": push.stream_no,
            "stream_seq": push.stream_seq as i64,
            "stream_flag": push.stream_flag as i64,
            "msg_key": push.msg_key,
            "expire": push.expire as i64,
        });
        SyncEntityItem {
            entity_id: push.server_message_id.to_string(),
            version: u64::from(push.message_seq),
            deleted: false,
            payload: Some(payload),
        }
    }

    async fn apply_sync_entities(
        &mut self,
        entity_type: &str,
        scope: Option<&str>,
        items: &[privchat_protocol::rpc::sync::SyncEntityItem],
    ) -> Result<Vec<SdkEvent>> {
        let _ = self.current_uid.clone().ok_or_else(|| {
            Error::InvalidState("current user is not set; login/authenticate required".to_string())
        })?;
        let current_user_id = self
            .current_uid
            .as_ref()
            .and_then(|v| v.parse::<u64>().ok());
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut emitted = Vec::new();

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
                        self.storage.delete_friend(user_id).await?;
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "friend".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    self.storage
                        .upsert_friend(UpsertFriendInput {
                            user_id,
                            tags: Self::json_get_string(&payload, &["tags"]),
                            is_pinned: Self::json_get_bool(&payload, &["is_pinned", "pinned"])
                                .unwrap_or(false),
                            created_at: Self::json_get_i64(&payload, &["created_at"])
                                .unwrap_or(now_ms),
                            updated_at: Self::json_get_i64(&payload, &["updated_at", "version"])
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "friend".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: false,
                    });
                }
            }
            "user_block" | "blacklist" | "user_mute" => {
                let event_entity_type = if entity_type == "user_mute" {
                    "user_mute"
                } else {
                    "blacklist"
                };
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let blocked_user_id =
                        Self::json_get_u64(&payload, &["blocked_user_id", "user_id", "uid"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    if blocked_user_id == 0 {
                        continue;
                    }
                    if item.deleted {
                        self.storage.delete_blacklist_entry(blocked_user_id).await?;
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: event_entity_type.to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    self.storage
                        .upsert_blacklist_entry(UpsertBlacklistInput {
                            blocked_user_id,
                            created_at: Self::json_get_i64(&payload, &["created_at"])
                                .unwrap_or(now_ms),
                            updated_at: Self::json_get_i64(&payload, &["updated_at", "version"])
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: event_entity_type.to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: false,
                    });
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
                        .upsert_user(UpsertUserInput {
                            user_id,
                            username: Self::json_get_string(&payload, &["username"]),
                            nickname: Self::json_get_string(&payload, &["nickname", "name"]),
                            alias: Self::json_get_string(&payload, &["alias"]),
                            avatar: Self::json_get_string(&payload, &["avatar"])
                                .unwrap_or_default(),
                            user_type: Self::json_get_i32(&payload, &["user_type", "type"])
                                .unwrap_or(0),
                            is_deleted: item.deleted
                                || Self::json_get_bool(&payload, &["is_deleted"]).unwrap_or(false),
                            channel_id: Self::json_get_string(&payload, &["channel_id"])
                                .unwrap_or_default(),
                            updated_at: Self::json_get_i64(&payload, &["updated_at", "version"])
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "user".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
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
                        .upsert_group(UpsertGroupInput {
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
                            updated_at: Self::json_get_i64(&payload, &["updated_at", "version"])
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "group".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
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
                        self.storage.delete_group_member(group_id, user_id).await?;
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "group_member".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    self.storage
                        .upsert_group_member(UpsertGroupMemberInput {
                            group_id,
                            user_id,
                            role: Self::json_get_i32(&payload, &["role"]).unwrap_or(2),
                            status: Self::json_get_i32(&payload, &["status"]).unwrap_or(0),
                            alias: Self::json_get_string(&payload, &["alias"]),
                            is_muted: Self::json_get_bool(&payload, &["is_muted"]).unwrap_or(false),
                            joined_at: Self::json_get_i64(&payload, &["joined_at"])
                                .unwrap_or(now_ms),
                            updated_at: Self::json_get_i64(&payload, &["updated_at", "version"])
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "group_member".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: false,
                    });
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
                    let Some(channel_type) =
                        Self::parse_protocol_channel_type(&payload, &["channel_type", "type"])
                    else {
                        eprintln!(
                            "[SDK.actor] skip channel entity with invalid channel_type, entity_id={}, payload={}",
                            item.entity_id, payload
                        );
                        continue;
                    };
                    let existing = self.storage.get_channel_by_id(channel_id).await?;
                    let incoming_last_ts =
                        Self::json_get_i64(&payload, &["last_msg_timestamp", "updated_at"]);
                    let incoming_last_content =
                        Self::json_get_string(&payload, &["last_msg_content"]);
                    let (last_msg_timestamp, last_msg_content, last_local_message_id) =
                        if let Some(existing) = existing.as_ref() {
                            (
                                incoming_last_ts.unwrap_or(existing.last_msg_timestamp),
                                incoming_last_content
                                    .unwrap_or_else(|| existing.last_msg_content.clone()),
                                existing.last_local_message_id,
                            )
                        } else {
                            (
                                incoming_last_ts.unwrap_or(now_ms),
                                incoming_last_content.unwrap_or_default(),
                                0,
                            )
                        };
                    self.storage
                        .upsert_channel(UpsertChannelInput {
                            channel_id,
                            channel_type,
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
                            last_msg_timestamp,
                            last_local_message_id,
                            last_msg_content,
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "channel".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
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
                    let member_uid =
                        Self::json_get_u64(&payload, &["member_uid", "user_id", "uid"])
                            .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.1))
                            .unwrap_or(0);
                    if channel_id == 0 || member_uid == 0 {
                        continue;
                    }
                    let Some(channel_type) =
                        Self::parse_protocol_channel_type(&payload, &["channel_type"])
                    else {
                        eprintln!(
                            "[SDK.actor] skip channel_member entity with invalid channel_type, entity_id={}, payload={}",
                            item.entity_id, payload
                        );
                        continue;
                    };
                    if item.deleted {
                        self.storage
                            .delete_channel_member(channel_id, channel_type, member_uid)
                            .await?;
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "channel_member".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    self.storage
                        .upsert_channel_member(UpsertChannelMemberInput {
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
                            extra: Self::json_get_string(&payload, &["extra"]).unwrap_or_default(),
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
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "channel_member".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: false,
                    });
                }
            }
            "channel_extra" | "channel_ext" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    if channel_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_channel_extra(UpsertChannelExtraInput {
                            channel_id,
                            channel_type,
                            browse_to: Self::json_get_u64(&payload, &["browse_to"]).unwrap_or(0),
                            keep_pts: Self::json_get_u64(&payload, &["keep_pts"]).unwrap_or(0),
                            keep_offset_y: Self::json_get_i32(&payload, &["keep_offset_y"])
                                .unwrap_or(0),
                            draft: Self::json_get_string(&payload, &["draft"]).unwrap_or_default(),
                            draft_updated_at: Self::json_get_u64(&payload, &["draft_updated_at"])
                                .unwrap_or(0),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "channel_extra".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
            }
            "channel_unread" | "channel_unread_count" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    if channel_id == 0 {
                        continue;
                    }
                    let unread_count =
                        Self::json_get_i32(&payload, &["unread_count", "count"]).unwrap_or(0);
                    let mut channel = self.storage.get_channel_by_id(channel_id).await?;
                    if let Some(existing) = channel.as_mut() {
                        existing.unread_count = unread_count;
                        existing.updated_at = now_ms;
                        self.storage
                            .upsert_channel(UpsertChannelInput {
                                channel_id: existing.channel_id,
                                channel_type: existing.channel_type,
                                channel_name: existing.channel_name.clone(),
                                channel_remark: existing.channel_remark.clone(),
                                avatar: existing.avatar.clone(),
                                unread_count: existing.unread_count,
                                top: existing.top,
                                mute: existing.mute,
                                last_msg_timestamp: existing.last_msg_timestamp,
                                last_local_message_id: existing.last_local_message_id,
                                last_msg_content: existing.last_msg_content.clone(),
                            })
                            .await?;
                    } else {
                        self.storage
                            .upsert_channel(UpsertChannelInput {
                                channel_id,
                                channel_type,
                                channel_name: String::new(),
                                channel_remark: String::new(),
                                avatar: String::new(),
                                unread_count,
                                top: 0,
                                mute: 0,
                                last_msg_timestamp: now_ms,
                                last_local_message_id: 0,
                                last_msg_content: String::new(),
                            })
                            .await?;
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "channel_unread".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
            }
            "message" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let server_message_id =
                        Self::json_get_u64(&payload, &["server_message_id", "message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    if server_message_id == 0 {
                        continue;
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type_scope = scoped_channel.map(|v| v.0);
                    let channel_id_scope = scoped_channel.map(|v| v.1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(channel_id_scope)
                        .unwrap_or(0);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(channel_type_scope)
                    .unwrap_or(1);
                    if item.deleted {
                        if channel_id > 0 {
                            let local_id = self
                                .storage
                                .upsert_remote_message(UpsertRemoteMessageInput {
                                    server_message_id,
                                    local_message_id: Self::json_get_u64(
                                        &payload,
                                        &["local_message_id"],
                                    )
                                    .unwrap_or(0),
                                    channel_id,
                                    channel_type,
                                    timestamp: Self::json_get_i64(
                                        &payload,
                                        &["timestamp", "created_at", "send_time"],
                                    )
                                    .unwrap_or(now_ms),
                                    from_uid: Self::json_get_u64(
                                        &payload,
                                        &["from_uid", "sender_id", "from", "uid"],
                                    )
                                    .unwrap_or(0),
                                    message_type: Self::parse_message_type(&payload),
                                    content: Self::json_get_string(
                                        &payload,
                                        &["content", "text", "body"],
                                    )
                                    .unwrap_or_default(),
                                    status: Self::json_get_i32(&payload, &["status"]).unwrap_or(2),
                                    pts: Self::json_get_i64(&payload, &["pts"])
                                        .unwrap_or(item.version as i64),
                                    setting: Self::json_get_i32(&payload, &["setting"])
                                        .unwrap_or(0),
                                    order_seq: Self::json_get_i64(&payload, &["order_seq"])
                                        .unwrap_or(item.version as i64),
                                    searchable_word: Self::json_get_string(
                                        &payload,
                                        &["searchable_word"],
                                    )
                                    .unwrap_or_default(),
                                    extra: Self::json_get_string(&payload, &["extra"])
                                        .unwrap_or_default(),
                                })
                                .await?;
                            let _ = self.storage.set_message_revoke(local_id, true, None).await;
                        }
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "message".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        if channel_id > 0 {
                            emitted.push(SdkEvent::TimelineUpdated {
                                channel_id,
                                channel_type,
                                message_id: server_message_id,
                                reason: "sync_entity_deleted".to_string(),
                            });
                        }
                        continue;
                    }
                    if channel_id == 0 {
                        continue;
                    }
                    let from_uid =
                        Self::json_get_u64(&payload, &["from_uid", "sender_id", "from", "uid"])
                            .unwrap_or(0);
                    let local_message_id =
                        Self::json_get_u64(&payload, &["local_message_id"]).unwrap_or(0);
                    let status = Self::json_get_i32(&payload, &["status"]).unwrap_or(2);
                    let searchable_word =
                        Self::json_get_string(&payload, &["searchable_word"]).unwrap_or_default();
                    let extra = Self::json_get_string(&payload, &["extra"]).unwrap_or_default();
                    let setting = Self::json_get_i32(&payload, &["setting"]).unwrap_or(0);
                    let order_seq =
                        Self::json_get_i64(&payload, &["order_seq"]).unwrap_or(item.version as i64);
                    let timestamp =
                        Self::json_get_i64(&payload, &["timestamp", "created_at", "send_time"])
                            .unwrap_or(now_ms);
                    let pts = Self::json_get_i64(&payload, &["pts"]).unwrap_or(item.version as i64);
                    let message_type = Self::parse_message_type(&payload);
                    let content = Self::json_get_string(&payload, &["content", "text", "body"])
                        .unwrap_or_default();

                    let message_id = self
                        .storage
                        .upsert_remote_message(UpsertRemoteMessageInput {
                            server_message_id,
                            local_message_id,
                            channel_id,
                            channel_type,
                            timestamp,
                            from_uid,
                            message_type,
                            content: content.clone(),
                            status,
                            pts,
                            setting,
                            order_seq,
                            searchable_word,
                            extra,
                        })
                        .await?;
                    let _ = self
                        .update_channel_last_message(
                            channel_id,
                            channel_type,
                            &content,
                            timestamp,
                            message_id,
                        )
                        .await;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "message".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: false,
                    });
                    emitted.push(SdkEvent::TimelineUpdated {
                        channel_id,
                        channel_type,
                        message_id,
                        reason: "sync_entity".to_string(),
                    });
                    let from_self = current_user_id.map(|v| v == from_uid).unwrap_or(false);
                    if local_message_id > 0 || from_self {
                        emitted.push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status,
                            server_message_id: Some(server_message_id),
                        });
                    }
                }
            }
            "message_extra" | "message_ext" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let message_id =
                        Self::json_get_u64(&payload, &["message_id", "server_message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    if message_id == 0 {
                        continue;
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    if payload.get("readed").is_some()
                        || payload.get("is_read").is_some()
                        || payload.get("read").is_some()
                    {
                        if channel_id > 0 {
                            let is_read =
                                Self::json_get_bool(&payload, &["readed", "is_read", "read"])
                                    .unwrap_or(false);
                            self.storage
                                .set_message_read(message_id, channel_id, channel_type, is_read)
                                .await?;
                            emitted.push(SdkEvent::ReadReceiptUpdated {
                                channel_id,
                                channel_type,
                                message_id,
                                is_read,
                            });
                        }
                    }
                    if payload.get("revoke").is_some() || payload.get("is_revoked").is_some() {
                        let revoke = Self::json_get_bool(&payload, &["revoke", "is_revoked"])
                            .unwrap_or(false);
                        let revoker = Self::json_get_u64(&payload, &["revoker", "revoked_by"]);
                        self.storage
                            .set_message_revoke(message_id, revoke, revoker)
                            .await?;
                    }
                    if let Some(content_edit) =
                        Self::json_get_string(&payload, &["content_edit", "edited_content"])
                    {
                        let edited_at = Self::json_get_i32(&payload, &["edited_at"])
                            .unwrap_or((now_ms / 1000) as i32);
                        self.storage
                            .edit_message(message_id, &content_edit, edited_at)
                            .await?;
                    }
                    if payload.get("is_pinned").is_some() || payload.get("pinned").is_some() {
                        let is_pinned = Self::json_get_bool(&payload, &["is_pinned", "pinned"])
                            .unwrap_or(false);
                        self.storage
                            .set_message_pinned(message_id, is_pinned)
                            .await?;
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "message_extra".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                    if channel_id > 0 {
                        emitted.push(SdkEvent::TimelineUpdated {
                            channel_id,
                            channel_type,
                            message_id,
                            reason: "message_extra_sync".to_string(),
                        });
                    }
                }
            }
            "message_reaction" | "reaction" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let message_id =
                        Self::json_get_u64(&payload, &["message_id", "server_message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    let uid =
                        Self::json_get_u64(&payload, &["uid", "user_id", "sender_id"]).unwrap_or(0);
                    let emoji = Self::json_get_string(&payload, &["emoji"]).unwrap_or_default();
                    if message_id == 0 || channel_id == 0 || uid == 0 || emoji.is_empty() {
                        continue;
                    }
                    self.storage
                        .upsert_message_reaction(UpsertMessageReactionInput {
                            channel_id,
                            channel_type,
                            uid,
                            name: Self::json_get_string(&payload, &["name", "nickname"])
                                .unwrap_or_default(),
                            emoji,
                            message_id,
                            seq: Self::json_get_i64(&payload, &["seq", "version"])
                                .unwrap_or(item.version as i64),
                            is_deleted: item.deleted
                                || Self::json_get_bool(&payload, &["deleted", "is_deleted"])
                                    .unwrap_or(false),
                            created_at: Self::json_get_i64(&payload, &["created_at"])
                                .unwrap_or(now_ms),
                        })
                        .await?;
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "message_reaction".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                    emitted.push(SdkEvent::TimelineUpdated {
                        channel_id,
                        channel_type,
                        message_id,
                        reason: "reaction_sync".to_string(),
                    });
                }
            }
            "mention" | "message_mention" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let message_id =
                        Self::json_get_u64(&payload, &["message_id", "server_message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    let mentioned_user_id =
                        Self::json_get_u64(&payload, &["mentioned_user_id", "uid", "user_id"])
                            .unwrap_or(0);
                    if message_id == 0 || channel_id == 0 || mentioned_user_id == 0 {
                        continue;
                    }
                    self.storage
                        .record_mention(MentionInput {
                            message_id,
                            channel_id,
                            channel_type,
                            mentioned_user_id,
                            sender_id: Self::json_get_u64(
                                &payload,
                                &["sender_id", "from_uid", "from", "operator_id"],
                            )
                            .unwrap_or(0),
                            is_mention_all: Self::json_get_bool(&payload, &["is_mention_all"])
                                .unwrap_or(false),
                            created_at: Self::json_get_i64(&payload, &["created_at"])
                                .unwrap_or(now_ms),
                        })
                        .await?;
                    if Self::json_get_bool(&payload, &["is_read"]).unwrap_or(false) {
                        let _ = self
                            .storage
                            .mark_mention_read(message_id, mentioned_user_id)
                            .await;
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "mention".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                    emitted.push(SdkEvent::TimelineUpdated {
                        channel_id,
                        channel_type,
                        message_id,
                        reason: "mention_sync".to_string(),
                    });
                }
            }
            "reminder" | "message_reminder" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let reminder_id = Self::json_get_u64(&payload, &["reminder_id", "id"])
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if reminder_id == 0 {
                        continue;
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    self.storage
                        .upsert_reminder(UpsertReminderInput {
                            reminder_id,
                            message_id: Self::json_get_u64(
                                &payload,
                                &["message_id", "server_message_id"],
                            )
                            .unwrap_or(0),
                            pts: Self::json_get_i64(&payload, &["pts", "version"])
                                .unwrap_or(item.version as i64),
                            channel_id,
                            channel_type,
                            uid: Self::json_get_u64(&payload, &["uid", "user_id"]).unwrap_or(0),
                            reminder_type: Self::json_get_i32(&payload, &["type", "reminder_type"])
                                .unwrap_or(0),
                            text: Self::json_get_string(&payload, &["text"]).unwrap_or_default(),
                            data: Self::json_get_string(&payload, &["data"]).unwrap_or_default(),
                            is_locate: Self::json_get_bool(&payload, &["is_locate"])
                                .unwrap_or(false),
                            version: Self::json_get_i64(&payload, &["version"])
                                .unwrap_or(item.version as i64),
                            done: item.deleted
                                || Self::json_get_bool(&payload, &["done"]).unwrap_or(false),
                            need_upload: Self::json_get_bool(&payload, &["need_upload"])
                                .unwrap_or(false),
                            publisher: Self::json_get_u64(&payload, &["publisher"]),
                        })
                        .await?;
                    if item.deleted {
                        let _ = self.storage.mark_reminder_done(reminder_id, true).await;
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "reminder".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
            }
            "message_status" | "message_read_status" => {
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let message_id =
                        Self::json_get_u64(&payload, &["message_id", "server_message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    if message_id == 0 {
                        continue;
                    }
                    if let Some(status) = Self::json_get_i32(&payload, &["status"]) {
                        let _ = self.storage.update_message_status(message_id, status).await;
                        emitted.push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status,
                            server_message_id: Some(message_id),
                        });
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = Self::parse_protocol_channel_type(
                        &payload,
                        &["channel_type", "type", "conversation_type"],
                    )
                    .or(scoped_channel.map(|v| v.0))
                    .unwrap_or(1);
                    let channel_id = Self::json_get_u64(&payload, &["channel_id"])
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    if channel_id > 0 {
                        if let Some(is_read) =
                            Self::json_get_bool(&payload, &["readed", "is_read", "read"])
                        {
                            let _ = self
                                .storage
                                .set_message_read(message_id, channel_id, channel_type, is_read)
                                .await;
                            emitted.push(SdkEvent::ReadReceiptUpdated {
                                channel_id,
                                channel_type,
                                message_id,
                                is_read,
                            });
                        }
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "message_status".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
            }
            "user_settings" => {
                for item in items {
                    let setting_key = item.entity_id.clone();
                    let key = format!("{USER_SETTINGS_ITEM_PREFIX}{setting_key}");
                    if item.deleted {
                        self.storage.kv_delete(key).await?;
                    } else {
                        let payload = item
                            .payload
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({}))
                            .to_string()
                            .into_bytes();
                        self.storage.kv_put(key, payload).await?;
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "user_settings".to_string(),
                        entity_id: setting_key,
                        deleted: item.deleted,
                    });
                }
                let snapshot = serde_json::json!({
                    "items": items,
                    "updated_at": now_ms
                })
                .to_string()
                .into_bytes();
                self.storage
                    .kv_put(USER_SETTINGS_KEY.to_string(), snapshot)
                    .await?;
            }
            _ => {}
        }
        Ok(emitted)
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

    async fn ensure_local_message_id(&mut self, message_id: u64) -> Result<u64> {
        let _ = self.current_uid_required()?;
        if let Some(existing) = self
            .storage
            .get_local_message_id(message_id)
            .await?
            .filter(|id| *id > 0)
        {
            return Ok(existing);
        }
        let local_message_id = self.next_local_message_id()?;
        self.storage
            .update_local_message_id(message_id, local_message_id)
            .await?;
        Ok(local_message_id)
    }

    fn should_process_outbound_queue(&self) -> bool {
        self.network_hint.is_online()
            && matches!(
                self.session_state,
                SessionState::Connected | SessionState::LoggedIn | SessionState::Authenticated
            )
            && self.current_uid.is_some()
    }

    async fn cleanup_tmp_dirs_if_needed(&mut self) -> Result<()> {
        if self.current_uid.is_none() {
            return Ok(());
        }
        let today = chrono::Utc::now().format("%Y%m%d").to_string();
        if self.last_tmp_cleanup_day.as_deref() == Some(today.as_str()) {
            return Ok(());
        }
        let paths = self.storage.get_storage_paths().await?;
        let tmp_root = PathBuf::from(paths.user_root).join("tmp");
        if tmp_root.exists() {
            for entry in std::fs::read_dir(&tmp_root)
                .map_err(|e| Error::Storage(format!("read tmp root failed: {e}")))?
            {
                let entry =
                    entry.map_err(|e| Error::Storage(format!("read tmp entry failed: {e}")))?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = match entry.file_name().into_string() {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if name.len() == 8 && name.chars().all(|c| c.is_ascii_digit()) && name < today {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
        self.last_tmp_cleanup_day = Some(today);
        Ok(())
    }

    async fn drain_normal_queue_once(&mut self, limit: usize) -> Result<usize> {
        let items = self.storage.normal_queue_peek(limit).await?;
        if items.is_empty() {
            return Ok(0);
        }
        let mut processed = 0usize;
        for (message_id, _payload) in items {
            let msg = match self.storage.get_message_by_id(message_id).await? {
                Some(v) => v,
                None => {
                    let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "drop_missing".to_string(),
                        message_id: Some(message_id),
                        queue_index: None,
                    });
                    processed += 1;
                    continue;
                }
            };
            let local_message_id = self.ensure_local_message_id(message_id).await?;
            match self.submit_message_via_sync(&msg, local_message_id).await {
                Ok(submit) => {
                    if let ServerDecision::Rejected { reason } = submit.decision {
                        let _ = self.storage.update_message_status(message_id, 3).await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: "failed".to_string(),
                            message_id: Some(message_id),
                            queue_index: None,
                        });
                        return Err(Error::Auth(format!("submit rejected: {reason}")));
                    }
                    if let Some(server_message_id) = submit.server_msg_id {
                        let _ = self
                            .storage
                            .mark_message_sent(message_id, server_message_id)
                            .await;
                    }
                    let _ = self.storage.update_message_status(message_id, 2).await;
                    let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                    let _ = self
                        .update_channel_last_message(
                            msg.channel_id,
                            msg.channel_type,
                            &msg.content,
                            submit.server_timestamp,
                            message_id,
                        )
                        .await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "dequeue".to_string(),
                        message_id: Some(message_id),
                        queue_index: None,
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 2,
                            server_message_id: submit.server_msg_id,
                        });
                    processed += 1;
                }
                Err(e) => {
                    let _ = self.storage.update_message_status(message_id, 1).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "failed".to_string(),
                        message_id: Some(message_id),
                        queue_index: None,
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 1,
                            server_message_id: None,
                        });
                    return Err(e);
                }
            }
        }
        Ok(processed)
    }

    async fn drain_file_queue_once(&mut self, queue_index: usize, limit: usize) -> Result<usize> {
        let items = self.storage.file_queue_peek(queue_index, limit).await?;
        if items.is_empty() {
            return Ok(0);
        }
        let mut processed = 0usize;
        for (message_id, payload) in items {
            let msg = match self.storage.get_message_by_id(message_id).await? {
                Some(v) => v,
                None => {
                    let _ = self
                        .storage
                        .file_queue_ack(queue_index, vec![message_id])
                        .await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "drop_missing".to_string(),
                        message_id: Some(message_id),
                        queue_index: Some(queue_index),
                    });
                    processed += 1;
                    continue;
                }
            };
            let local_message_id = self.ensure_local_message_id(message_id).await?;
            match self
                .process_outbound_file(&msg, local_message_id, payload)
                .await
            {
                Ok(sent) => {
                    let _ = self
                        .storage
                        .mark_message_sent(message_id, sent.server_message_id)
                        .await;
                    let _ = self.storage.update_message_status(message_id, 2).await;
                    let _ = self
                        .storage
                        .file_queue_ack(queue_index, vec![message_id])
                        .await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "dequeue".to_string(),
                        message_id: Some(message_id),
                        queue_index: Some(queue_index),
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 2,
                            server_message_id: Some(sent.server_message_id),
                        });
                    processed += 1;
                }
                Err(e) => {
                    let _ = self.storage.update_message_status(message_id, 1).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "failed".to_string(),
                        message_id: Some(message_id),
                        queue_index: Some(queue_index),
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 1,
                            server_message_id: None,
                        });
                    return Err(e);
                }
            }
        }
        Ok(processed)
    }

    async fn drain_outbound_queues(&mut self) -> Result<usize> {
        if !self.should_process_outbound_queue() {
            return Ok(0);
        }
        let mut drained = 0usize;
        drained += self
            .drain_normal_queue_once(OUTBOUND_DRAIN_BATCH_SIZE)
            .await?;
        let queue_count = self.storage.file_queue_count().await.unwrap_or(0);
        for queue_index in 0..queue_count {
            drained += self
                .drain_file_queue_once(queue_index, OUTBOUND_DRAIN_BATCH_SIZE)
                .await?;
        }
        Ok(drained)
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

    async fn try_auto_reconnect(&mut self) -> Result<SessionState> {
        eprintln!("[SDK.actor] monitor: reconnect start");
        match timeout(self.connect_timeout_total(), self.connect()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("[SDK.actor] monitor: reconnect failed: {e}");
                return Err(e);
            }
            Err(_) => {
                let e = Error::Transport("reconnect timeout".to_string());
                eprintln!("[SDK.actor] monitor: reconnect failed: {e}");
                return Err(e);
            }
        }

        let uid = match self.current_uid.clone() {
            Some(v) => v,
            None => return Ok(SessionState::Connected),
        };
        let snapshot = match self.storage.load_session(uid).await {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(SessionState::Connected),
            Err(e) => {
                eprintln!("[SDK.actor] monitor: load session failed: {e}");
                return Ok(SessionState::Connected);
            }
        };

        eprintln!(
            "[SDK.actor] monitor: restoring session user_id={}",
            snapshot.user_id
        );
        match timeout(
            Duration::from_secs(20),
            self.authenticate(snapshot.user_id, snapshot.token, snapshot.device_id),
        )
        .await
        {
            Ok(Ok(())) => {
                self.bootstrap_completed = snapshot.bootstrap_completed;
                eprintln!("[SDK.actor] monitor: session restored");
                Ok(SessionState::Authenticated)
            }
            Ok(Err(e)) => {
                eprintln!("[SDK.actor] monitor: restore auth failed: {e}");
                Ok(SessionState::Connected)
            }
            Err(_) => {
                eprintln!("[SDK.actor] monitor: restore auth timeout");
                Ok(SessionState::Connected)
            }
        }
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

    async fn probe_connection(&mut self) -> bool {
        let transport = match self.transport.as_mut() {
            Some(transport) => transport,
            None => return false,
        };
        if !transport.is_connected().await {
            return false;
        }
        let payload = match encode_message(&PingRequest {
            timestamp: chrono::Utc::now().timestamp_millis(),
        }) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[SDK.actor] monitor: encode ping failed: {e}");
                return false;
            }
        };
        let options = TransportOptions::new()
            .with_biz_type(MessageType::PingRequest as u8)
            .with_timeout(Duration::from_secs(2));
        match transport
            .request_with_options(Bytes::from(payload), options)
            .await
        {
            Ok(raw) => match decode_message::<PongResponse>(&raw) {
                Ok(_) => true,
                Err(e) => {
                    eprintln!("[SDK.actor] monitor: decode pong failed: {e}");
                    true
                }
            },
            Err(e) => {
                eprintln!("[SDK.actor] monitor: ping request failed: {e}");
                false
            }
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
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc login",
            )
            .await?;

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
        self.storage.flush_user(uid.clone()).await?;
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
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc register",
            )
            .await?;
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
        self.storage.flush_user(uid.clone()).await?;
        self.current_uid = Some(uid);
        Ok(out)
    }

    async fn authenticate(&mut self, user_id: u64, token: String, device_id: String) -> Result<()> {
        eprintln!("[SDK.actor] authenticate: enter user_id={user_id}");
        let timeout = self.timeout();
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
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::AuthorizationRequest as u8,
                timeout,
                "auth request",
            )
            .await?;
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
        self.storage.flush_user(uid.clone()).await?;
        self.current_uid = Some(uid);
        eprintln!("[SDK.actor] authenticate: success");
        Ok(())
    }

    async fn sync_entities(&mut self, entity_type: String, scope: Option<String>) -> Result<usize> {
        let timeout = self.timeout();
        let entity_type_for_apply = entity_type.clone();
        let scope_for_apply = scope.clone();
        let persist_cursor =
            Self::should_persist_sync_cursor(&entity_type_for_apply, scope_for_apply.as_deref());
        self.last_sync_entity_events.clear();
        let mut since_version = if persist_cursor {
            self.load_sync_since_version(&entity_type_for_apply, scope_for_apply.as_deref())
                .await
                .unwrap_or(0)
        } else {
            0
        };
        let mut total_queued = 0usize;
        let mut total_dropped = 0usize;
        let mut fetched_pages = 0usize;
        let mut restarted_full_sync = false;
        let final_next_version = loop {
            fetched_pages += 1;
            if fetched_pages > 64 {
                return Err(Error::InvalidState(
                    "sync_entities exceeded max paging iterations".to_string(),
                ));
            }

            let req = privchat_protocol::rpc::sync::SyncEntitiesRequest {
                entity_type: entity_type_for_apply.clone(),
                since_version: Some(since_version),
                scope: scope_for_apply.clone(),
                limit: Some(200),
            };
            let request = RpcRequest {
                route: privchat_protocol::rpc::routes::entity::SYNC_ENTITIES.to_string(),
                body: serde_json::to_value(req)
                    .map_err(|e| Error::Serialization(format!("encode sync_entities body: {e}")))?,
            };
            let payload = encode_message(&request)
                .map_err(|e| Error::Serialization(format!("encode sync_entities rpc: {e}")))?;
            let raw = self
                .request_bytes(
                    Bytes::from(payload),
                    MessageType::RpcRequest as u8,
                    timeout,
                    "rpc sync_entities",
                )
                .await?;
            let rpc_resp: RpcResponse = decode_message(&raw)
                .map_err(|e| Error::Serialization(format!("decode sync_entities rpc: {e}")))?;
            if rpc_resp.code != 0 {
                return Err(Error::Auth(rpc_resp.message));
            }
            let body = rpc_resp
                .data
                .ok_or_else(|| Error::Serialization("empty sync_entities data".into()))?;
            let response: privchat_protocol::rpc::sync::SyncEntitiesResponse =
                serde_json::from_value(body).map_err(|e| {
                    Error::Serialization(format!("decode sync_entities response: {e}"))
                })?;
            if let Some(min_version) = response.min_version {
                if since_version < min_version && since_version > 0 && !restarted_full_sync {
                    restarted_full_sync = true;
                    since_version = 0;
                    total_queued = 0;
                    total_dropped = 0;
                    continue;
                }
            }
            let stats = self.receive_pipeline.enqueue(
                entity_type_for_apply.clone(),
                scope_for_apply.clone(),
                response.items,
            );
            total_queued += stats.queued_items;
            total_dropped += stats.dropped_duplicates;

            if !response.has_more {
                break response.next_version;
            }
            if response.next_version <= since_version {
                return Err(Error::InvalidState(format!(
                    "sync_entities paging stalled: entity_type={} scope={:?} since={} next={}",
                    entity_type_for_apply, scope_for_apply, since_version, response.next_version
                )));
            }
            since_version = response.next_version;
        };

        let mut applied = 0usize;
        let mut entity_events: Vec<SdkEvent> = Vec::new();
        for batch in SyncCommitApplier::drain_batches(&mut self.receive_pipeline) {
            let count = batch.items.len();
            match self
                .apply_sync_entities(&batch.entity_type, batch.scope.as_deref(), &batch.items)
                .await
            {
                Ok(events) => {
                    applied += count;
                    entity_events.extend(events);
                }
                Err(err) => {
                    SyncCommitApplier::requeue_front(&mut self.receive_pipeline, batch);
                    return Err(err);
                }
            }
        }
        if persist_cursor {
            self.save_sync_next_version(
                &entity_type_for_apply,
                scope_for_apply.as_deref(),
                final_next_version,
            )
            .await?;
        }
        self.last_sync_queued = total_queued;
        self.last_sync_dropped_duplicates = total_dropped;
        self.last_sync_entity_events = entity_events;
        Ok(applied)
    }

    async fn enqueue_and_apply_sync_items(
        &mut self,
        entity_type: String,
        scope: Option<String>,
        items: Vec<SyncEntityItem>,
    ) -> Result<usize> {
        self.last_sync_entity_events.clear();
        let stats = self
            .receive_pipeline
            .enqueue(entity_type.clone(), scope.clone(), items);
        self.last_sync_queued = stats.queued_items;
        self.last_sync_dropped_duplicates = stats.dropped_duplicates;

        let mut applied = 0usize;
        let mut entity_events: Vec<SdkEvent> = Vec::new();
        for batch in SyncCommitApplier::drain_batches(&mut self.receive_pipeline) {
            let count = batch.items.len();
            match self
                .apply_sync_entities(&batch.entity_type, batch.scope.as_deref(), &batch.items)
                .await
            {
                Ok(events) => {
                    applied += count;
                    entity_events.extend(events);
                }
                Err(err) => {
                    SyncCommitApplier::requeue_front(&mut self.receive_pipeline, batch);
                    return Err(err);
                }
            }
        }
        self.last_sync_entity_events = entity_events;
        Ok(applied)
    }

    async fn handle_inbound_frame(&mut self, biz_type: u8, data: Vec<u8>) -> Result<usize> {
        if self.session_state != SessionState::Authenticated {
            return Ok(0);
        }
        let message_type = MessageType::from(biz_type);
        let mut items = Vec::new();
        match message_type {
            MessageType::PushMessageRequest => {
                let req: PushMessageRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode push message: {e}")))?;
                items.push(Self::push_message_to_sync_item(req));
            }
            MessageType::PushBatchRequest => {
                let req: PushBatchRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode push batch: {e}")))?;
                items.extend(
                    req.messages
                        .into_iter()
                        .map(Self::push_message_to_sync_item),
                );
            }
            _ => {
                return Ok(0);
            }
        }

        self.enqueue_and_apply_sync_items("message".to_string(), None, items)
            .await
    }

    async fn run_bootstrap_sync(&mut self) -> Result<()> {
        if self.session_state != SessionState::Authenticated {
            return Err(Error::InvalidState(
                "run_bootstrap_sync requires authenticated state".to_string(),
            ));
        }

        // Keep order aligned with legacy SDK bootstrap strategy.
        let order = [
            "friend",
            "user",
            "user_block",
            "group",
            "channel",
            "channel_extra",
            "channel_unread",
            "user_settings",
        ];
        for entity_type in order {
            match self.sync_entities(entity_type.to_string(), None).await {
                Ok(count) => {
                    eprintln!(
                        "[SDK.actor] bootstrap sync entity={} count={}",
                        entity_type, count
                    );
                }
                Err(e) if Self::is_unsupported_entity_error(&e) => {
                    eprintln!(
                        "[SDK.actor] bootstrap sync skip unsupported entity_type={}: {}",
                        entity_type, e
                    );
                }
                Err(e) => return Err(e),
            }
        }

        // Scoped member sync (best effort but still inside bootstrap critical path):
        // - group_member scoped by group_id
        // - channel_member scoped by "{channel_type}:{channel_id}"
        let mut group_offset = 0usize;
        let group_page_size = 500usize;
        loop {
            let groups = self
                .storage
                .list_groups(group_page_size, group_offset)
                .await?;
            if groups.is_empty() {
                break;
            }
            for group in groups.iter() {
                let scope = group.group_id.to_string();
                match self
                    .sync_entities("group_member".to_string(), Some(scope))
                    .await
                {
                    Ok(count) => {
                        eprintln!(
                            "[SDK.actor] bootstrap sync entity=group_member scope=group:{} count={}",
                            group.group_id, count
                        );
                    }
                    Err(e) if Self::is_unsupported_entity_error(&e) => {
                        eprintln!(
                            "[SDK.actor] bootstrap sync skip unsupported entity_type=group_member: {}",
                            e
                        );
                    }
                    Err(e) => return Err(e),
                }
            }
            if groups.len() < group_page_size {
                break;
            }
            group_offset += group_page_size;
        }
        let mut channel_offset = 0usize;
        let channel_page_size = 500usize;
        loop {
            let channels = self
                .storage
                .list_channels(channel_page_size, channel_offset)
                .await?;
            if channels.is_empty() {
                break;
            }
            for channel in channels.iter() {
                let scope = format!("{}:{}", channel.channel_type, channel.channel_id);
                match self
                    .sync_entities("channel_member".to_string(), Some(scope))
                    .await
                {
                    Ok(count) => {
                        eprintln!(
                            "[SDK.actor] bootstrap sync entity=channel_member scope=channel:{}:{} count={}",
                            channel.channel_type, channel.channel_id, count
                        );
                    }
                    Err(e) if Self::is_unsupported_entity_error(&e) => {
                        eprintln!(
                            "[SDK.actor] bootstrap sync skip unsupported entity_type=channel_member: {}",
                            e
                        );
                    }
                    Err(e) => return Err(e),
                }
            }
            if channels.len() < channel_page_size {
                break;
            }
            channel_offset += channel_page_size;
        }
        self.bootstrap_completed = true;
        if let Some(uid) = &self.current_uid {
            self.storage
                .set_bootstrap_completed(uid.clone(), true)
                .await?;
        }

        // Notify server that bootstrap is finished and this session is ready
        // to receive catch-up and realtime pushes. This endpoint is idempotent.
        let timeout = self.timeout();
        let ready_req = privchat_protocol::rpc::sync::SessionReadyRequest {};
        let ready_rpc = RpcRequest {
            route: routes::sync::SESSION_READY.to_string(),
            body: serde_json::to_value(ready_req)
                .map_err(|e| Error::Serialization(format!("encode session_ready body: {e}")))?,
        };
        let payload = encode_message(&ready_rpc)
            .map_err(|e| Error::Serialization(format!("encode session_ready rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc session_ready",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode session_ready rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty session_ready data".into()))?;
        let ready_ok: privchat_protocol::rpc::sync::SessionReadyResponse =
            serde_json::from_value(body)
                .map_err(|e| Error::Serialization(format!("decode session_ready response: {e}")))?;
        if !ready_ok {
            return Err(Error::InvalidState(
                "session_ready rejected by server".to_string(),
            ));
        }

        Ok(())
    }

    async fn sync_channel(&mut self, channel_id: u64, channel_type: i32) -> Result<usize> {
        // Keep channel-targeted sync aligned with local-first bootstrap:
        // channel base + extra + unread + members in one call chain.
        let scope = Some(format!("{channel_type}:{channel_id}"));
        let mut total = 0usize;
        total += self
            .sync_entities("channel".to_string(), scope.clone())
            .await?;
        total += self
            .sync_entities("channel_extra".to_string(), scope.clone())
            .await?;
        total += self
            .sync_entities("channel_unread".to_string(), scope.clone())
            .await?;
        total += self
            .sync_entities("channel_member".to_string(), scope)
            .await?;
        Ok(total)
    }

    async fn sync_all_channels(&mut self) -> Result<usize> {
        let mut total = 0usize;
        total += self.sync_entities("channel".to_string(), None).await?;
        total += self
            .sync_entities("channel_extra".to_string(), None)
            .await?;
        total += self
            .sync_entities("channel_unread".to_string(), None)
            .await?;
        let mut channel_offset = 0usize;
        let channel_page_size = 500usize;
        loop {
            let channels = self
                .storage
                .list_channels(channel_page_size, channel_offset)
                .await?;
            if channels.is_empty() {
                break;
            }
            for channel in channels.iter() {
                let scope = format!("{}:{}", channel.channel_type, channel.channel_id);
                total += self
                    .sync_entities("channel_member".to_string(), Some(scope))
                    .await?;
            }
            if channels.len() < channel_page_size {
                break;
            }
            channel_offset += channel_page_size;
        }
        Ok(total)
    }

    async fn subscribe_presence(&mut self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let timeout = self.timeout();
        let req = SubscribePresenceRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::SUBSCRIBE.to_string(),
            body: serde_json::to_value(req).map_err(|e| {
                Error::Serialization(format!("encode subscribe_presence body: {e}"))
            })?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode subscribe_presence rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc subscribe_presence",
            )
            .await?;
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
        let req = UnsubscribePresenceRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::UNSUBSCRIBE.to_string(),
            body: serde_json::to_value(req).map_err(|e| {
                Error::Serialization(format!("encode unsubscribe_presence body: {e}"))
            })?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode unsubscribe_presence rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc unsubscribe_presence",
            )
            .await?;
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
        let req = GetOnlineStatusRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::STATUS_GET.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode fetch_presence body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode fetch_presence rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc fetch_presence",
            )
            .await?;
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
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc send_typing",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode send_typing rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        Ok(())
    }

    fn build_send_message_request_with_content(
        &self,
        message: &StoredMessage,
        local_message_id: u64,
        content: String,
    ) -> Result<privchat_protocol::protocol::SendMessageRequest> {
        let message_type = u32::try_from(message.message_type).map_err(|_| {
            Error::InvalidState(format!("invalid message_type: {}", message.message_type))
        })?;
        let metadata_value = serde_json::from_str::<serde_json::Value>(&message.extra)
            .unwrap_or(serde_json::Value::Null);
        let payload = serde_json::to_vec(&serde_json::json!({
            "content": content,
            "metadata": metadata_value,
        }))
        .map_err(|e| Error::Serialization(format!("encode send payload: {e}")))?;

        Ok(privchat_protocol::protocol::SendMessageRequest {
            setting: privchat_protocol::protocol::MessageSetting {
                need_receipt: true,
                signal: 0,
            },
            client_seq: 1,
            local_message_id,
            stream_no: format!("stream_{local_message_id}"),
            channel_id: message.channel_id,
            message_type,
            expire: 3600,
            from_uid: message.from_uid,
            topic: "chat".to_string(),
            payload,
        })
    }

    async fn direct_send_message(
        &mut self,
        req: privchat_protocol::protocol::SendMessageRequest,
    ) -> Result<privchat_protocol::protocol::SendMessageResponse> {
        if req.local_message_id == 0 {
            return Err(Error::InvalidState(
                "local_message_id must be non-zero".to_string(),
            ));
        }
        let timeout = self.timeout();
        let request_data = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode send request: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(request_data),
                MessageType::SendMessageRequest as u8,
                timeout,
                "direct send message",
            )
            .await?;
        let response: privchat_protocol::protocol::SendMessageResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode send response: {e}")))?;
        if response.reason_code != 0 {
            return Err(Error::Transport(format!(
                "send message failed: reason_code={}",
                response.reason_code
            )));
        }
        Ok(response)
    }

    fn guess_file_type(message_type: i32, filename: &str, mime: &str) -> &'static str {
        if message_type == 2 || mime.starts_with("image/") {
            return "image";
        }
        if message_type == 3 || mime.starts_with("video/") {
            return "video";
        }
        if message_type == 8 || mime.starts_with("audio/") {
            return "audio";
        }
        let ext = filename
            .rsplit('.')
            .next()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" => "image",
            "mp4" | "mov" | "mkv" | "avi" | "webm" => "video",
            "mp3" | "wav" | "aac" | "m4a" | "ogg" => "audio",
            _ => "file",
        }
    }

    fn guess_mime_type(filename: &str) -> &'static str {
        let ext = filename
            .rsplit('.')
            .next()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "heic" => "image/heic",
            "mp4" => "video/mp4",
            "mov" => "video/quicktime",
            "mkv" => "video/x-matroska",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "aac" => "audio/aac",
            "m4a" => "audio/mp4",
            "ogg" => "audio/ogg",
            "pdf" => "application/pdf",
            "txt" => "text/plain",
            "json" => "application/json",
            "zip" => "application/zip",
            _ => "application/octet-stream",
        }
    }

    fn yyyymm_from_timestamp_ms(ts_ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ts_ms)
            .unwrap_or_else(chrono::Utc::now)
            .format("%Y%m")
            .to_string()
    }

    fn yyyymmdd_from_timestamp_ms(ts_ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ts_ms)
            .unwrap_or_else(chrono::Utc::now)
            .format("%Y%m%d")
            .to_string()
    }

    fn transparent_png_1x1() -> &'static [u8] {
        &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]
    }

    fn generate_image_thumbnail_sync(
        source_path: &std::path::Path,
        output_path: &std::path::Path,
        max_edge: u32,
        quality: u8,
    ) -> Result<(u32, u32, u64)> {
        use image::codecs::jpeg::JpegEncoder;
        let reader = image::ImageReader::open(source_path)
            .map_err(|e| Error::Storage(format!("open image failed: {e}")))?;
        let img = reader
            .decode()
            .map_err(|e| Error::Storage(format!("decode image failed: {e}")))?;
        let (w, h) = (img.width(), img.height());
        let (tw, th) = if w >= h {
            let nw = w.min(max_edge).max(1);
            let nh = ((h as u64) * (nw as u64) / (w as u64)).max(1) as u32;
            (nw, nh)
        } else {
            let nh = h.min(max_edge).max(1);
            let nw = ((w as u64) * (nh as u64) / (h as u64)).max(1) as u32;
            (nw, nh)
        };
        let thumb = img.resize_exact(tw, th, image::imageops::FilterType::Triangle);
        let rgba = thumb.to_rgba8();
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Storage(format!("create thumb dir failed: {e}")))?;
        }
        let file = std::fs::File::create(output_path)
            .map_err(|e| Error::Storage(format!("create thumb failed: {e}")))?;
        let mut writer = std::io::BufWriter::new(file);
        let mut encoder = JpegEncoder::new_with_quality(&mut writer, quality);
        encoder
            .encode(
                rgba.as_raw(),
                thumb.width(),
                thumb.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| Error::Storage(format!("encode thumb failed: {e}")))?;
        writer
            .flush()
            .map_err(|e| Error::Storage(format!("flush thumb failed: {e}")))?;
        let file_size = output_path
            .metadata()
            .map(|m| m.len())
            .map_err(|e| Error::Storage(format!("stat thumb failed: {e}")))?;
        Ok((thumb.width(), thumb.height(), file_size))
    }

    async fn request_upload_token(
        &mut self,
        user_id: u64,
        filename: String,
        file_size: i64,
        mime_type: String,
        file_type: String,
    ) -> Result<FileRequestUploadTokenResponse> {
        let payload = FileRequestUploadTokenRequest {
            user_id,
            filename: Some(filename),
            file_size,
            mime_type,
            file_type,
            business_type: "message".to_string(),
        };
        let value = self
            .rpc_call_json(
                routes::file::REQUEST_UPLOAD_TOKEN.to_string(),
                serde_json::to_value(payload)
                    .map_err(|e| Error::Serialization(format!("encode upload token body: {e}")))?,
            )
            .await?;
        serde_json::from_value::<FileRequestUploadTokenResponse>(value)
            .map_err(|e| Error::Serialization(format!("decode upload token response: {e}")))
    }

    async fn upload_file_bytes(
        &self,
        upload_url: &str,
        upload_token: &str,
        filename: &str,
        mime_type: &str,
        data: Vec<u8>,
    ) -> Result<UploadedFileInfo> {
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str(mime_type)
            .map_err(|e| Error::Transport(format!("build upload body failed: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = reqwest::Client::new()
            .post(upload_url)
            .header("X-Upload-Token", upload_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| Error::Transport(format!("upload request failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_else(|_| String::new());
            return Err(Error::Transport(format!(
                "upload failed: status={} body={}",
                status, text
            )));
        }
        let value = response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| Error::Serialization(format!("decode upload response: {e}")))?;
        let file_id = value
            .get("file_id")
            .and_then(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
                    .or_else(|| v.as_i64().map(|n| n.to_string()))
            })
            .ok_or_else(|| Error::Serialization("upload response missing file_id".to_string()))?;
        let storage_source_id = value
            .get("storage_source_id")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        Ok(UploadedFileInfo {
            file_id,
            storage_source_id,
        })
    }

    async fn upload_callback(&mut self, user_id: u64, file_id: String) -> Result<()> {
        let payload = FileUploadCallbackRequest {
            file_id,
            user_id,
            status: "uploaded".to_string(),
        };
        let value = self
            .rpc_call_json(
                routes::file::UPLOAD_CALLBACK.to_string(),
                serde_json::to_value(payload).map_err(|e| {
                    Error::Serialization(format!("encode upload callback body: {e}"))
                })?,
            )
            .await?;
        if let Some(ok) = value.as_bool() {
            if ok {
                return Ok(());
            }
            return Err(Error::Transport("upload callback rejected".to_string()));
        }
        Ok(())
    }

    async fn process_outbound_file(
        &mut self,
        message: &StoredMessage,
        local_message_id: u64,
        payload: Vec<u8>,
    ) -> Result<privchat_protocol::protocol::SendMessageResponse> {
        if payload.is_empty() {
            return Err(Error::InvalidState(
                "attachment payload is empty".to_string(),
            ));
        }
        let filename = std::path::Path::new(&message.content)
            .file_name()
            .and_then(|v| v.to_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("file-{}.bin", message.message_id));

        let storage_paths = self.storage.get_storage_paths().await?;
        let yyyymm = Self::yyyymm_from_timestamp_ms(message.created_at);
        let yyyymmdd = Self::yyyymmdd_from_timestamp_ms(message.created_at);
        let files_dir = PathBuf::from(&storage_paths.user_root)
            .join("files")
            .join(yyyymm)
            .join(message.message_id.to_string());
        let tmp_dir = PathBuf::from(&storage_paths.user_root)
            .join("tmp")
            .join(yyyymmdd);
        std::fs::create_dir_all(&files_dir)
            .map_err(|e| Error::Storage(format!("create files dir failed: {e}")))?;
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| Error::Storage(format!("create tmp dir failed: {e}")))?;

        let body_path = files_dir.join(&filename);
        let meta_path = files_dir.join("meta.json");
        std::fs::write(&body_path, &payload)
            .map_err(|e| Error::Storage(format!("write body file failed: {e}")))?;
        let mut upload_payload = payload;
        let mut upload_filename = filename.clone();
        let mut body_size = upload_payload.len() as u64;

        let mime_type = Self::guess_mime_type(&filename).to_string();
        let file_type =
            Self::guess_file_type(message.message_type, &filename, &mime_type).to_string();

        let mut source_width = None;
        let mut source_height = None;
        let mut thumb_upload: Option<(PathBuf, String, String)> = None;
        if file_type == "image" {
            if let Ok(reader) = image::ImageReader::open(&body_path) {
                if let Ok(img) = reader.decode() {
                    source_width = Some(img.width());
                    source_height = Some(img.height());
                }
            }
            let thumb_path = tmp_dir.join(format!("{}_thumb.jpg", message.message_id));
            let (w, h, size) =
                Self::generate_image_thumbnail_sync(&body_path, &thumb_path, 90, 85)?;
            thumb_upload = Some((
                thumb_path,
                "image/jpeg".to_string(),
                format!("{}_thumb.jpg", message.message_id),
            ));
            let meta = MediaMeta {
                source: MediaSourceMeta {
                    original_filename: filename.clone(),
                    mime: mime_type.clone(),
                    width: source_width,
                    height: source_height,
                    file_size: Some(body_size),
                },
                thumbnail: Some(MediaThumbnailMeta {
                    width: Some(w),
                    height: Some(h),
                    file_size: Some(size),
                    mime: Some("image/jpeg".to_string()),
                }),
                processing: Some(MediaProcessingMeta {
                    strategy: Some("client_preprocess".to_string()),
                    created_at: Some(chrono::Utc::now().timestamp()),
                }),
            };
            std::fs::write(
                &meta_path,
                serde_json::to_string(&meta)
                    .map_err(|e| Error::Serialization(format!("encode media meta failed: {e}")))?,
            )
            .map_err(|e| Error::Storage(format!("write media meta failed: {e}")))?;
        } else if file_type == "video" {
            if let Some(hook) = self.video_process_hook.as_ref() {
                let ext = std::path::Path::new(&filename)
                    .extension()
                    .and_then(|v| v.to_str())
                    .unwrap_or("mp4");
                let compressed_path =
                    tmp_dir.join(format!("{}_compressed.{ext}", message.message_id));
                let compressed_ok = hook(
                    MediaProcessOp::Compress,
                    &body_path,
                    &meta_path,
                    &compressed_path,
                )
                .map_err(|e| Error::Storage(format!("video compress hook failed: {e}")))?;
                if compressed_ok && compressed_path.exists() {
                    upload_payload = std::fs::read(&compressed_path).map_err(|e| {
                        Error::Storage(format!("read compressed video failed: {e}"))
                    })?;
                    body_size = upload_payload.len() as u64;
                    upload_filename = filename.clone();
                }
            }

            let hook_thumb_path = tmp_dir.join(format!("{}_thumb.jpg", message.message_id));
            let mut hook_used = false;
            if let Some(hook) = self.video_process_hook.as_ref() {
                let ok = hook(
                    MediaProcessOp::Thumbnail,
                    &body_path,
                    &meta_path,
                    &hook_thumb_path,
                )
                .map_err(|e| Error::Storage(format!("video thumbnail hook failed: {e}")))?;
                if ok && hook_thumb_path.exists() {
                    hook_used = true;
                    thumb_upload = Some((
                        hook_thumb_path.clone(),
                        "image/jpeg".to_string(),
                        format!("{}_thumb.jpg", message.message_id),
                    ));
                }
            }
            if !hook_used {
                let thumb_path = tmp_dir.join(format!("{}_thumb.png", message.message_id));
                std::fs::write(&thumb_path, Self::transparent_png_1x1()).map_err(|e| {
                    Error::Storage(format!("write video placeholder thumb failed: {e}"))
                })?;
                thumb_upload = Some((
                    thumb_path,
                    "image/png".to_string(),
                    format!("{}_thumb.png", message.message_id),
                ));
            }

            let (thumb_w, thumb_h, thumb_size, thumb_mime) = match thumb_upload.as_ref() {
                Some((path, mime, _)) => {
                    let size = std::fs::metadata(path)
                        .map(|m| m.len())
                        .map_err(|e| Error::Storage(format!("stat video thumb failed: {e}")))?;
                    (None, None, Some(size), Some(mime.clone()))
                }
                None => (None, None, None, None),
            };
            let meta = MediaMeta {
                source: MediaSourceMeta {
                    original_filename: upload_filename.clone(),
                    mime: mime_type.clone(),
                    width: None,
                    height: None,
                    file_size: Some(body_size),
                },
                thumbnail: Some(MediaThumbnailMeta {
                    width: thumb_w,
                    height: thumb_h,
                    file_size: thumb_size,
                    mime: thumb_mime,
                }),
                processing: Some(MediaProcessingMeta {
                    strategy: Some("client_preprocess".to_string()),
                    created_at: Some(chrono::Utc::now().timestamp()),
                }),
            };
            std::fs::write(
                &meta_path,
                serde_json::to_string(&meta)
                    .map_err(|e| Error::Serialization(format!("encode media meta failed: {e}")))?,
            )
            .map_err(|e| Error::Storage(format!("write media meta failed: {e}")))?;
        } else {
            let meta = MediaMeta {
                source: MediaSourceMeta {
                    original_filename: upload_filename.clone(),
                    mime: mime_type.clone(),
                    width: None,
                    height: None,
                    file_size: Some(body_size),
                },
                thumbnail: None,
                processing: Some(MediaProcessingMeta {
                    strategy: Some("client_preprocess".to_string()),
                    created_at: Some(chrono::Utc::now().timestamp()),
                }),
            };
            std::fs::write(
                &meta_path,
                serde_json::to_string(&meta)
                    .map_err(|e| Error::Serialization(format!("encode media meta failed: {e}")))?,
            )
            .map_err(|e| Error::Storage(format!("write media meta failed: {e}")))?;
        }

        let thumbnail_file_id = if let Some((thumb_path, thumb_mime, thumb_name)) = thumb_upload {
            let thumb_size = std::fs::metadata(&thumb_path)
                .map(|m| m.len() as i64)
                .map_err(|e| Error::Storage(format!("stat thumb file failed: {e}")))?;
            let thumb_token = self
                .request_upload_token(
                    message.from_uid,
                    thumb_name.clone(),
                    thumb_size,
                    thumb_mime.clone(),
                    "image".to_string(),
                )
                .await?;
            let thumb_bytes = std::fs::read(&thumb_path)
                .map_err(|e| Error::Storage(format!("read thumb file failed: {e}")))?;
            let uploaded_thumb = self
                .upload_file_bytes(
                    &thumb_token.upload_url,
                    &thumb_token.token,
                    &thumb_name,
                    &thumb_mime,
                    thumb_bytes,
                )
                .await?;
            self.upload_callback(message.from_uid, uploaded_thumb.file_id.clone())
                .await?;
            Some(uploaded_thumb.file_id)
        } else {
            None
        };

        let token = self
            .request_upload_token(
                message.from_uid,
                upload_filename.clone(),
                upload_payload.len() as i64,
                mime_type.clone(),
                file_type,
            )
            .await?;
        let uploaded = self
            .upload_file_bytes(
                &token.upload_url,
                &token.token,
                &upload_filename,
                &mime_type,
                upload_payload,
            )
            .await?;
        self.upload_callback(message.from_uid, uploaded.file_id.clone())
            .await?;

        let attachment_content = if let Some(thumb_file_id) = thumbnail_file_id {
            serde_json::json!({
                "file_id": uploaded.file_id,
                "thumbnail_file_id": thumb_file_id,
                "filename": upload_filename,
                "mime_type": mime_type,
                "storage_source_id": uploaded.storage_source_id,
            })
        } else {
            serde_json::json!({
                "file_id": uploaded.file_id,
                "filename": upload_filename,
                "mime_type": mime_type,
                "storage_source_id": uploaded.storage_source_id,
            })
        };
        let content = serde_json::to_string(&attachment_content)
            .map_err(|e| Error::Serialization(format!("encode attachment content: {e}")))?;
        let req =
            self.build_send_message_request_with_content(message, local_message_id, content)?;
        self.direct_send_message(req).await
    }

    async fn rpc_call_json(
        &mut self,
        route: String,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let timeout = self.timeout();
        let request = RpcRequest { route, body };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode rpc request: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc call",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode rpc response: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        Ok(rpc_resp.data.unwrap_or(serde_json::Value::Null))
    }

    async fn update_channel_last_message(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        content: &str,
        timestamp_ms: i64,
        message_id: u64,
    ) -> Result<()> {
        let existing = self.storage.get_channel_by_id(channel_id).await?;
        let (channel_name, channel_remark, avatar, top, mute, unread_count) =
            if let Some(c) = existing {
                (
                    c.channel_name,
                    c.channel_remark,
                    c.avatar,
                    c.top,
                    c.mute,
                    c.unread_count,
                )
            } else {
                (
                    channel_id.to_string(),
                    String::new(),
                    String::new(),
                    0,
                    0,
                    0,
                )
            };
        self.storage
            .upsert_channel(UpsertChannelInput {
                channel_id,
                channel_type,
                channel_name,
                channel_remark,
                avatar,
                unread_count,
                top,
                mute,
                last_msg_timestamp: timestamp_ms,
                last_local_message_id: message_id,
                last_msg_content: content.to_string(),
            })
            .await?;
        Ok(())
    }

    async fn submit_message_via_sync(
        &mut self,
        message: &StoredMessage,
        local_message_id: u64,
    ) -> Result<ClientSubmitResponse> {
        let channel_type = u8::try_from(message.channel_type).map_err(|_| {
            Error::InvalidState(format!("invalid channel_type: {}", message.channel_type))
        })?;
        let command_type = ContentMessageType::from_u32(message.message_type as u32)
            .map(|v| v.as_str().to_string())
            .unwrap_or_else(|| "text".to_string());
        let mut payload = serde_json::json!({
            "content": message.content,
            "text": message.content,
        });
        if let Ok(extra_json) = serde_json::from_str::<serde_json::Value>(&message.extra) {
            if let (serde_json::Value::Object(base), serde_json::Value::Object(extra)) =
                (&mut payload, extra_json)
            {
                for (k, v) in extra {
                    base.insert(k, v);
                }
            }
        }
        let submit_req = ClientSubmitRequest {
            local_message_id,
            channel_id: message.channel_id,
            channel_type,
            last_pts: 0,
            command_type,
            payload,
            client_timestamp: chrono::Utc::now().timestamp_millis(),
            device_id: None,
        };
        let body = self
            .rpc_call_json(
                routes::sync::SUBMIT.to_string(),
                serde_json::to_value(submit_req)
                    .map_err(|e| Error::Serialization(format!("encode submit body: {e}")))?,
            )
            .await?;
        serde_json::from_value::<ClientSubmitResponse>(body)
            .map_err(|e| Error::Serialization(format!("decode submit response: {e}")))
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
    startup_error: Arc<StdMutex<Option<Error>>>,
}

impl PrivchatSdk {
    fn is_timeline_like_event(event: &SdkEvent) -> bool {
        matches!(
            event,
            SdkEvent::TimelineUpdated { .. }
                | SdkEvent::ReadReceiptUpdated { .. }
                | SdkEvent::MessageSendStatusChanged { .. }
                | SdkEvent::SyncEntityChanged { .. }
                | SdkEvent::SyncChannelApplied { .. }
                | SdkEvent::SyncAllChannelsApplied { .. }
                | SdkEvent::TypingSent { .. }
        )
    }

    fn is_network_like_event(event: &SdkEvent) -> bool {
        matches!(
            event,
            SdkEvent::ConnectionStateChanged { .. } | SdkEvent::NetworkHintChanged { .. }
        )
    }

    pub fn new(config: PrivchatConfig) -> Self {
        Self::with_runtime(config, RuntimeProvider::new_owned())
    }

    pub fn with_runtime(config: PrivchatConfig, runtime_provider: RuntimeProvider) -> Self {
        let configured_data_dir = config.data_dir.clone();
        let (tx, mut rx) = mpsc::channel::<Command>(64);
        let actor_cmd_tx = tx.clone();
        let (event_tx, _) = broadcast::channel::<SdkEvent>(256);
        let actor_event_tx = event_tx.clone();
        let event_seq = Arc::new(AtomicU64::new(0));
        let actor_event_seq = event_seq.clone();
        let event_history = Arc::new(StdMutex::new(VecDeque::new()));
        let actor_event_history = event_history.clone();
        let event_history_limit = DEFAULT_EVENT_HISTORY_LIMIT;
        let task_registry = TaskRegistry::new();
        let startup_error = Arc::new(StdMutex::new(None));
        let actor_startup_error = startup_error.clone();
        let actor_task = runtime_provider.spawn(async move {
            eprintln!("[SDK.actor] loop: started");
            if configured_data_dir.trim().is_empty() {
                eprintln!("[SDK.actor] storage base: <default>");
            } else {
                eprintln!("[SDK.actor] storage base: {}", configured_data_dir);
            }
            let storage = match if configured_data_dir.trim().is_empty() {
                StorageHandle::start()
            } else {
                StorageHandle::start_at(PathBuf::from(configured_data_dir))
            } {
                Ok(s) => s,
                Err(e) => {
                    if let Ok(mut locked) = actor_startup_error.lock() {
                        *locked = Some(Error::Storage(format!("storage init failed: {e}")));
                    }
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
                    if let Ok(mut locked) = actor_startup_error.lock() {
                        *locked =
                            Some(Error::InvalidState(format!("snowflake init failed: {e:?}")));
                    }
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
                should_auto_reconnect: false,
                network_hint: NetworkHint::Unknown,
                receive_pipeline: ReceivePipeline::default(),
                last_sync_queued: 0,
                last_sync_dropped_duplicates: 0,
                last_sync_entity_events: Vec::new(),
                video_process_hook: None,
                last_tmp_cleanup_day: None,
                pending_events: Vec::new(),
            };
            let mut inbound_task: Option<tokio::task::JoinHandle<()>> = None;
            let mut health_tick = interval(Duration::from_secs(2));
            health_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = health_tick.tick() => {
                        if state.session_state == SessionState::Shutdown || !state.should_auto_reconnect {
                            continue;
                        }
                        if !state.network_hint.is_online() {
                            continue;
                        }

                        if matches!(
                            state.session_state,
                            SessionState::Connected | SessionState::LoggedIn | SessionState::Authenticated
                        ) {
                            let is_connected = state.probe_connection().await;
                            if !is_connected {
                                stop_inbound_task(&mut inbound_task).await;
                                if let Some((from, to)) = state.apply_transport_health(false) {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged { from, to },
                                    );
                                }
                            }
                        }

                        if state.session_state == SessionState::New && state.should_auto_reconnect {
                            let from = state.session_state.as_connection_state();
                            match state.try_auto_reconnect().await {
                                Ok(next) => {
                                    state.session_state = next;
                                    start_inbound_task(&state, actor_cmd_tx.clone(), &mut inbound_task)
                                        .await;
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged {
                                            from,
                                            to: state.session_state.as_connection_state(),
                                        },
                                    );
                                }
                                Err(_) => {
                                    // keep New and retry on next tick
                                }
                            }
                        }

                        let _ = state.cleanup_tmp_dirs_if_needed().await;

                        if state.should_process_outbound_queue() {
                            let _ = state.drain_outbound_queues().await;
                        }
                        for event in state.take_pending_events() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                event,
                            );
                        }
                    }
                    cmd = rx.recv() => {
                        let Some(cmd) = cmd else { break; };
                        match cmd {
                    Command::Connect { resp } => {
                        eprintln!("[SDK.actor] loop: cmd connect");
                        if !state.network_hint.is_online() {
                            let _ = resp.send(Err(state.network_disconnected_error()));
                        } else {
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
                                state.should_auto_reconnect = true;
                                start_inbound_task(&state, actor_cmd_tx.clone(), &mut inbound_task)
                                    .await;
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
                    }
                    Command::Disconnect { resp } => {
                        eprintln!("[SDK.actor] loop: cmd disconnect");
                        state.should_auto_reconnect = false;
                        stop_inbound_task(&mut inbound_task).await;
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
                        let is_connected = state.is_connected().await;
                        if let Some((from, to)) = state.apply_transport_health(is_connected) {
                            stop_inbound_task(&mut inbound_task).await;
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged { from, to },
                            );
                        }
                        let _ = resp.send(Ok(is_connected));
                    }
                    Command::GetConnectionState { resp } => {
                        let is_connected = state.is_connected().await;
                        if let Some((from, to)) = state.apply_transport_health(is_connected) {
                            stop_inbound_task(&mut inbound_task).await;
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged { from, to },
                            );
                        }
                        let _ = resp.send(Ok(state.session_state.as_connection_state()));
                    }
                    Command::Ping { resp } => {
                        let result = if state.probe_connection().await {
                            Ok(())
                        } else {
                            stop_inbound_task(&mut inbound_task).await;
                            let transition = state.apply_transport_health(false);
                            state.push_connection_transition_event(transition);
                            Err(state.network_disconnected_error())
                        };
                        let _ = resp.send(result);
                    }
                    Command::SetNetworkHint { hint, resp } => {
                        let old_hint = state.network_hint;
                        state.network_hint = hint;
                        if old_hint != hint {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::NetworkHintChanged {
                                    from: old_hint,
                                    to: hint,
                                },
                            );
                        }
                        if matches!(hint, NetworkHint::Offline) {
                            stop_inbound_task(&mut inbound_task).await;
                            if let Some((from, to)) = state.apply_transport_health(false) {
                                emit_sequenced_event(
                                    &actor_event_tx,
                                    &actor_event_history,
                                    &actor_event_seq,
                                    event_history_limit,
                                    SdkEvent::ConnectionStateChanged { from, to },
                                );
                            }
                        } else if matches!(old_hint, NetworkHint::Offline) && state.should_auto_reconnect {
                            // Trigger reconnect sooner on network recovery.
                            if state.session_state == SessionState::New {
                                let from = state.session_state.as_connection_state();
                                if let Ok(next) = state.try_auto_reconnect().await {
                                    state.session_state = next;
                                    start_inbound_task(&state, actor_cmd_tx.clone(), &mut inbound_task)
                                        .await;
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged {
                                            from,
                                            to: state.session_state.as_connection_state(),
                                        },
                                    );
                                }
                            }
                        }
                        let _ = resp.send(Ok(()));
                    }
                    Command::InboundFrame { biz_type, data } => {
                        match state.handle_inbound_frame(biz_type, data).await {
                            Ok(applied) if applied > 0 => {
                                for evt in state.last_sync_entity_events.clone() {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        evt,
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("[SDK.actor] inbound apply failed: {e}");
                            }
                        }
                    }
                    Command::InboundDisconnected => {
                        stop_inbound_task(&mut inbound_task).await;
                        if let Some((from, to)) = state.apply_transport_health(false) {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ConnectionStateChanged { from, to },
                            );
                        }
                    }
                    Command::SetVideoProcessHook { hook, resp } => {
                        state.video_process_hook = hook;
                        let _ = resp.send(Ok(()));
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
                        let event_entity_type = entity_type.clone();
                        let event_scope = scope.clone();
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
                        if let Ok(applied) = result {
                            for evt in state.last_sync_entity_events.clone() {
                                emit_sequenced_event(
                                    &actor_event_tx,
                                    &actor_event_history,
                                    &actor_event_seq,
                                    event_history_limit,
                                    evt,
                                );
                            }
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::SyncEntitiesApplied {
                                    entity_type: event_entity_type,
                                    scope: event_scope,
                                    queued: state.last_sync_queued,
                                    applied,
                                    dropped_duplicates: state.last_sync_dropped_duplicates,
                                },
                            );
                            let _ = resp.send(Ok(applied));
                        } else {
                            let _ = resp.send(result);
                        }
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
                        if let Ok(applied) = result {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::SyncChannelApplied {
                                    channel_id,
                                    channel_type,
                                    applied,
                                },
                            );
                            let _ = resp.send(Ok(applied));
                        } else {
                            let _ = resp.send(result);
                        }
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
                        if let Ok(applied) = result {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::SyncAllChannelsApplied { applied },
                            );
                            let _ = resp.send(Ok(applied));
                        } else {
                            let _ = resp.send(result);
                        }
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
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TypingSent {
                                    channel_id,
                                    channel_type,
                                    is_typing,
                                },
                            );
                        }
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
                        state.should_auto_reconnect = false;
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
                            Ok(_) => match state
                                .storage
                                .normal_queue_push(message_id, payload)
                                .await
                            {
                                Ok(pushed_id) => {
                                    let _ = state
                                        .storage
                                        .update_message_status(message_id, 1)
                                        .await;
                                    let _ = state.drain_outbound_queues().await;
                                    Ok(pushed_id)
                                }
                                Err(e) => Err(e),
                            },
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::OutboundQueueUpdated {
                                    kind: "normal".to_string(),
                                    action: "enqueue".to_string(),
                                    message_id: Some(message_id),
                                    queue_index: None,
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::PeekOutboundMessages { limit, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .normal_queue_peek(limit)
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
                            Ok(_) => state.storage.normal_queue_ack(message_ids).await,
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
                            Ok(_) => {
                                let q = state
                                    .storage
                                    .select_file_queue(route_key)
                                    .await;
                                match q {
                                    Ok(queue_index) => {
                                        match state
                                            .storage
                                            .file_queue_push(queue_index, message_id, payload)
                                            .await
                                        {
                                            Ok(file_msg_id) => {
                                                let _ = state
                                                    .storage
                                                    .update_message_status(message_id, 1)
                                                    .await;
                                                let _ = state.drain_outbound_queues().await;
                                                Ok(FileQueueRef {
                                                    queue_index,
                                                    message_id: file_msg_id,
                                                })
                                            }
                                            Err(e) => Err(e),
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            Err(e) => Err(e),
                        };
                        if let Ok(queue_ref) = &result {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::OutboundQueueUpdated {
                                    kind: "file".to_string(),
                                    action: "enqueue".to_string(),
                                    message_id: Some(message_id),
                                    queue_index: Some(queue_ref.queue_index),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::PeekOutboundFiles {
                        queue_index,
                        limit,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state
                                .storage
                                .file_queue_peek(queue_index, limit)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .file_queue_ack(queue_index, message_ids)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::CreateLocalMessage { input, resp } => {
                        let channel_id = input.channel_id;
                        let channel_type = input.channel_type;
                        let result = match state.current_uid_required() {
                            Ok(_) => match state.next_local_message_id() {
                                Ok(local_message_id) => {
                                    state
                                        .storage
                                        .create_local_message(local_message_id, input)
                                        .await
                                }
                                Err(e) => Err(e),
                            },
                            Err(e) => Err(e),
                        };
                        if let Ok(message_id) = result {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TimelineUpdated {
                                    channel_id,
                                    channel_type,
                                    message_id,
                                    reason: "local_create".to_string(),
                                },
                            );
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::MessageSendStatusChanged {
                                    message_id,
                                    status: 0,
                                    server_message_id: None,
                                },
                            );
                            let _ = resp.send(Ok(message_id));
                        } else {
                            let _ = resp.send(result);
                        }
                    }
                    Command::GetMessageById { message_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_messages(channel_id, channel_type, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannel { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_channel(input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetChannelById { channel_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_channel_by_id(channel_id).await,
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
                            Ok(_) => state.storage.list_channels(limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannelExtra { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_channel_extra(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .get_channel_extra(channel_id, channel_type)
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
                            Ok(_) => state
                                .storage
                                .mark_message_sent(message_id, server_message_id)
                                .await,
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::MessageSendStatusChanged {
                                    message_id,
                                    status: 2,
                                    server_message_id: Some(server_message_id),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::UpdateMessageStatus {
                        message_id,
                        status,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.update_message_status(message_id, status).await,
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::MessageSendStatusChanged {
                                    message_id,
                                    status,
                                    server_message_id: None,
                                },
                            );
                        }
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
                            Ok(_) => {
                                state
                                    .storage
                                    .set_message_read(message_id, channel_id, channel_type, is_read)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::ReadReceiptUpdated {
                                    channel_id,
                                    channel_type,
                                    message_id,
                                    is_read,
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::SetMessageRevoke {
                        message_id,
                        revoked,
                        revoker,
                        resp,
                    } => {
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .set_message_revoke(message_id, revoked, revoker)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TimelineUpdated {
                                    channel_id: message_ctx.as_ref().map(|m| m.channel_id).unwrap_or(0),
                                    channel_type: message_ctx.as_ref().map(|m| m.channel_type).unwrap_or(0),
                                    message_id,
                                    reason: "revoke".to_string(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::EditMessage {
                        message_id,
                        content,
                        edited_at,
                        resp,
                    } => {
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .edit_message(message_id, &content, edited_at)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TimelineUpdated {
                                    channel_id: message_ctx.as_ref().map(|m| m.channel_id).unwrap_or(0),
                                    channel_type: message_ctx.as_ref().map(|m| m.channel_type).unwrap_or(0),
                                    message_id,
                                    reason: "edit".to_string(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::SetMessagePinned {
                        message_id,
                        is_pinned,
                        resp,
                    } => {
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .set_message_pinned(message_id, is_pinned)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TimelineUpdated {
                                    channel_id: message_ctx.as_ref().map(|m| m.channel_id).unwrap_or(0),
                                    channel_type: message_ctx.as_ref().map(|m| m.channel_type).unwrap_or(0),
                                    message_id,
                                    reason: if is_pinned {
                                        "pin".to_string()
                                    } else {
                                        "unpin".to_string()
                                    },
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::GetMessageExtra { message_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_extra(message_id).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .mark_channel_read(channel_id, channel_type)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .get_channel_unread_count(channel_id, channel_type)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .get_total_unread_count(exclude_muted)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertUser { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_user(input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetUserById { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_user_by_id(user_id).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::ListUsersByIds { user_ids, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.list_users_by_ids(user_ids).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertFriend { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_friend(input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteFriend { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.delete_friend(user_id).await,
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
                            Ok(_) => state.storage.list_friends(limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertBlacklistEntry { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_blacklist_entry(input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteBlacklistEntry {
                        blocked_user_id,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .delete_blacklist_entry(blocked_user_id)
                                    .await
                            }
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_blacklist_entries(limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertGroup { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_group(input).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetGroupById { group_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_group_by_id(group_id).await,
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
                            Ok(_) => state.storage.list_groups(limit, offset).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertGroupMember { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_group_member(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .delete_group_member(group_id, user_id)
                                    .await
                            }
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_group_members(group_id, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertChannelMember { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_channel_member(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_channel_members(
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
                            Ok(_) => {
                                state
                                    .storage
                                    .delete_channel_member(channel_id, channel_type, member_uid)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertMessageReaction { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_message_reaction(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_message_reactions(message_id, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RecordMention { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.record_mention(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .get_unread_mention_count(channel_id, channel_type, user_id)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_unread_mention_message_ids(channel_id, channel_type, user_id, limit)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .mark_mention_read(message_id, user_id)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .mark_all_mentions_read(channel_id, channel_type, user_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetAllUnreadMentionCounts { user_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .get_all_unread_mention_counts(user_id)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpsertReminder { input, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.upsert_reminder(input).await,
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
                            Ok(_) => {
                                state
                                    .storage
                                    .list_pending_reminders(reminder_uid, limit, offset)
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
                            Ok(_) => {
                                state
                                    .storage
                                    .mark_reminder_done(reminder_id, done)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::KvPut { key, value, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.kv_put(key, value).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::KvGet { key, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.kv_get(key).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::KvScanPrefix { prefix, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.kv_scan_prefix(prefix).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::GetUserStoragePaths { resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.get_storage_paths().await.map(|paths| {
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
                    Command::ListLocalAccounts { resp } => {
                        let result = state.storage.list_local_accounts().await.map(
                            |(active_uid, entries)| {
                                entries
                                    .into_iter()
                                    .map(|entry| LocalAccountSummary {
                                        is_active: active_uid
                                            .as_ref()
                                            .map(|uid| uid == &entry.uid)
                                            .unwrap_or(false),
                                        uid: entry.uid,
                                        created_at: entry.created_at,
                                        last_login_at: entry.last_login_at,
                                    })
                                    .collect::<Vec<_>>()
                            },
                        );
                        let _ = resp.send(result);
                    }
                    Command::SetCurrentUid { uid, resp } => {
                        let result = match state.storage.list_local_accounts().await {
                            Ok((_, entries)) => {
                                if !entries.iter().any(|entry| entry.uid == uid) {
                                    Err(Error::InvalidState(format!(
                                        "local account not found: {uid}"
                                    )))
                                } else {
                                    let saved = state.storage.save_current_uid(uid.clone()).await;
                                    let loaded = state.storage.load_session(uid.clone()).await;
                                    match (saved, loaded) {
                                        (Ok(()), Ok(snapshot)) => {
                                            state.current_uid = Some(uid);
                                            state.bootstrap_completed = snapshot
                                                .map(|s| s.bootstrap_completed)
                                                .unwrap_or(false);
                                            Ok(())
                                        }
                                        (Err(e), _) => Err(e),
                                        (_, Err(e)) => Err(e),
                                    }
                                }
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::WipeCurrentUserFull { resp } => {
                        let from_state = state.session_state.as_connection_state();
                        state.should_auto_reconnect = false;
                        state.bootstrap_completed = false;
                        state.session_state = SessionState::Connected;
                        let result = match state.current_uid.clone() {
                            Some(uid) => {
                                let clear_uid = state.storage.clear_current_uid().await;
                                let wipe = state.storage.wipe_user_full(uid).await;
                                state.current_uid = None;
                                wipe.and(clear_uid)
                            }
                            None => Ok(()),
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
                    Command::Shutdown { resp } => {
                        eprintln!("[SDK.actor] loop: cmd shutdown");
                        state.should_auto_reconnect = false;
                        stop_inbound_task(&mut inbound_task).await;
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
                        for event in state.take_pending_events() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                event,
                            );
                        }
                    }
                }
            }
            stop_inbound_task(&mut inbound_task).await;
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
            startup_error,
        }
    }

    fn actor_channel_error(&self) -> Error {
        if let Ok(locked) = self.startup_error.lock() {
            if let Some(err) = locked.clone() {
                return err;
            }
        }
        if self.shutting_down.load(Ordering::Acquire) {
            Error::Shutdown
        } else {
            Error::ActorClosed
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
            .map_err(|_| self.actor_channel_error())?;
        let out = resp_rx.await.map_err(|_| self.actor_channel_error())?;
        eprintln!("[SDK.api] connect: recv");
        out
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Disconnect { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn is_connected(&self) -> Result<bool> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::IsConnected { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn connection_state(&self) -> Result<ConnectionState> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetConnectionState { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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

    pub fn recent_timeline_events(&self, limit: usize) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .rev()
            .filter(|evt| Self::is_timeline_like_event(&evt.event))
            .take(capped)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn recent_network_events(&self, limit: usize) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .rev()
            .filter(|evt| Self::is_network_like_event(&evt.event))
            .take(capped)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn timeline_events_since(
        &self,
        from_sequence_id: u64,
        limit: usize,
    ) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .filter(|evt| {
                evt.sequence_id > from_sequence_id && Self::is_timeline_like_event(&evt.event)
            })
            .take(capped)
            .cloned()
            .collect()
    }

    pub fn network_events_since(
        &self,
        from_sequence_id: u64,
        limit: usize,
    ) -> Vec<SequencedSdkEvent> {
        let capped = limit.min(self.event_history_limit);
        if capped == 0 {
            return vec![];
        }
        let locked = self.event_history.lock().expect("event history poisoned");
        locked
            .iter()
            .filter(|evt| {
                evt.sequence_id > from_sequence_id && Self::is_network_like_event(&evt.event)
            })
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn set_network_hint(&self, hint: NetworkHint) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetNetworkHint {
                hint,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn set_video_process_hook(&self, hook: Option<VideoProcessHook>) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetVideoProcessHook {
                hook,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        let out = resp_rx.await.map_err(|_| self.actor_channel_error())?;
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        let out = resp_rx.await.map_err(|_| self.actor_channel_error())?;
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn sync_all_channels(&self) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SyncAllChannels { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn is_bootstrap_completed(&self) -> Result<bool> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::IsBootstrapCompleted { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn run_bootstrap_sync(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RunBootstrapSync { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn session_snapshot(&self) -> Result<Option<SessionSnapshot>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetSessionSnapshot { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn clear_local_state(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ClearLocalState { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
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
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn kv_get(&self, key: String) -> Result<Option<Vec<u8>>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::KvGet { key, resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn kv_scan_prefix(&self, prefix: String) -> Result<Vec<(String, Vec<u8>)>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::KvScanPrefix {
                prefix,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn user_storage_paths(&self) -> Result<UserStoragePaths> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetUserStoragePaths { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn list_local_accounts(&self) -> Result<Vec<LocalAccountSummary>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListLocalAccounts { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn set_current_uid(&self, uid: String) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetCurrentUid { uid, resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn wipe_current_user_full(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::WipeCurrentUserFull { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }
}

#[cfg(test)]
mod tests {
    use super::{
        error_codes, Action, ConnectionState, Error, PrivchatConfig, SdkEvent, SessionState, State,
    };
    use privchat_protocol::PushMessageRequest;

    #[test]
    fn state_machine_allows_token_auth_restore_from_connected() {
        assert!(matches!(
            SessionState::New.can(Action::Login),
            Err(Error::InvalidState(_))
        ));
        assert!(matches!(
            SessionState::Connected.can(Action::Authenticate),
            Ok(SessionState::Authenticated)
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

    #[test]
    fn reconnect_action_is_idempotent_for_connected_states() {
        assert!(matches!(
            SessionState::Connected.can(Action::Connect),
            Ok(SessionState::Connected)
        ));
        assert!(matches!(
            SessionState::LoggedIn.can(Action::Connect),
            Ok(SessionState::Connected)
        ));
        assert!(matches!(
            SessionState::Authenticated.can(Action::Connect),
            Ok(SessionState::Connected)
        ));
    }

    #[test]
    fn push_message_maps_into_sync_entity_item() {
        let push = PushMessageRequest {
            server_message_id: 900001,
            message_seq: 77,
            local_message_id: 12345,
            channel_id: 100,
            channel_type: 1,
            from_uid: 100001,
            message_type: 1,
            payload: br#"{"content":"hello"}"#.to_vec(),
            ..PushMessageRequest::default()
        };
        let item = State::push_message_to_sync_item(push);
        assert_eq!(item.entity_id, "900001");
        assert_eq!(item.version, 77);
        let payload = item.payload.expect("payload");
        assert_eq!(
            payload
                .get("server_message_id")
                .and_then(|v| v.as_u64())
                .unwrap_or_default(),
            900001
        );
        assert_eq!(
            payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default(),
            "hello"
        );
    }

    #[test]
    fn scoped_single_entity_sync_does_not_persist_cursor() {
        assert!(!State::should_persist_sync_cursor("user", Some("10001")));
        assert!(!State::should_persist_sync_cursor("group", Some("20001")));
        assert!(!State::should_persist_sync_cursor(
            "channel",
            Some("1:30001")
        ));

        assert!(State::should_persist_sync_cursor(
            "group_member",
            Some("20001")
        ));
        assert!(State::should_persist_sync_cursor("friend", None));
        assert!(State::should_persist_sync_cursor("channel", None));
    }

    #[test]
    fn stable_sdk_error_codes_are_mapped() {
        assert_eq!(
            Error::Transport("x".to_string()).sdk_code(),
            error_codes::TRANSPORT_FAILURE
        );
        assert_eq!(Error::NotConnected.sdk_code(), error_codes::NETWORK_DISCONNECTED);
        assert_eq!(
            Error::Storage("x".to_string()).sdk_code(),
            error_codes::STORAGE_FAILURE
        );
        assert_eq!(Error::Auth("x".to_string()).sdk_code(), error_codes::AUTH_FAILURE);
        assert_eq!(Error::Shutdown.sdk_code(), error_codes::SHUTDOWN);
        assert_eq!(
            Error::InvalidState("x".to_string()).sdk_code(),
            error_codes::INVALID_STATE
        );
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
        assert!(
            got_state_change,
            "missing ConnectionStateChanged(... -> Shutdown)"
        );
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
            events
                .iter()
                .any(|e| matches!(e.event, SdkEvent::ShutdownStarted)),
            "replay missing ShutdownStarted"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, SdkEvent::ShutdownCompleted)),
            "replay missing ShutdownCompleted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_is_idempotent_and_rejects_new_work() {
        let sdk = super::PrivchatSdk::new(PrivchatConfig::default());
        sdk.shutdown().await;
        sdk.shutdown().await;

        let err = sdk
            .connect()
            .await
            .expect_err("connect should fail after shutdown");
        assert!(matches!(err, Error::Shutdown));
    }
}
