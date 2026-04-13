// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
use privchat_protocol::message::MessagePayloadEnvelope;
use privchat_protocol::presence::{
    PresenceBatchStatusRequest, PresenceBatchStatusResponse, PresenceChangedNotification,
    TypingActionType as ProtoTypingActionType, TypingIndicatorRequest,
};
use privchat_protocol::rpc::auth::{AuthLoginRequest, AuthResponse, UserRegisterRequest};
use privchat_protocol::rpc::contact::friend::FriendPendingResponse;
use privchat_protocol::rpc::file::upload::{
    FileRequestUploadTokenRequest, FileRequestUploadTokenResponse,
};
use privchat_protocol::rpc::message::history::{MessageHistoryGetRequest, MessageHistoryResponse};
use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::sync::{
    ChannelExtraSyncPayload, ChannelMemberSyncPayload, ChannelReadCursorSyncPayload,
    ChannelSyncPayload, FriendSyncPayload, GetChannelPtsRequest, GetChannelPtsResponse,
    GetDifferenceRequest, GetDifferenceResponse, GroupMemberSyncPayload, GroupSyncPayload,
    MessageStatusSyncPayload, MessageSyncPayload, SyncEntityItem,
};
use privchat_protocol::{
    decode_message, encode_message, AuthType, AuthorizationRequest, AuthorizationResponse,
    ClientInfo, ContentMessageType, DeviceInfo, DeviceType, DisconnectRequest, DisconnectResponse,
    ErrorCode, MessageType, PingRequest, PongResponse, PublishRequest, PublishResponse,
    PushBatchRequest, PushBatchResponse, PushMessageRequest, PushMessageResponse, RpcRequest,
    RpcResponse, SendMessageRequest, SendMessageResponse, SubscribeRequest, SubscribeResponse,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

pub mod error_codes;
mod local_store;
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

fn env_var_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(target_os = "android")]
fn android_system_property(key: &str) -> Option<String> {
    use std::ffi::{CStr, CString};

    unsafe extern "C" {
        fn __system_property_get(
            name: *const libc::c_char,
            value: *mut libc::c_char,
        ) -> libc::c_int;
    }

    let c_key = CString::new(key).ok()?;
    let mut buf = [0 as libc::c_char; 92];
    let len = unsafe { __system_property_get(c_key.as_ptr(), buf.as_mut_ptr()) };
    if len <= 0 {
        return None;
    }
    let value = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .trim()
        .to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(not(target_os = "android"))]
fn android_system_property(_key: &str) -> Option<String> {
    None
}

fn default_client_type(os: &str) -> String {
    env_var_trimmed("PRIVCHAT_CLIENT_TYPE").unwrap_or_else(|| os.to_string())
}

fn default_app_id(os: &str) -> String {
    env_var_trimmed("PRIVCHAT_APP_ID").unwrap_or_else(|| format!("com.privchat.{os}"))
}

fn default_app_package(os: &str) -> Option<String> {
    Some(env_var_trimmed("PRIVCHAT_APP_PACKAGE").unwrap_or_else(|| default_app_id(os)))
}

fn default_device_model() -> Option<String> {
    env_var_trimmed("PRIVCHAT_DEVICE_MODEL").or_else(|| {
        #[cfg(target_os = "android")]
        {
            android_system_property("ro.product.marketname")
                .or_else(|| android_system_property("ro.product.model"))
        }
        #[cfg(not(target_os = "android"))]
        {
            None
        }
    })
}

fn default_manufacturer() -> Option<String> {
    env_var_trimmed("PRIVCHAT_DEVICE_MANUFACTURER").or_else(|| {
        #[cfg(target_os = "android")]
        {
            android_system_property("ro.product.manufacturer")
        }
        #[cfg(not(target_os = "android"))]
        {
            None
        }
    })
}

fn default_device_name(os: &str) -> String {
    if let Some(name) = env_var_trimmed("PRIVCHAT_DEVICE_NAME") {
        return name;
    }
    if let Some(model) = default_device_model() {
        if let Some(manufacturer) = default_manufacturer() {
            let manufacturer_lower = manufacturer.to_lowercase();
            let model_lower = model.to_lowercase();
            if model_lower.starts_with(&manufacturer_lower) {
                return model;
            }
            return format!("{manufacturer} {model}");
        }
        return model;
    }
    format!("privchat-{os}")
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
    pub expires_at: u64,
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
    ResumeSyncStarted,
    ResumeSyncCompleted {
        entity_types_synced: usize,
        channels_scanned: usize,
        channels_applied: usize,
        channel_failures: usize,
    },
    ResumeSyncFailed {
        classification: ResumeFailureClass,
        scope: ResumeEscalationScope,
        error_code: u32,
        message: String,
    },
    ResumeSyncEscalated {
        classification: ResumeFailureClass,
        scope: ResumeEscalationScope,
        reason: String,
        entity_type: Option<String>,
        channel_id: Option<u64>,
        channel_type: Option<i32>,
    },
    ResumeSyncChannelStarted {
        channel_id: u64,
        channel_type: i32,
    },
    ResumeSyncChannelCompleted {
        channel_id: u64,
        channel_type: i32,
        applied: usize,
    },
    ResumeSyncChannelFailed {
        channel_id: u64,
        channel_type: i32,
        classification: ResumeFailureClass,
        scope: ResumeEscalationScope,
        error_code: u32,
        message: String,
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
    SubscriptionMessageReceived {
        channel_id: u64,
        topic: Option<String>,
        payload: Vec<u8>,
        publisher: Option<String>,
        server_message_id: Option<u64>,
        timestamp: u64,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ChannelPrefsState {
    #[serde(default)]
    notification_mode: i32,
    #[serde(default)]
    favourite: bool,
    #[serde(default)]
    low_priority: bool,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GroupSettingsCache {
    #[serde(default)]
    group_id: u64,
    #[serde(default)]
    mute_all: bool,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

fn channel_prefs_key(channel_id: u64, channel_type: i32) -> String {
    format!("__channel_prefs__:{channel_id}:{channel_type}")
}

fn group_settings_key(group_id: u64) -> String {
    format!("__group_settings__:{group_id}")
}

fn decode_channel_prefs(raw: Option<Vec<u8>>) -> ChannelPrefsState {
    raw.and_then(|b| serde_json::from_slice::<ChannelPrefsState>(&b).ok())
        .unwrap_or_default()
}

fn decode_group_settings_cache(raw: Option<Vec<u8>>) -> GroupSettingsCache {
    raw.and_then(|b| serde_json::from_slice::<GroupSettingsCache>(&b).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResumeFailureClass {
    RetryableTemporaryError,
    ChannelResyncRequired,
    EntityResyncRequired,
    FullRebuildRequired,
    FatalProtocolError,
}

impl ResumeFailureClass {
    pub fn sdk_code(self) -> u32 {
        match self {
            ResumeFailureClass::RetryableTemporaryError => error_codes::RESUME_RETRYABLE_TEMPORARY,
            ResumeFailureClass::ChannelResyncRequired => {
                error_codes::RESUME_CHANNEL_RESYNC_REQUIRED
            }
            ResumeFailureClass::EntityResyncRequired => error_codes::RESUME_ENTITY_RESYNC_REQUIRED,
            ResumeFailureClass::FullRebuildRequired => error_codes::RESUME_FULL_REBUILD_REQUIRED,
            ResumeFailureClass::FatalProtocolError => error_codes::RESUME_FATAL_PROTOCOL_ERROR,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResumeEscalationScope {
    Retry,
    ChannelScopedResync,
    EntityScopedResync,
    FullRebuild,
}

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
    file_url: String,
    thumbnail_url: Option<String>,
    file_size: u64,
    original_size: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    mime_type: String,
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

fn inbound_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_INBOUND_LOG").ok().as_deref() == Some("1"))
}

fn rpc_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_RPC_LOG").ok().as_deref() == Some("1"))
}

fn actor_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_ACTOR_LOG").ok().as_deref() == Some("1"))
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
        loop {
            match event_rx.recv().await {
                Ok(ClientEvent::MessageReceived(context)) => {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] message received biz_type={} len={}",
                            context.biz_type,
                            context.data.len()
                        );
                    }
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
                Ok(ClientEvent::Disconnected { .. }) => {
                    eprintln!("[SDK.inbound] transport disconnected");
                    let _ = actor_tx.send(Command::InboundDisconnected).await;
                    break;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(skipped)) => {
                    eprintln!(
                        "[SDK.inbound] event stream lagged, skipped={} (continue)",
                        skipped
                    );
                }
                Err(RecvError::Closed) => {
                    eprintln!("[SDK.inbound] event stream closed");
                    break;
                }
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
    pub is_online: bool,
    pub last_seen_at: i64,
    pub device_count: u32,
    pub version: u64,
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
pub struct UpsertRemoteMessageResult {
    pub message_id: u64,
    pub inserted_new: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub message_id: u64,
    pub server_message_id: Option<u64>,
    pub local_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub extra: String,
    pub revoked: bool,
    pub revoked_by: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineSnapshot {
    pub messages: Vec<StoredMessage>,
    pub newest_message_id: Option<u64>,
    pub oldest_message_id: Option<u64>,
    pub has_more_before: bool,
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageCachePolicyConfig {
    pub per_channel_budget_bytes: u32,
    pub global_budget_bytes: u32,
    pub min_messages: u16,
    pub max_messages: u16,
}

impl Default for MessageCachePolicyConfig {
    fn default() -> Self {
        Self {
            per_channel_budget_bytes: 64 * 1024,
            global_budget_bytes: 8 * 1024 * 1024,
            min_messages: 10,
            max_messages: 200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageCachePolicy {
    Disabled,
    Enabled(MessageCachePolicyConfig),
}

impl Default for MessageCachePolicy {
    fn default() -> Self {
        Self::Enabled(MessageCachePolicyConfig::default())
    }
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
    pub version: i64,
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
    pub version: i64,
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
    pub version: i64,
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
    pub version: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertFriendInput {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub version: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredFriend {
    pub user_id: u64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub alias: Option<String>,
    pub avatar: String,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub version: i64,
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
    pub version: i64,
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
    pub version: i64,
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
    pub version: i64,
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
    pub version: i64,
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
    BatchGetPresence {
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
    Subscribe {
        channel_id: u64,
        channel_type: u8,
        token: Option<String>,
        resp: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        channel_id: u64,
        channel_type: u8,
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
    KickOutboundDrain,
    CreateLocalMessage {
        input: NewMessage,
        local_message_id: Option<u64>,
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
    QueryTimelineSnapshot {
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<TimelineSnapshot>>,
    },
    SetMessageCachePolicy {
        policy: MessageCachePolicy,
        resp: oneshot::Sender<Result<()>>,
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
    ProjectChannelReadCursor {
        channel_id: u64,
        channel_type: i32,
        last_read_pts: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ChannelCacheKey {
    channel_id: u64,
    channel_type: i32,
}

#[derive(Debug, Clone)]
struct ChannelMessageCache {
    messages: VecDeque<StoredMessage>,
    estimated_bytes: usize,
    has_more_before: bool,
}

#[derive(Debug, Clone)]
enum ResumeFailureTarget {
    Global,
    EntityType(String),
    Channel { channel_id: u64, channel_type: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumeFailureHandling {
    Continue,
    Abort,
}

#[derive(Debug, Default, Clone, Copy)]
struct ResumeRunStats {
    entity_types_synced: usize,
    channels_scanned: usize,
    channels_applied: usize,
    channel_failures: usize,
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
    message_cache_policy: MessageCachePolicy,
    channel_message_cache: HashMap<ChannelCacheKey, ChannelMessageCache>,
    channel_cache_generation: HashMap<ChannelCacheKey, u64>,
    channel_cache_lru: VecDeque<ChannelCacheKey>,
    channel_cache_total_bytes: usize,
    cache_debug_log: bool,
    cache_hit_count: u64,
    cache_miss_count: u64,
    pending_prelogin_inbound_frames: Vec<(u8, Vec<u8>)>,
    presence_cache: Arc<StdMutex<HashMap<u64, PresenceStatus>>>,
}

impl State {
    fn clear_presence_cache(&self) {
        if let Ok(mut locked) = self.presence_cache.lock() {
            locked.clear();
        }
    }

    fn update_presence_cache(&self, items: &[PresenceStatus]) {
        if let Ok(mut locked) = self.presence_cache.lock() {
            for item in items {
                match locked.get(&item.user_id) {
                    Some(existing) if existing.version >= item.version => {}
                    _ => {
                        locked.insert(item.user_id, item.clone());
                    }
                }
            }
        }
    }

    fn apply_presence_changed_payload(&self, payload: &[u8]) {
        let Ok(notification) = serde_json::from_slice::<PresenceChangedNotification>(payload)
        else {
            return;
        };
        let snapshot = notification.snapshot;
        self.update_presence_cache(&[PresenceStatus {
            user_id: snapshot.user_id,
            is_online: snapshot.is_online,
            last_seen_at: snapshot.last_seen_at,
            device_count: snapshot.device_count,
            version: snapshot.version,
        }]);
    }

    fn cache_presence_response(
        &self,
        response: PresenceBatchStatusResponse,
    ) -> Vec<PresenceStatus> {
        let mut out: Vec<PresenceStatus> = response
            .items
            .into_iter()
            .map(|snapshot| PresenceStatus {
                user_id: snapshot.user_id,
                is_online: snapshot.is_online,
                last_seen_at: snapshot.last_seen_at,
                device_count: snapshot.device_count,
                version: snapshot.version,
            })
            .collect();
        out.sort_by_key(|v| v.user_id);
        self.update_presence_cache(&out);
        out
    }

    fn sync_version_key(entity_type: &str, scope: Option<&str>) -> String {
        let scope_part = scope.unwrap_or("*");
        format!("__sync_version__:{entity_type}:{scope_part}")
    }

    fn resume_repair_channel_key(channel_id: u64, channel_type: i32) -> String {
        format!("__resume_repair__:channel:{channel_type}:{channel_id}")
    }

    fn resume_repair_entity_key(entity_type: &str) -> String {
        format!("__resume_repair__:entity:{entity_type}")
    }

    fn resume_repair_full_rebuild_key() -> String {
        "__resume_repair__:full_rebuild".to_string()
    }

    fn resume_channel_pts_key(channel_id: u64, channel_type: i32) -> String {
        format!("__resume_pts__:{channel_type}:{channel_id}")
    }

    fn resume_repair_payload(classification: ResumeFailureClass, reason: &str) -> Vec<u8> {
        serde_json::json!({
            "classification": classification,
            "reason": reason,
            "timestamp_ms": chrono::Utc::now().timestamp_millis(),
        })
        .to_string()
        .into_bytes()
    }

    async fn clear_resume_repair_key(&self, key: String) {
        if let Err(err) = self.storage.kv_delete(key).await {
            eprintln!("[SDK.actor] clear resume repair marker failed: {err}");
        }
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

    async fn load_resume_channel_pts(&self, channel_id: u64, channel_type: i32) -> Option<u64> {
        let key = Self::resume_channel_pts_key(channel_id, channel_type);
        let raw = self.storage.kv_get(key).await.ok().flatten()?;
        let text = String::from_utf8(raw).ok()?;
        text.trim().parse::<u64>().ok()
    }

    async fn save_resume_channel_pts(
        &self,
        channel_id: u64,
        channel_type: i32,
        pts: u64,
    ) -> Result<()> {
        let key = Self::resume_channel_pts_key(channel_id, channel_type);
        self.storage.kv_put(key, pts.to_string().into_bytes()).await
    }

    #[cfg(test)]
    fn should_apply_entity_version(existing_version: Option<u64>, incoming_version: u64) -> bool {
        existing_version
            .map(|v| incoming_version >= v)
            .unwrap_or(true)
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

    fn classify_resume_error(err: &Error) -> ResumeFailureClass {
        match err {
            Error::Transport(_) | Error::NotConnected | Error::ActorClosed => {
                ResumeFailureClass::RetryableTemporaryError
            }
            Error::Serialization(_) => ResumeFailureClass::FatalProtocolError,
            Error::Storage(_) => ResumeFailureClass::FullRebuildRequired,
            Error::Auth(message) => {
                Self::classify_resume_message(message, ResumeFailureClass::FullRebuildRequired)
            }
            Error::Shutdown => ResumeFailureClass::RetryableTemporaryError,
            Error::InvalidState(message) => {
                let lowered = message.to_ascii_lowercase();
                if lowered.contains("session_ready rejected")
                    || lowered.contains("requires authenticated")
                {
                    ResumeFailureClass::FullRebuildRequired
                } else if lowered.contains("paging stalled")
                    || lowered.contains("since_version")
                    || lowered.contains("entity resync")
                {
                    ResumeFailureClass::EntityResyncRequired
                } else if lowered.contains("max paging iterations")
                    || lowered.contains("last_pts")
                    || lowered.contains("gap")
                    || lowered.contains("channel resync")
                {
                    ResumeFailureClass::ChannelResyncRequired
                } else {
                    ResumeFailureClass::FatalProtocolError
                }
            }
        }
    }

    fn classify_resume_message(message: &str, default: ResumeFailureClass) -> ResumeFailureClass {
        let lowered = message.to_ascii_lowercase();
        if lowered.contains("code=20900")
            || lowered.contains("syncchannelresyncrequired")
            || lowered.contains("channel scoped resync required")
        {
            ResumeFailureClass::ChannelResyncRequired
        } else if lowered.contains("code=20901")
            || lowered.contains("syncentityresyncrequired")
            || lowered.contains("entity scoped resync required")
        {
            ResumeFailureClass::EntityResyncRequired
        } else if lowered.contains("code=20902")
            || lowered.contains("syncfullrebuildrequired")
            || lowered.contains("full rebuild required")
        {
            ResumeFailureClass::FullRebuildRequired
        } else if lowered.contains("pts too old")
            || lowered.contains("gap")
            || lowered.contains("channel resync")
            || lowered.contains("message history window")
            || lowered.contains("last_pts")
        {
            ResumeFailureClass::ChannelResyncRequired
        } else if lowered.contains("entity resync")
            || lowered.contains("since_version")
            || lowered.contains("version too old")
        {
            ResumeFailureClass::EntityResyncRequired
        } else if lowered.contains("resume token invalid")
            || lowered.contains("session not found")
            || lowered.contains("invalid token")
            || lowered.contains("full rebuild")
            || lowered.contains("bootstrap")
            || lowered.contains("session_ready rejected")
        {
            ResumeFailureClass::FullRebuildRequired
        } else if lowered.contains("decode") || lowered.contains("schema") {
            ResumeFailureClass::FatalProtocolError
        } else {
            default
        }
    }

    fn sync_rpc_rejection(op: &str, code: i32, message: String) -> Error {
        let protocol_code = u32::try_from(code).ok().and_then(ErrorCode::from_code);
        match protocol_code {
            Some(ErrorCode::SyncChannelResyncRequired) => Error::InvalidState(format!(
                "{op} channel resync required: code={code} message={message}"
            )),
            Some(ErrorCode::SyncEntityResyncRequired) => Error::InvalidState(format!(
                "{op} entity resync required: code={code} message={message}"
            )),
            Some(ErrorCode::SyncFullRebuildRequired) => Error::Auth(format!(
                "{op} full rebuild required: code={code} message={message}"
            )),
            Some(ErrorCode::ProtocolError)
            | Some(ErrorCode::DecodingError)
            | Some(ErrorCode::EncodingError) => Error::InvalidState(format!(
                "{op} protocol error: code={code} message={message}"
            )),
            _ => Error::Auth(format!("{op} rejected: code={code} message={message}")),
        }
    }

    fn resume_escalation_scope(
        classification: ResumeFailureClass,
        target: &ResumeFailureTarget,
    ) -> ResumeEscalationScope {
        match classification {
            ResumeFailureClass::RetryableTemporaryError => ResumeEscalationScope::Retry,
            ResumeFailureClass::ChannelResyncRequired => ResumeEscalationScope::ChannelScopedResync,
            ResumeFailureClass::EntityResyncRequired => ResumeEscalationScope::EntityScopedResync,
            ResumeFailureClass::FullRebuildRequired => ResumeEscalationScope::FullRebuild,
            ResumeFailureClass::FatalProtocolError => match target {
                ResumeFailureTarget::Channel { .. } => ResumeEscalationScope::ChannelScopedResync,
                ResumeFailureTarget::EntityType(_) => ResumeEscalationScope::EntityScopedResync,
                ResumeFailureTarget::Global => ResumeEscalationScope::FullRebuild,
            },
        }
    }

    fn queue_resume_started(&mut self) {
        self.pending_events.push(SdkEvent::ResumeSyncStarted);
    }

    fn queue_resume_completed(&mut self, stats: ResumeRunStats) {
        self.pending_events.push(SdkEvent::ResumeSyncCompleted {
            entity_types_synced: stats.entity_types_synced,
            channels_scanned: stats.channels_scanned,
            channels_applied: stats.channels_applied,
            channel_failures: stats.channel_failures,
        });
    }

    async fn execute_channel_scoped_resync(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        classification: ResumeFailureClass,
        reason: &str,
    ) -> Result<()> {
        let key = Self::resume_repair_channel_key(channel_id, channel_type);
        self.storage
            .kv_put(
                key.clone(),
                Self::resume_repair_payload(classification, reason),
            )
            .await?;
        match self.sync_channel(channel_id, channel_type).await {
            Ok(applied) => {
                self.storage.kv_delete(key).await?;
                self.pending_events.push(SdkEvent::SyncChannelApplied {
                    channel_id,
                    channel_type,
                    applied,
                });
                Ok(())
            }
            Err(err) => {
                eprintln!(
                    "[SDK.actor] channel scoped resync failed: channel_id={} channel_type={} err={}",
                    channel_id, channel_type, err
                );
                Err(err)
            }
        }
    }

    async fn execute_entity_scoped_resync(
        &mut self,
        entity_type: &str,
        classification: ResumeFailureClass,
        reason: &str,
    ) -> Result<()> {
        let key = Self::resume_repair_entity_key(entity_type);
        self.storage
            .kv_put(
                key.clone(),
                Self::resume_repair_payload(classification, reason),
            )
            .await?;
        match self.sync_entities(entity_type.to_string(), None).await {
            Ok(applied) => {
                self.storage.kv_delete(key).await?;
                self.queue_last_sync_events(entity_type.to_string(), None, applied);
                Ok(())
            }
            Err(err) => {
                eprintln!(
                    "[SDK.actor] entity scoped resync failed: entity_type={} err={}",
                    entity_type, err
                );
                Err(err)
            }
        }
    }

    async fn execute_full_rebuild_required(
        &mut self,
        classification: ResumeFailureClass,
        reason: &str,
    ) -> Result<()> {
        let key = Self::resume_repair_full_rebuild_key();
        self.storage
            .kv_put(key, Self::resume_repair_payload(classification, reason))
            .await?;
        self.bootstrap_completed = false;
        if let Some(uid) = &self.current_uid {
            self.storage
                .set_bootstrap_completed(uid.clone(), false)
                .await?;
        }
        Ok(())
    }

    async fn handle_resume_failure(
        &mut self,
        target: ResumeFailureTarget,
        err: &Error,
    ) -> ResumeFailureHandling {
        let classification = Self::classify_resume_error(err);
        let scope = Self::resume_escalation_scope(classification, &target);
        let message = err.to_string();

        match &target {
            ResumeFailureTarget::Channel {
                channel_id,
                channel_type,
            } => {
                self.pending_events.push(SdkEvent::ResumeSyncChannelFailed {
                    channel_id: *channel_id,
                    channel_type: *channel_type,
                    classification,
                    scope,
                    error_code: classification.sdk_code(),
                    message: message.clone(),
                });
            }
            ResumeFailureTarget::EntityType(_) | ResumeFailureTarget::Global => {}
        }

        let (entity_type, channel_id, channel_type) = match &target {
            ResumeFailureTarget::Global => (None, None, None),
            ResumeFailureTarget::EntityType(entity_type) => (Some(entity_type.clone()), None, None),
            ResumeFailureTarget::Channel {
                channel_id,
                channel_type,
            } => (None, Some(*channel_id), Some(*channel_type)),
        };
        self.pending_events.push(SdkEvent::ResumeSyncEscalated {
            classification,
            scope,
            reason: message.clone(),
            entity_type,
            channel_id,
            channel_type,
        });

        match scope {
            ResumeEscalationScope::ChannelScopedResync => {
                if let ResumeFailureTarget::Channel {
                    channel_id,
                    channel_type,
                } = target
                {
                    if let Err(resync_err) = self
                        .execute_channel_scoped_resync(
                            channel_id,
                            channel_type,
                            classification,
                            &message,
                        )
                        .await
                    {
                        self.pending_events.push(SdkEvent::ResumeSyncFailed {
                            classification,
                            scope,
                            error_code: classification.sdk_code(),
                            message: resync_err.to_string(),
                        });
                    }
                }
                ResumeFailureHandling::Continue
            }
            ResumeEscalationScope::EntityScopedResync => {
                if let ResumeFailureTarget::EntityType(entity_type) = target {
                    match self
                        .execute_entity_scoped_resync(&entity_type, classification, &message)
                        .await
                    {
                        Ok(()) => ResumeFailureHandling::Continue,
                        Err(resync_err) => {
                            self.pending_events.push(SdkEvent::ResumeSyncFailed {
                                classification,
                                scope,
                                error_code: classification.sdk_code(),
                                message: resync_err.to_string(),
                            });
                            ResumeFailureHandling::Abort
                        }
                    }
                } else {
                    self.pending_events.push(SdkEvent::ResumeSyncFailed {
                        classification,
                        scope,
                        error_code: classification.sdk_code(),
                        message,
                    });
                    ResumeFailureHandling::Abort
                }
            }
            ResumeEscalationScope::Retry | ResumeEscalationScope::FullRebuild => {
                if scope == ResumeEscalationScope::FullRebuild {
                    if let Err(rebuild_err) = self
                        .execute_full_rebuild_required(classification, &message)
                        .await
                    {
                        self.pending_events.push(SdkEvent::ResumeSyncFailed {
                            classification,
                            scope,
                            error_code: classification.sdk_code(),
                            message: rebuild_err.to_string(),
                        });
                        return ResumeFailureHandling::Abort;
                    }
                }
                self.pending_events.push(SdkEvent::ResumeSyncFailed {
                    classification,
                    scope,
                    error_code: classification.sdk_code(),
                    message,
                });
                ResumeFailureHandling::Abort
            }
        }
    }

    fn queue_last_sync_events(
        &mut self,
        event_entity_type: String,
        event_scope: Option<String>,
        applied: usize,
    ) {
        self.pending_events
            .extend(self.last_sync_entity_events.iter().cloned());
        self.pending_events.push(SdkEvent::SyncEntitiesApplied {
            entity_type: event_entity_type,
            scope: event_scope,
            queued: self.last_sync_queued,
            applied,
            dropped_duplicates: self.last_sync_dropped_duplicates,
        });
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

    fn cache_key(channel_id: u64, channel_type: i32) -> ChannelCacheKey {
        ChannelCacheKey {
            channel_id,
            channel_type,
        }
    }

    fn cache_config(&self) -> Option<&MessageCachePolicyConfig> {
        match &self.message_cache_policy {
            MessageCachePolicy::Disabled => None,
            MessageCachePolicy::Enabled(cfg) => Some(cfg),
        }
    }

    fn estimate_message_bytes(message: &StoredMessage) -> usize {
        // Keep this lightweight and stable: string payloads + fixed object overhead.
        96 + message.content.len() + message.extra.len()
    }

    fn touch_cache_lru(&mut self, key: ChannelCacheKey) {
        if let Some(pos) = self.channel_cache_lru.iter().position(|k| *k == key) {
            self.channel_cache_lru.remove(pos);
        }
        self.channel_cache_lru.push_back(key);
    }

    fn evict_channel_cache(&mut self, key: ChannelCacheKey) {
        if let Some(removed) = self.channel_message_cache.remove(&key) {
            self.channel_cache_total_bytes = self
                .channel_cache_total_bytes
                .saturating_sub(removed.estimated_bytes);
        }
        if let Some(pos) = self.channel_cache_lru.iter().position(|k| *k == key) {
            self.channel_cache_lru.remove(pos);
        }
    }

    fn enforce_global_cache_budget(&mut self) {
        let Some(global_budget) = self
            .cache_config()
            .map(|cfg| usize::try_from(cfg.global_budget_bytes).unwrap_or(usize::MAX))
        else {
            return;
        };
        while self.channel_cache_total_bytes > global_budget {
            let Some(oldest) = self.channel_cache_lru.pop_front() else {
                break;
            };
            if let Some(removed) = self.channel_message_cache.remove(&oldest) {
                self.channel_cache_total_bytes = self
                    .channel_cache_total_bytes
                    .saturating_sub(removed.estimated_bytes);
            }
        }
    }

    fn invalidate_channel_cache_with_reason(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        reason: &str,
    ) {
        let key = Self::cache_key(channel_id, channel_type);
        let next_gen = self
            .channel_cache_generation
            .get(&key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.channel_cache_generation.insert(key, next_gen);
        self.evict_channel_cache(key);
        if self.cache_debug_log {
            eprintln!(
                "[SDK.cache] invalidate channel={}:{} reason={} gen={}",
                channel_type, channel_id, reason, next_gen
            );
        }
    }

    fn invalidate_channel_cache(&mut self, channel_id: u64, channel_type: i32) {
        self.invalidate_channel_cache_with_reason(channel_id, channel_type, "manual");
    }

    fn invalidate_cache_for_events(&mut self, events: &[SdkEvent]) {
        for event in events {
            match event {
                SdkEvent::TimelineUpdated {
                    channel_id,
                    channel_type,
                    ..
                } => self.invalidate_channel_cache_with_reason(
                    *channel_id,
                    *channel_type,
                    "event_apply",
                ),
                _ => {}
            }
        }
    }

    fn store_channel_cache(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        mut messages: Vec<StoredMessage>,
        has_more_before: bool,
    ) {
        let Some(config) = self.cache_config().cloned() else {
            return;
        };
        if messages.is_empty() {
            self.invalidate_channel_cache(channel_id, channel_type);
            return;
        }
        let max_messages = usize::from(config.max_messages.max(1));
        if messages.len() > max_messages {
            messages.truncate(max_messages);
        }
        let min_messages = usize::from(config.min_messages.min(config.max_messages).max(1));
        let per_budget = usize::try_from(config.per_channel_budget_bytes).unwrap_or(usize::MAX);
        let mut deque: VecDeque<StoredMessage> = VecDeque::with_capacity(messages.len());
        let mut bytes = 0usize;
        for message in messages {
            bytes = bytes.saturating_add(Self::estimate_message_bytes(&message));
            deque.push_back(message);
            while deque.len() > min_messages && (deque.len() > max_messages || bytes > per_budget) {
                if let Some(old) = deque.pop_back() {
                    bytes = bytes.saturating_sub(Self::estimate_message_bytes(&old));
                }
            }
        }
        let key = Self::cache_key(channel_id, channel_type);
        self.evict_channel_cache(key);
        self.channel_cache_total_bytes = self.channel_cache_total_bytes.saturating_add(bytes);
        self.channel_message_cache.insert(
            key,
            ChannelMessageCache {
                messages: deque,
                estimated_bytes: bytes,
                has_more_before,
            },
        );
        self.touch_cache_lru(key);
        self.enforce_global_cache_budget();
    }

    fn snapshot_from_cache(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Option<TimelineSnapshot> {
        if offset != 0 {
            return None;
        }
        let _ = self.cache_config()?;
        let key = Self::cache_key(channel_id, channel_type);
        let entry = self.channel_message_cache.get(&key)?;
        let cap = limit.max(1);
        let mut messages: Vec<StoredMessage> = entry.messages.iter().take(cap).cloned().collect();
        if messages.is_empty() {
            return None;
        }
        let newest_message_id = messages.first().map(|m| m.message_id);
        let oldest_message_id = messages.last().map(|m| m.message_id);
        let has_more_before = entry.has_more_before || entry.messages.len() > messages.len();
        self.touch_cache_lru(key);
        self.cache_hit_count = self.cache_hit_count.saturating_add(1);
        if self.cache_debug_log {
            eprintln!(
                "[SDK.cache] hit channel={}:{} limit={} offset={} hit={} miss={}",
                channel_type, channel_id, cap, offset, self.cache_hit_count, self.cache_miss_count
            );
        }
        Some(TimelineSnapshot {
            messages: std::mem::take(&mut messages),
            newest_message_id,
            oldest_message_id,
            has_more_before,
            from_cache: true,
        })
    }

    async fn query_timeline_snapshot(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<TimelineSnapshot> {
        if let Some(snapshot) = self.snapshot_from_cache(channel_id, channel_type, limit, offset) {
            return Ok(snapshot);
        }
        self.cache_miss_count = self.cache_miss_count.saturating_add(1);
        if self.cache_debug_log {
            eprintln!(
                "[SDK.cache] miss channel={}:{} limit={} offset={} hit={} miss={}",
                channel_type,
                channel_id,
                limit,
                offset,
                self.cache_hit_count,
                self.cache_miss_count
            );
        }
        let key = Self::cache_key(channel_id, channel_type);
        let generation_before = self
            .channel_cache_generation
            .get(&key)
            .copied()
            .unwrap_or(0);
        let fetch_limit = limit.max(1).saturating_add(1);
        let mut rows = self
            .storage
            .list_messages(channel_id, channel_type, fetch_limit, offset)
            .await?;
        let mut has_more_before = false;
        let cap = limit.max(1);
        if rows.len() > cap {
            has_more_before = true;
            rows.truncate(cap);
        }
        let generation_after = self
            .channel_cache_generation
            .get(&key)
            .copied()
            .unwrap_or(0);
        if offset == 0 && generation_before == generation_after {
            self.store_channel_cache(channel_id, channel_type, rows.clone(), has_more_before);
        } else if self.cache_debug_log && offset == 0 && generation_before != generation_after {
            eprintln!(
                "[SDK.cache] skip-store stale generation channel={}:{} before={} after={}",
                channel_type, channel_id, generation_before, generation_after
            );
        }
        Ok(TimelineSnapshot {
            newest_message_id: rows.first().map(|m| m.message_id),
            oldest_message_id: rows.last().map(|m| m.message_id),
            messages: rows,
            has_more_before,
            from_cache: false,
        })
    }

    fn set_message_cache_policy(&mut self, policy: MessageCachePolicy) {
        self.message_cache_policy = policy;
        self.channel_message_cache.clear();
        self.channel_cache_generation.clear();
        self.channel_cache_lru.clear();
        self.channel_cache_total_bytes = 0;
        self.cache_hit_count = 0;
        self.cache_miss_count = 0;
    }

    async fn request_bytes(
        &mut self,
        payload: Bytes,
        biz_type: u8,
        timeout: Duration,
        context: &str,
    ) -> Result<Bytes> {
        if biz_type == MessageType::RpcRequest as u8 && rpc_logs_enabled() {
            match decode_message::<RpcRequest>(&payload) {
                Ok(req) => {
                    let body_preview = {
                        let s = req.body.to_string();
                        if s.len() > 8192 {
                            format!("{}...", &s[..8192])
                        } else {
                            s
                        }
                    };
                    eprintln!(
                        "[SDK.rpc] request context={} route={} body={}",
                        context, req.route, body_preview
                    );
                }
                Err(e) => {
                    eprintln!(
                        "[SDK.rpc] request context={} decode_error={} payload_len={}",
                        context,
                        e,
                        payload.len()
                    );
                }
            }
        }
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
            Ok(raw) => {
                if biz_type == MessageType::RpcRequest as u8 && rpc_logs_enabled() {
                    match decode_message::<RpcResponse>(&raw) {
                        Ok(resp) => {
                            let data_preview = resp
                                .data
                                .as_ref()
                                .map(|v| {
                                    let s = v.to_string();
                                    if s.len() > 8192 {
                                        format!("{}...", &s[..8192])
                                    } else {
                                        s
                                    }
                                })
                                .unwrap_or_else(|| "null".to_string());
                            eprintln!(
                                "[SDK.rpc] response context={} code={} message={} data={}",
                                context, resp.code, resp.message, data_preview
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "[SDK.rpc] response context={} decode_error={} payload_len={}",
                                context,
                                e,
                                raw.len()
                            );
                        }
                    }
                }
                Ok(raw)
            }
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

    fn resolve_channel_unread_count(
        existing_unread: Option<i32>,
        synced_unread: Option<i32>,
    ) -> i32 {
        existing_unread
            .unwrap_or_else(|| synced_unread.unwrap_or(0))
            .max(0)
    }

    fn resolve_channel_last_message_fields(
        existing: Option<&StoredChannel>,
        synced_timestamp: Option<i64>,
        synced_content: Option<String>,
    ) -> (i64, String, u64) {
        if let Some(existing) = existing {
            if existing.last_local_message_id > 0 || existing.last_msg_timestamp > 0 {
                return (
                    existing.last_msg_timestamp,
                    existing.last_msg_content.clone(),
                    existing.last_local_message_id,
                );
            }
        }

        (
            synced_timestamp.unwrap_or_default(),
            synced_content.unwrap_or_default(),
            0,
        )
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

    fn should_log_unsupported_entity_skip(entity_type: &str) -> bool {
        !matches!(
            entity_type,
            "user_block" | "channel_extra" | "channel_unread"
        )
    }

    fn log_unsupported_sync_skip(
        &self,
        phase: &str,
        entity_type: &str,
        scope: Option<String>,
        err: &Error,
    ) {
        if !Self::should_log_unsupported_entity_skip(entity_type) {
            return;
        }
        match scope {
            Some(scope) => eprintln!(
                "[SDK.actor] {} skip unsupported entity_type={} scope={} reason={}",
                phase, entity_type, scope, err
            ),
            None => eprintln!(
                "[SDK.actor] {} skip unsupported entity_type={} reason={}",
                phase, entity_type, err
            ),
        }
    }

    fn normalized_message_type_from_str(message_type: &str) -> i32 {
        let normalized = match message_type.trim().to_ascii_lowercase().as_str() {
            "image" => ContentMessageType::Image,
            "file" => ContentMessageType::File,
            "voice" => ContentMessageType::Voice,
            "video" => ContentMessageType::Video,
            "system" => ContentMessageType::System,
            "audio" => ContentMessageType::Audio,
            "location" => ContentMessageType::Location,
            "contact_card" | "contactcard" | "card" => ContentMessageType::ContactCard,
            "sticker" => ContentMessageType::Sticker,
            "forward" => ContentMessageType::Forward,
            _ => ContentMessageType::Text,
        };
        i32::try_from(normalized.as_u32()).unwrap_or(0)
    }

    fn normalized_message_content_and_extra(
        payload: &serde_json::Value,
    ) -> (String, Option<String>) {
        match payload {
            serde_json::Value::Null => (String::new(), None),
            serde_json::Value::String(text) => (text.clone(), None),
            serde_json::Value::Object(_) => (
                Self::json_get_string(payload, &["content"])
                    .or_else(|| Self::json_get_string(payload, &["text"]))
                    .or_else(|| Self::json_get_string(payload, &["body"]))
                    .or_else(|| Self::json_get_string(payload, &["content", "text"]))
                    .or_else(|| Self::json_get_string(payload, &["content", "body"]))
                    .unwrap_or_default(),
                Some(payload.to_string()),
            ),
            serde_json::Value::Array(_) => (String::new(), Some(payload.to_string())),
            other => (other.to_string(), None),
        }
    }

    fn payload_bytes_to_message_content_and_extra(payload: &[u8]) -> (String, Option<String>) {
        if payload.is_empty() {
            return (String::new(), None);
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) {
            return Self::normalized_message_content_and_extra(&value);
        }
        (String::from_utf8_lossy(payload).to_string(), None)
    }

    fn build_message_sync_item(
        server_message_id: u64,
        local_message_id: u64,
        channel_id: u64,
        channel_type: i32,
        timestamp_ms: i64,
        from_uid: u64,
        message_type: i32,
        content: String,
        extra: Option<String>,
        status: i32,
        pts: i64,
        setting: i32,
        order_seq: i64,
        topic: Option<String>,
        stream_no: Option<String>,
        stream_seq: Option<i64>,
        stream_flag: Option<i64>,
        msg_key: Option<String>,
        expire: Option<i64>,
    ) -> SyncEntityItem {
        let payload = serde_json::to_value(MessageSyncPayload {
            server_message_id: Some(server_message_id),
            message_id: Some(server_message_id),
            id: Some(server_message_id),
            local_message_id: Some(local_message_id),
            channel_id: Some(channel_id),
            channel_type: Some(channel_type),
            type_field: Some(channel_type),
            conversation_type: Some(channel_type),
            timestamp: Some(timestamp_ms),
            created_at: Some(timestamp_ms),
            send_time: Some(timestamp_ms),
            from_uid: Some(from_uid),
            sender_id: Some(from_uid),
            from: Some(from_uid),
            uid: Some(from_uid),
            message_type: Some(message_type),
            content: Some(content),
            text: None,
            body: None,
            status: Some(status),
            pts: Some(pts),
            setting: Some(setting),
            order_seq: Some(order_seq),
            searchable_word: None,
            extra,
            topic,
            stream_no,
            stream_seq,
            stream_flag,
            msg_key,
            expire,
        })
        .unwrap_or_else(|_| serde_json::json!({}));
        SyncEntityItem {
            entity_id: server_message_id.to_string(),
            version: u64::try_from(pts.max(0)).unwrap_or(0),
            deleted: false,
            payload: Some(payload),
        }
    }

    fn sync_item_from_difference_commit(
        commit: &privchat_protocol::rpc::sync::ServerCommit,
    ) -> (String, SyncEntityItem) {
        match commit.message_type.as_str() {
            "message.revoke" | "message_extra" | "message_ext" => {
                let mut payload = commit.content.clone();
                if let Some(obj) = payload.as_object_mut() {
                    obj.entry("channel_id".to_string())
                        .or_insert_with(|| serde_json::json!(commit.channel_id));
                    obj.entry("channel_type".to_string())
                        .or_insert_with(|| serde_json::json!(i32::from(commit.channel_type)));
                    obj.entry("message_id".to_string())
                        .or_insert_with(|| serde_json::json!(commit.server_msg_id));
                }
                (
                    "message_extra".to_string(),
                    SyncEntityItem {
                        entity_id: commit.server_msg_id.to_string(),
                        version: commit.pts,
                        deleted: false,
                        payload: Some(payload),
                    },
                )
            }
            "message_reaction" | "reaction" | "message.reaction" => {
                let mut payload = commit.content.clone();
                let entity_id = if let Some(obj) = payload.as_object_mut() {
                    obj.entry("channel_id".to_string())
                        .or_insert_with(|| serde_json::json!(commit.channel_id));
                    obj.entry("channel_type".to_string())
                        .or_insert_with(|| serde_json::json!(i32::from(commit.channel_type)));
                    let message_id = obj
                        .get("message_id")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(commit.server_msg_id);
                    let uid = obj.get("uid").and_then(|v| v.as_u64()).unwrap_or(0);
                    let emoji = obj
                        .get("emoji")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if uid > 0 && !emoji.is_empty() {
                        format!("{message_id}:{uid}:{emoji}")
                    } else {
                        commit.server_msg_id.to_string()
                    }
                } else {
                    commit.server_msg_id.to_string()
                };
                (
                    "message_reaction".to_string(),
                    SyncEntityItem {
                        entity_id,
                        version: commit.pts,
                        deleted: false,
                        payload: Some(payload),
                    },
                )
            }
            _ => {
                let (content, extra) = Self::normalized_message_content_and_extra(&commit.content);
                (
                    "message".to_string(),
                    Self::build_message_sync_item(
                        commit.server_msg_id,
                        commit.local_message_id.unwrap_or(0),
                        commit.channel_id,
                        i32::from(commit.channel_type),
                        commit.server_timestamp,
                        commit.sender_id,
                        Self::normalized_message_type_from_str(&commit.message_type),
                        content,
                        extra,
                        2,
                        i64::try_from(commit.pts).unwrap_or(i64::MAX),
                        0,
                        i64::try_from(commit.pts).unwrap_or(i64::MAX),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ),
                )
            }
        }
    }

    fn push_message_to_sync_item(push: PushMessageRequest) -> SyncEntityItem {
        let deleted = push.deleted;
        let (content, extra) = Self::payload_bytes_to_message_content_and_extra(&push.payload);
        let mut item = Self::build_message_sync_item(
            push.server_message_id,
            push.local_message_id,
            push.channel_id,
            i32::from(push.channel_type),
            i64::from(push.timestamp),
            push.from_uid,
            i32::try_from(push.message_type).unwrap_or(0),
            content,
            extra,
            2,
            i64::from(push.message_seq),
            if push.setting.need_receipt { 1 } else { 0 },
            i64::from(push.message_seq),
            Some(push.topic),
            Some(push.stream_no),
            Some(i64::from(push.stream_seq)),
            Some(i64::from(push.stream_flag)),
            Some(push.msg_key),
            Some(i64::from(push.expire)),
        );
        // 透传 deleted 标志：deleted=true 时 SDK 走 set_message_revoke 路径
        item.deleted = deleted;
        item
    }

    fn json_field_u64(value: &serde_json::Value, path: &[&str]) -> Option<u64> {
        let mut cur = value;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_u64()
            .or_else(|| cur.as_i64().and_then(|v| u64::try_from(v).ok()))
            .or_else(|| cur.as_str().and_then(|s| s.parse::<u64>().ok()))
    }

    fn json_field_string(value: &serde_json::Value, path: &[&str]) -> Option<String> {
        let mut cur = value;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_str().map(|s| s.to_string())
    }

    fn push_message_to_status_sync_item(push: &PushMessageRequest) -> Option<SyncEntityItem> {
        let payload_json: serde_json::Value = serde_json::from_slice(&push.payload).ok()?;
        let notification_type =
            Self::json_field_string(&payload_json, &["metadata", "notification_type"])?;

        match notification_type.as_str() {
            // 已读游标同步通知：走 channel_read_cursor 实体
            "self_read_pts_updated"
            | "peer_read_pts_updated"
            | "user_read_pts"
            | "channel_read_cursor_updated" => {
                let channel_id =
                    Self::json_field_u64(&payload_json, &["metadata", "channel_id"]).unwrap_or(0);
                let read_pts =
                    Self::json_field_u64(&payload_json, &["metadata", "read_pts"]).unwrap_or(0);
                if channel_id == 0 || read_pts == 0 {
                    return None;
                }
                let channel_type =
                    Self::json_field_u64(&payload_json, &["metadata", "channel_type"]).unwrap_or(1);
                let reader_id =
                    Self::json_field_u64(&payload_json, &["metadata", "reader_id"]).unwrap_or(0);
                let updated_at = Self::json_field_u64(&payload_json, &["metadata", "updated_at"])
                    .unwrap_or(u64::from(push.timestamp) * 1000);
                // read cursor 事件在服务器侧当前可能携带 message_seq=0，这里必须给一个单调版本，
                // 否则会被本地投影当作旧版本丢弃，导致多端已读不一致。
                let version = u64::from(push.message_seq)
                    .max(updated_at)
                    .max(read_pts)
                    .max(1);
                let payload = serde_json::json!({
                    "channel_id": channel_id,
                    "channel_type": channel_type as i32,
                    "type": channel_type as i32,
                    "reader_id": reader_id,
                    "last_read_pts": read_pts,
                    "updated_at": i64::try_from(updated_at).unwrap_or(i64::MAX),
                });
                Some(SyncEntityItem {
                    entity_id: format!("{}:{}", channel_id, reader_id),
                    version,
                    deleted: false,
                    payload: Some(payload),
                })
            }
            _ => None,
        }
    }

    fn send_message_to_sync_item(
        req: SendMessageRequest,
        channel_type: u8,
    ) -> Option<SyncEntityItem> {
        if req.local_message_id == 0 {
            return None;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let (content, extra) = Self::payload_bytes_to_message_content_and_extra(&req.payload);
        Some(Self::build_message_sync_item(
            req.local_message_id,
            req.local_message_id,
            req.channel_id,
            i32::from(channel_type),
            now_ms,
            req.from_uid,
            i32::try_from(req.message_type).unwrap_or(0),
            content,
            extra,
            2,
            0,
            if req.setting.need_receipt { 1 } else { 0 },
            0,
            Some(req.topic),
            Some(req.stream_no),
            Some(0),
            Some(0),
            Some(String::new()),
            Some(req.expire as i64),
        ))
    }

    fn log_inbound_decoded(message_type: MessageType, data: &[u8]) {
        if !inbound_logs_enabled() {
            return;
        }
        match message_type {
            MessageType::AuthorizationRequest => {
                if let Ok(v) = decode_message::<AuthorizationRequest>(data) {
                    eprintln!("[SDK.inbound] decoded AuthorizationRequest: {:?}", v);
                }
            }
            MessageType::AuthorizationResponse => {
                if let Ok(v) = decode_message::<AuthorizationResponse>(data) {
                    eprintln!("[SDK.inbound] decoded AuthorizationResponse: {:?}", v);
                }
            }
            MessageType::DisconnectRequest => {
                if let Ok(v) = decode_message::<DisconnectRequest>(data) {
                    eprintln!("[SDK.inbound] decoded DisconnectRequest: {:?}", v);
                }
            }
            MessageType::DisconnectResponse => {
                if let Ok(v) = decode_message::<DisconnectResponse>(data) {
                    eprintln!("[SDK.inbound] decoded DisconnectResponse: {:?}", v);
                }
            }
            MessageType::SendMessageRequest => {
                if let Ok(v) = decode_message::<SendMessageRequest>(data) {
                    eprintln!("[SDK.inbound] decoded SendMessageRequest: {:?}", v);
                }
            }
            MessageType::SendMessageResponse => {
                if let Ok(v) = decode_message::<SendMessageResponse>(data) {
                    eprintln!("[SDK.inbound] decoded SendMessageResponse: {:?}", v);
                }
            }
            MessageType::PushMessageRequest => {
                if let Ok(v) = decode_message::<PushMessageRequest>(data) {
                    eprintln!("[SDK.inbound] decoded PushMessageRequest: {:?}", v);
                }
            }
            MessageType::PushMessageResponse => {
                if let Ok(v) = decode_message::<PushMessageResponse>(data) {
                    eprintln!("[SDK.inbound] decoded PushMessageResponse: {:?}", v);
                }
            }
            MessageType::PushBatchRequest => {
                if let Ok(v) = decode_message::<PushBatchRequest>(data) {
                    eprintln!("[SDK.inbound] decoded PushBatchRequest: {:?}", v);
                }
            }
            MessageType::PushBatchResponse => {
                if let Ok(v) = decode_message::<PushBatchResponse>(data) {
                    eprintln!("[SDK.inbound] decoded PushBatchResponse: {:?}", v);
                }
            }
            MessageType::PingRequest => {
                if let Ok(v) = decode_message::<PingRequest>(data) {
                    eprintln!("[SDK.inbound] decoded PingRequest: {:?}", v);
                }
            }
            MessageType::PongResponse => {
                if let Ok(v) = decode_message::<PongResponse>(data) {
                    eprintln!("[SDK.inbound] decoded PongResponse: {:?}", v);
                }
            }
            MessageType::SubscribeRequest => {
                if let Ok(v) = decode_message::<SubscribeRequest>(data) {
                    eprintln!("[SDK.inbound] decoded SubscribeRequest: {:?}", v);
                }
            }
            MessageType::SubscribeResponse => {
                if let Ok(v) = decode_message::<SubscribeResponse>(data) {
                    eprintln!("[SDK.inbound] decoded SubscribeResponse: {:?}", v);
                }
            }
            MessageType::PublishRequest => {
                if let Ok(v) = decode_message::<PublishRequest>(data) {
                    eprintln!("[SDK.inbound] decoded PublishRequest: {:?}", v);
                }
            }
            MessageType::PublishResponse => {
                if let Ok(v) = decode_message::<PublishResponse>(data) {
                    eprintln!("[SDK.inbound] decoded PublishResponse: {:?}", v);
                }
            }
            MessageType::RpcRequest => {
                if let Ok(v) = decode_message::<RpcRequest>(data) {
                    eprintln!("[SDK.inbound] decoded RpcRequest: {:?}", v);
                }
            }
            MessageType::RpcResponse => {
                if let Ok(v) = decode_message::<RpcResponse>(data) {
                    eprintln!("[SDK.inbound] decoded RpcResponse: {:?}", v);
                }
            }
        }
    }

    async fn apply_sync_entities(
        &mut self,
        entity_type: &str,
        scope: Option<&str>,
        items: &[privchat_protocol::rpc::sync::SyncEntityItem],
        bump_unread_on_incoming: bool,
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
                    let friend_sync = serde_json::from_value::<FriendSyncPayload>(payload.clone())
                        .unwrap_or_default();
                    let user_id = friend_sync
                        .user_id
                        .or(friend_sync.uid)
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if user_id == 0 {
                        continue;
                    }
                    let friend_meta = friend_sync.friend.unwrap_or_default();
                    let embedded_user = friend_sync.user.unwrap_or_default();
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
                            tags: friend_sync.tags.clone(),
                            is_pinned: friend_sync
                                .is_pinned
                                .or(friend_sync.pinned)
                                .unwrap_or(false),
                            created_at: friend_sync
                                .created_at
                                .or(friend_meta.created_at)
                                .unwrap_or(now_ms),
                            version: item.version as i64,
                            updated_at: friend_sync
                                .updated_at
                                .or(friend_meta.updated_at)
                                .or(friend_meta.version)
                                .or(friend_sync.version)
                                .unwrap_or(item.version as i64),
                        })
                        .await?;
                    // Current server returns user profile inside friend payload. We must persist it
                    // even when `entity_type=user` is unsupported, otherwise DM title falls back to raw ID.
                    let has_embedded_user = embedded_user.username.is_some()
                        || embedded_user.nickname.is_some()
                        || embedded_user.name.is_some()
                        || embedded_user.alias.is_some()
                        || embedded_user.avatar.is_some();
                    if has_embedded_user {
                        self.storage
                            .upsert_user(UpsertUserInput {
                                user_id,
                                username: embedded_user.username.clone(),
                                nickname: embedded_user
                                    .nickname
                                    .clone()
                                    .or(embedded_user.name.clone()),
                                alias: embedded_user.alias.clone(),
                                avatar: embedded_user.avatar.clone().unwrap_or_default(),
                                user_type: embedded_user
                                    .user_type
                                    .or(embedded_user.type_field)
                                    .unwrap_or(0),
                                is_deleted: false,
                                channel_id: String::new(),
                                version: item.version as i64,
                                updated_at: embedded_user
                                    .updated_at
                                    .or(embedded_user.version)
                                    .unwrap_or(item.version as i64),
                            })
                            .await?;
                    }
                    if actor_logs_enabled() {
                        eprintln!(
                            "[SDK.actor] friend sync hydrated user: user_id={} username={:?} nickname={:?}",
                            user_id,
                            embedded_user.username,
                            embedded_user.nickname.or(embedded_user.name)
                        );
                    }
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
                            version: item.version as i64,
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
                    let group_sync = serde_json::from_value::<GroupSyncPayload>(payload.clone())
                        .unwrap_or_default();
                    let group_id = group_sync
                        .group_id
                        .or_else(|| Self::json_get_u64(&payload, &["group_id"]))
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if group_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_group(UpsertGroupInput {
                            group_id,
                            name: group_sync.name.clone().or_else(|| {
                                Self::json_get_string(&payload, &["name", "group_name"])
                            }),
                            avatar: group_sync
                                .avatar
                                .clone()
                                .or(group_sync.avatar_url.clone())
                                .or_else(|| {
                                    Self::json_get_string(&payload, &["avatar", "avatar_url"])
                                })
                                .unwrap_or_default(),
                            owner_id: group_sync
                                .owner_id
                                .or_else(|| Self::json_get_u64(&payload, &["owner_id", "owner"])),
                            is_dismissed: item.deleted
                                || Self::json_get_bool(&payload, &["is_dismissed"])
                                    .unwrap_or(false),
                            created_at: group_sync
                                .created_at
                                .or_else(|| Self::json_get_i64(&payload, &["created_at"]))
                                .unwrap_or(now_ms),
                            version: item.version as i64,
                            updated_at: group_sync
                                .updated_at
                                .or_else(|| {
                                    Self::json_get_i64(&payload, &["updated_at", "version"])
                                })
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
                    let group_member = serde_json::from_value::<GroupMemberSyncPayload>(payload)
                        .unwrap_or_default();
                    let group_id = group_member
                        .group_id
                        .or_else(|| Self::resolve_group_id_from_scope(scope))
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.0))
                        .unwrap_or(0);
                    let user_id = group_member
                        .user_id
                        .or(group_member.uid)
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
                            role: group_member.role.unwrap_or(2),
                            status: group_member.status.unwrap_or(0),
                            alias: group_member.alias,
                            is_muted: group_member.is_muted.unwrap_or(false),
                            joined_at: group_member.joined_at.unwrap_or(now_ms),
                            version: item.version as i64,
                            updated_at: group_member
                                .updated_at
                                .or(group_member.version)
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
                    let channel_sync =
                        serde_json::from_value::<ChannelSyncPayload>(payload.clone())
                            .unwrap_or_default();
                    let channel_id = channel_sync
                        .channel_id
                        .or_else(|| Self::json_get_u64(&payload, &["channel_id"]))
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if channel_id == 0 {
                        continue;
                    }
                    let typed_channel_type = channel_sync
                        .channel_type
                        .or(channel_sync.type_field)
                        .and_then(|v| i32::try_from(v).ok());
                    let Some(channel_type) = typed_channel_type.or_else(|| {
                        Self::parse_protocol_channel_type(&payload, &["channel_type", "type"])
                    }) else {
                        eprintln!(
                            "[SDK.actor] skip channel entity with invalid channel_type, entity_id={}, payload={}",
                            item.entity_id, payload
                        );
                        continue;
                    };
                    let existing = self.storage.get_channel_by_id(channel_id).await?;
                    let (last_msg_timestamp, last_msg_content, last_local_message_id) =
                        Self::resolve_channel_last_message_fields(
                            existing.as_ref(),
                            channel_sync.last_msg_timestamp,
                            channel_sync.last_msg_content.clone(),
                        );
                    // Unread is kept as a local projection from message timeline + read cursor
                    // once the channel already exists locally. Server channel sync only provides
                    // the cold-start baseline for channels not yet materialized on device.
                    let materialized_unread = if existing.is_some() {
                        self.storage
                            .count_materialized_unread(channel_id, channel_type)
                            .await
                            .ok()
                    } else {
                        None
                    };
                    let unread_count = match (
                        existing.as_ref().map(|c| c.unread_count),
                        channel_sync.unread_count,
                        materialized_unread,
                    ) {
                        (Some(_existing_unread), Some(0), Some(0)) => 0,
                        (existing_unread, synced_unread, _) => {
                            Self::resolve_channel_unread_count(existing_unread, synced_unread)
                        }
                    };
                    let top = channel_sync
                        .top
                        .unwrap_or_else(|| existing.as_ref().map(|c| c.top).unwrap_or(0));
                    let mute = channel_sync
                        .mute
                        .unwrap_or_else(|| existing.as_ref().map(|c| c.mute).unwrap_or(0));
                    self.storage
                        .upsert_channel(UpsertChannelInput {
                            channel_id,
                            channel_type,
                            channel_name: channel_sync
                                .channel_name
                                .clone()
                                .or(channel_sync.name.clone())
                                .or_else(|| {
                                    Self::json_get_string(&payload, &["channel_name", "name"])
                                })
                                .unwrap_or_default(),
                            channel_remark: Self::json_get_string(&payload, &["channel_remark"])
                                .unwrap_or_default(),
                            avatar: channel_sync
                                .avatar
                                .clone()
                                .or_else(|| Self::json_get_string(&payload, &["avatar"]))
                                .unwrap_or_default(),
                            unread_count,
                            top,
                            mute,
                            last_msg_timestamp,
                            last_local_message_id,
                            last_msg_content,
                            version: item.version as i64,
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
                    let channel_member =
                        serde_json::from_value::<ChannelMemberSyncPayload>(payload)
                            .unwrap_or_default();
                    let channel_id = channel_member
                        .channel_id
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.0))
                        .unwrap_or(0);
                    let member_uid = channel_member
                        .member_uid
                        .or(channel_member.user_id)
                        .or(channel_member.uid)
                        .or_else(|| Self::parse_two_ids(&item.entity_id).map(|v| v.1))
                        .unwrap_or(0);
                    if channel_id == 0 || member_uid == 0 {
                        continue;
                    }
                    let Some(channel_type) = channel_member
                        .channel_type
                        .or(channel_member.type_field)
                        .map(|v| if v == 0 { 1 } else { v })
                    else {
                        eprintln!("[SDK.actor] skip channel_member entity with invalid channel_type, entity_id={}", item.entity_id);
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
                    let member_name = channel_member
                        .member_name
                        .clone()
                        .or(channel_member.name.clone())
                        .unwrap_or_default();
                    let member_remark = channel_member
                        .member_remark
                        .clone()
                        .or(channel_member.remark.clone())
                        .unwrap_or_default();
                    let member_avatar = channel_member
                        .member_avatar
                        .clone()
                        .or(channel_member.avatar.clone())
                        .unwrap_or_default();
                    self.storage
                        .upsert_channel_member(UpsertChannelMemberInput {
                            channel_id,
                            channel_type,
                            member_uid,
                            member_name,
                            member_remark,
                            member_avatar,
                            member_invite_uid: channel_member
                                .member_invite_uid
                                .or(channel_member.inviter_uid)
                                .unwrap_or(0),
                            role: channel_member.role.unwrap_or(0),
                            status: channel_member.status.unwrap_or(0),
                            is_deleted: channel_member.is_deleted.unwrap_or(false),
                            robot: channel_member.robot.unwrap_or(0),
                            version: channel_member.version.unwrap_or(item.version as i64),
                            created_at: channel_member.created_at.unwrap_or(now_ms),
                            updated_at: channel_member.updated_at.unwrap_or(now_ms),
                            extra: channel_member.extra.unwrap_or_default(),
                            forbidden_expiration_time: channel_member
                                .forbidden_expiration_time
                                .unwrap_or(0),
                            member_avatar_cache_key: channel_member
                                .member_avatar_cache_key
                                .unwrap_or_default(),
                        })
                        .await?;
                    // Hydrate user profile from channel_member payload when available,
                    // so DM/group title resolution can avoid falling back to raw numeric IDs.
                    let inferred_username = channel_member
                        .member_name
                        .as_ref()
                        .or(channel_member.name.as_ref())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let inferred_alias = channel_member
                        .member_remark
                        .as_ref()
                        .or(channel_member.remark.as_ref())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let inferred_avatar = channel_member
                        .member_avatar
                        .clone()
                        .or(channel_member.avatar.clone())
                        .unwrap_or_default();
                    if inferred_username.is_some()
                        || inferred_alias.is_some()
                        || !inferred_avatar.is_empty()
                    {
                        let _ = self
                            .storage
                            .upsert_user(UpsertUserInput {
                                user_id: member_uid,
                                username: inferred_username.clone(),
                                nickname: inferred_username,
                                alias: inferred_alias,
                                avatar: inferred_avatar,
                                user_type: 0,
                                is_deleted: false,
                                channel_id: String::new(),
                                version: item.version as i64,
                                updated_at: channel_member
                                    .updated_at
                                    .or(channel_member.version)
                                    .unwrap_or(item.version as i64),
                            })
                            .await;
                    }
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
                    let channel_extra = serde_json::from_value::<ChannelExtraSyncPayload>(payload)
                        .unwrap_or_default();
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_id = channel_extra
                        .channel_id
                        .or(scoped_channel.map(|v| v.1))
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    let channel_type = channel_extra
                        .channel_type
                        .or(channel_extra.type_field)
                        .and_then(|v| i32::try_from(v).ok())
                        .or(scoped_channel.map(|v| v.0))
                        .map(|v| if v == 0 { 1 } else { v })
                        .unwrap_or(1);
                    if channel_id == 0 {
                        continue;
                    }
                    self.storage
                        .upsert_channel_extra(UpsertChannelExtraInput {
                            channel_id,
                            channel_type,
                            browse_to: channel_extra.browse_to.unwrap_or(0),
                            keep_pts: channel_extra.keep_pts.unwrap_or(0),
                            keep_offset_y: channel_extra.keep_offset_y.unwrap_or(0),
                            draft: channel_extra.draft.unwrap_or_default(),
                            draft_updated_at: channel_extra.draft_updated_at.unwrap_or(0),
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
                    let message_sync =
                        serde_json::from_value::<MessageSyncPayload>(payload).unwrap_or_default();
                    let server_message_id = message_sync
                        .server_message_id
                        .or(message_sync.message_id)
                        .or(message_sync.id)
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if server_message_id == 0 {
                        continue;
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type_scope = scoped_channel.map(|v| v.0);
                    let channel_id_scope = scoped_channel.map(|v| v.1);
                    let channel_id = message_sync.channel_id.or(channel_id_scope).unwrap_or(0);
                    let channel_type = message_sync
                        .channel_type
                        .or(message_sync.type_field)
                        .or(message_sync.conversation_type)
                        .or(channel_type_scope)
                        .map(|v| if v == 0 { 1 } else { v })
                        .unwrap_or(1);
                    let local_message_id = message_sync.local_message_id.unwrap_or(0);
                    let timestamp = message_sync
                        .timestamp
                        .or(message_sync.created_at)
                        .or(message_sync.send_time)
                        .unwrap_or(now_ms);
                    let from_uid = message_sync
                        .from_uid
                        .or(message_sync.sender_id)
                        .or(message_sync.from)
                        .or(message_sync.uid)
                        .unwrap_or(0);
                    let message_type = message_sync.message_type.unwrap_or_else(|| {
                        i32::try_from(ContentMessageType::Text.as_u32()).unwrap_or(0)
                    });
                    let content = message_sync
                        .content
                        .clone()
                        .or(message_sync.text.clone())
                        .or(message_sync.body.clone())
                        .unwrap_or_default();
                    let status = message_sync.status.unwrap_or(2);
                    let pts = message_sync.pts.unwrap_or(item.version as i64);
                    let setting = message_sync.setting.unwrap_or(0);
                    let order_seq = message_sync.order_seq.unwrap_or(item.version as i64);
                    let searchable_word = message_sync.searchable_word.unwrap_or_default();
                    let extra = message_sync.extra.unwrap_or_default();
                    if item.deleted {
                        let revoker = content.parse::<serde_json::Value>().ok().and_then(|value| {
                            Self::json_get_u64(&value, &["revoked_by", "revoker"])
                        });
                        match self
                            .storage
                            .set_message_revoke_by_server_message_id(
                                server_message_id,
                                true,
                                revoker,
                            )
                            .await?
                        {
                            Some(revoked_message) => {
                                eprintln!(
                                    "[SDK.revoke] applied server_message_id={} local_message_id={} channel_id={} channel_type={} revoker={:?}",
                                    server_message_id,
                                    revoked_message.message_id,
                                    revoked_message.channel_id,
                                    revoked_message.channel_type,
                                    revoker
                                );
                                emitted.push(SdkEvent::TimelineUpdated {
                                    channel_id: revoked_message.channel_id,
                                    channel_type: revoked_message.channel_type,
                                    message_id: revoked_message.message_id,
                                    reason: "sync_entity_deleted".to_string(),
                                });
                            }
                            None => {
                                eprintln!(
                                    "[SDK.revoke] existing message not found server_message_id={} channel_id={} channel_type={}",
                                    server_message_id, channel_id, channel_type
                                );
                            }
                        }
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "message".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    if channel_id == 0 {
                        eprintln!(
                            "[SDK.message] skip sync item without channel server_message_id={} deleted=false",
                            server_message_id
                        );
                        continue;
                    }
                    let upserted = self
                        .storage
                        .upsert_remote_message_with_result(UpsertRemoteMessageInput {
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
                    let message_id = upserted.message_id;
                    // During bootstrap/periodic sync, do NOT bump unread —
                    // the authoritative count is a local projection from message timeline + read cursor.
                    // For realtime push messages, bump_unread_on_incoming is true.
                    let from_self = current_user_id.map(|v| v == from_uid).unwrap_or(false);
                    let should_bump_unread =
                        bump_unread_on_incoming && !from_self && upserted.inserted_new;
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.unread] message apply: channel_id={} channel_type={} message_id={} server_message_id={} from_uid={} from_self={} inserted_new={} bump_unread_on_incoming={} should_bump_unread={}",
                            channel_id,
                            channel_type,
                            message_id,
                            server_message_id,
                            from_uid,
                            from_self,
                            upserted.inserted_new,
                            bump_unread_on_incoming,
                            should_bump_unread
                        );
                    }
                    let _ = self
                        .update_channel_last_message(
                            channel_id,
                            channel_type,
                            &content,
                            timestamp,
                            message_id,
                            Some(from_uid),
                            should_bump_unread,
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
                    let raw_message_id =
                        Self::json_get_u64(&payload, &["message_id", "server_message_id", "id"])
                            .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                            .unwrap_or(0);
                    if raw_message_id == 0 {
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
                    let message_id = if channel_id > 0 {
                        self.storage
                            .get_message_id_by_server_message_id(
                                channel_id,
                                channel_type,
                                raw_message_id,
                            )
                            .await?
                            .unwrap_or(raw_message_id)
                    } else {
                        raw_message_id
                    };
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
                    let raw_message_id =
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
                    let message_id = if channel_id > 0 && raw_message_id > 0 {
                        self.storage
                            .get_message_id_by_server_message_id(
                                channel_id,
                                channel_type,
                                raw_message_id,
                            )
                            .await?
                            .unwrap_or(raw_message_id)
                    } else {
                        raw_message_id
                    };
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
                    let status_sync = serde_json::from_value::<MessageStatusSyncPayload>(payload)
                        .unwrap_or_default();
                    let raw_message_id = status_sync
                        .message_id
                        .or(status_sync.server_message_id)
                        .or(status_sync.id)
                        .or_else(|| Self::parse_entity_id_u64(&item.entity_id))
                        .unwrap_or(0);
                    if raw_message_id == 0 {
                        continue;
                    }
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_type = status_sync
                        .channel_type
                        .or(status_sync.type_field)
                        .or(status_sync.conversation_type)
                        .or(scoped_channel.map(|v| v.0))
                        .map(|v| if v == 0 { 1 } else { v })
                        .unwrap_or(1);
                    let channel_id = status_sync
                        .channel_id
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    let mut message_id = raw_message_id;
                    if channel_id > 0 {
                        if let Ok(Some(local_id)) = self
                            .storage
                            .get_message_id_by_server_message_id(
                                channel_id,
                                channel_type,
                                raw_message_id,
                            )
                            .await
                        {
                            message_id = local_id;
                        }
                    }
                    if let Some(status) = status_sync.status {
                        let _ = self.storage.update_message_status(message_id, status).await;
                        emitted.push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status,
                            server_message_id: Some(raw_message_id),
                        });
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "message_status".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
            }
            "channel_read_cursor" => {
                let current_uid = self
                    .current_uid
                    .as_ref()
                    .and_then(|uid| uid.parse::<u64>().ok())
                    .unwrap_or(0);
                for item in items {
                    let payload = item
                        .payload
                        .clone()
                        .unwrap_or_else(|| serde_json::json!({}));
                    let read_cursor =
                        serde_json::from_value::<ChannelReadCursorSyncPayload>(payload)
                            .unwrap_or_default();
                    let scoped_channel = Self::parse_channel_scope(scope);
                    let channel_id = read_cursor
                        .channel_id
                        .or(scoped_channel.map(|v| v.1))
                        .unwrap_or(0);
                    if channel_id == 0 {
                        continue;
                    }
                    let channel_type = read_cursor
                        .channel_type
                        .or(read_cursor.type_field)
                        .or(scoped_channel.map(|v| v.0))
                        .map(|v| if v == 0 { 1 } else { v })
                        .unwrap_or(1);
                    let reader_id = read_cursor.reader_id.unwrap_or(current_uid);
                    let read_pts = read_cursor.last_read_pts.unwrap_or(0);
                    if reader_id == current_uid && read_pts > 0 {
                        let unread_before = self
                            .storage
                            .get_channel_unread_count(channel_id, channel_type)
                            .await
                            .ok();
                        let _ = self
                            .storage
                            .project_channel_read_cursor(channel_id, channel_type, read_pts)
                            .await;
                        let unread_after = self
                            .storage
                            .get_channel_unread_count(channel_id, channel_type)
                            .await
                            .ok();
                        if inbound_logs_enabled() {
                            eprintln!(
                                "[SDK.unread] read_cursor apply: channel_id={} channel_type={} reader_id={} current_uid={} read_pts={} unread_before={:?} unread_after={:?}",
                                channel_id,
                                channel_type,
                                reader_id,
                                current_uid,
                                read_pts,
                                unread_before,
                                unread_after
                            );
                        }
                    } else if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.unread] read_cursor skip: channel_id={} channel_type={} reader_id={} current_uid={} read_pts={}",
                            channel_id,
                            channel_type,
                            reader_id,
                            current_uid,
                            read_pts
                        );
                    }
                    emitted.push(SdkEvent::SyncEntityChanged {
                        entity_type: "channel_read_cursor".to_string(),
                        entity_id: item.entity_id.clone(),
                        deleted: item.deleted,
                    });
                }
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
            let send_req = self.build_send_message_request_with_content(
                &msg,
                local_message_id,
                msg.content.clone(),
            )?;
            match self.direct_send_message(send_req).await {
                Ok(resp) => {
                    if let Err(err) = self
                        .storage
                        .mark_message_sent(message_id, resp.server_message_id)
                        .await
                    {
                        // Server has accepted this message; ack queue item to avoid duplicate sends.
                        let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: "commit_failed".to_string(),
                            message_id: Some(message_id),
                            queue_index: None,
                        });
                        return Err(err);
                    }
                    let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                    let last_ts = if msg.created_at > 0 {
                        msg.created_at
                    } else {
                        chrono::Utc::now().timestamp_millis()
                    };
                    let _ = self
                        .update_channel_last_message(
                            msg.channel_id,
                            msg.channel_type,
                            &msg.content,
                            last_ts,
                            message_id,
                            Some(msg.from_uid),
                            false,
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
                            server_message_id: Some(resp.server_message_id),
                        });
                    processed += 1;
                }
                Err(e) => {
                    // Reconciliation for "server committed but response lost":
                    // sync/push may land slightly later than request timeout; poll briefly.
                    if let Some(server_message_id) = self
                        .await_server_message_id(message_id, 12, Duration::from_millis(80))
                        .await
                    {
                        let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: "dequeue_reconciled".to_string(),
                            message_id: Some(message_id),
                            queue_index: None,
                        });
                        self.pending_events
                            .push(SdkEvent::MessageSendStatusChanged {
                                message_id,
                                status: 2,
                                server_message_id: Some(server_message_id),
                            });
                        processed += 1;
                        continue;
                    }
                    eprintln!(
                        "[SDK.actor] normal queue send failed: message_id={} error={}",
                        message_id, e
                    );
                    let _ = self.storage.update_message_status(message_id, 3).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "failed".to_string(),
                        message_id: Some(message_id),
                        queue_index: None,
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 3,
                            server_message_id: None,
                        });
                    let _ = self.storage.normal_queue_ack(vec![message_id]).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "failed_drop".to_string(),
                        message_id: Some(message_id),
                        queue_index: None,
                    });
                    processed += 1;
                    continue;
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
                Ok(resp) => {
                    if let Err(err) = self
                        .storage
                        .mark_message_sent(message_id, resp.server_message_id)
                        .await
                    {
                        // Server has accepted this message; ack queue item to avoid duplicate sends.
                        let _ = self
                            .storage
                            .file_queue_ack(queue_index, vec![message_id])
                            .await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "file".to_string(),
                            action: "commit_failed".to_string(),
                            message_id: Some(message_id),
                            queue_index: Some(queue_index),
                        });
                        return Err(err);
                    }
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
                            server_message_id: Some(resp.server_message_id),
                        });
                    processed += 1;
                }
                Err(e) => {
                    // Reconciliation for "server committed but response lost":
                    // sync/push may land slightly later than request timeout; poll briefly.
                    if let Some(server_message_id) = self
                        .await_server_message_id(message_id, 12, Duration::from_millis(80))
                        .await
                    {
                        let _ = self
                            .storage
                            .file_queue_ack(queue_index, vec![message_id])
                            .await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "file".to_string(),
                            action: "dequeue_reconciled".to_string(),
                            message_id: Some(message_id),
                            queue_index: Some(queue_index),
                        });
                        self.pending_events
                            .push(SdkEvent::MessageSendStatusChanged {
                                message_id,
                                status: 2,
                                server_message_id: Some(server_message_id),
                            });
                        processed += 1;
                        continue;
                    }
                    eprintln!(
                        "[SDK.actor] file queue send failed: queue_index={} message_id={} error={}",
                        queue_index, message_id, e
                    );
                    let _ = self.storage.update_message_status(message_id, 3).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "failed".to_string(),
                        message_id: Some(message_id),
                        queue_index: Some(queue_index),
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 3,
                            server_message_id: None,
                        });
                    let _ = self
                        .storage
                        .file_queue_ack(queue_index, vec![message_id])
                        .await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "failed_drop".to_string(),
                        message_id: Some(message_id),
                        queue_index: Some(queue_index),
                    });
                    processed += 1;
                    continue;
                }
            }
        }
        Ok(processed)
    }

    async fn await_server_message_id(
        &self,
        message_id: u64,
        attempts: usize,
        delay: Duration,
    ) -> Option<u64> {
        for idx in 0..attempts.max(1) {
            if let Ok(Some(latest)) = self.storage.get_message_by_id(message_id).await {
                if let Some(server_message_id) = latest.server_message_id {
                    return Some(server_message_id);
                }
            }
            if idx + 1 < attempts {
                sleep(delay).await;
            }
        }
        None
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
        if actor_logs_enabled() {
            eprintln!(
                "[SDK.actor] connect: enter (has_transport={}, endpoints={})",
                self.transport.is_some(),
                self.config.endpoints.len()
            );
        }
        if let Some(transport) = self.transport.as_ref() {
            if transport.is_connected().await {
                if actor_logs_enabled() {
                    eprintln!("[SDK.actor] connect: already connected");
                }
                return Ok(());
            }
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] connect: found stale transport, rebuilding");
            }
            self.transport = None;
        }
        let endpoints = self.config.endpoints.clone();
        let mut last_err = None;
        for ep in endpoints {
            if actor_logs_enabled() {
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
            }
            match timeout(self.timeout(), self.connect_one(&ep)).await {
                Ok(Ok(c)) => {
                    self.transport = Some(c);
                    if actor_logs_enabled() {
                        eprintln!("[SDK.actor] connect: success");
                    }
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
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] connect_one: begin");
        }
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
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] connect_one: connected");
        }
        Ok(client)
    }

    async fn login(
        &mut self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] login: enter");
        }
        let timeout = self.timeout();
        let os = std::env::consts::OS.to_string();
        let device_name = default_device_name(&os);
        let device_model = default_device_model();
        let manufacturer = default_manufacturer();
        let app_id = default_app_id(&os);
        let req = RpcRequest {
            route: "account/auth/login".to_string(),
            body: serde_json::to_value(AuthLoginRequest {
                username,
                password,
                device_id: device_id.clone(),
                device_info: Some(DeviceInfo {
                    device_id: device_id.clone(),
                    device_type: DeviceType::from_str(&os),
                    app_id: app_id.clone(),
                    push_token: None,
                    push_channel: None,
                    device_name,
                    device_model,
                    os_version: Some(os),
                    app_version: Some("0.1.0".to_string()),
                    manufacturer,
                    device_fingerprint: None,
                }),
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
        if let Err(e) = self.replay_prelogin_inbound_frames().await {
            eprintln!("[SDK.inbound] replay after login failed: {e}");
        }
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] login: success user_id={}", out.user_id);
        }
        Ok(out)
    }

    async fn register(
        &mut self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<LoginResult> {
        let timeout = self.timeout();
        let os = std::env::consts::OS.to_string();
        let device_name = default_device_name(&os);
        let device_model = default_device_model();
        let manufacturer = default_manufacturer();
        let app_id = default_app_id(&os);
        let req = RpcRequest {
            route: routes::account_user::REGISTER.to_string(),
            body: serde_json::to_value(UserRegisterRequest {
                username,
                password,
                nickname: None,
                phone: None,
                email: None,
                device_id: device_id.clone(),
                device_info: Some(DeviceInfo {
                    device_id: device_id.clone(),
                    device_type: DeviceType::from_str(&os),
                    app_id,
                    push_token: None,
                    push_channel: None,
                    device_name,
                    device_model,
                    os_version: Some(os),
                    app_version: Some("0.1.0".to_string()),
                    manufacturer,
                    device_fingerprint: None,
                }),
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
        if let Err(e) = self.replay_prelogin_inbound_frames().await {
            eprintln!("[SDK.inbound] replay after register failed: {e}");
        }
        Ok(out)
    }

    async fn authenticate(&mut self, user_id: u64, token: String, device_id: String) -> Result<()> {
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] authenticate: enter user_id={user_id}");
        }
        let timeout = self.timeout();
        let token_for_persist = token.clone();
        let device_id_for_persist = device_id.clone();
        let os = std::env::consts::OS.to_string();
        let app_id = default_app_id(&os);
        let app_package = default_app_package(&os);
        let device_name = default_device_name(&os);
        let device_model = default_device_model();
        let manufacturer = default_manufacturer();
        let req = AuthorizationRequest {
            auth_type: AuthType::JWT,
            auth_token: token,
            client_info: ClientInfo {
                client_type: default_client_type(&os),
                version: "0.1.0".to_string(),
                os: os.clone(),
                os_version: os.clone(),
                device_model: device_model.clone(),
                app_package,
            },
            device_info: DeviceInfo {
                device_id,
                device_type: DeviceType::from_str(std::env::consts::OS),
                app_id,
                push_token: None,
                push_channel: None,
                device_name,
                device_model,
                os_version: Some(os),
                app_version: Some("0.1.0".to_string()),
                manufacturer,
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
                    expires_at: 0,
                },
            )
            .await?;
        self.storage.flush_user(uid.clone()).await?;
        self.current_uid = Some(uid);
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] authenticate: success");
        }
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
                let data_preview = rpc_resp
                    .data
                    .as_ref()
                    .map(|v| {
                        let s = v.to_string();
                        if s.len() > 512 {
                            format!("{}...", &s[..512])
                        } else {
                            s
                        }
                    })
                    .unwrap_or_else(|| "<none>".to_string());
                if Self::should_log_unsupported_entity_skip(&entity_type_for_apply)
                    || !matches!(rpc_resp.code, 10100)
                {
                    eprintln!(
                        "[SDK.actor] sync_entities rpc rejected: entity_type={} scope={:?} since_version={} code={} message={} data={}",
                        entity_type_for_apply,
                        scope_for_apply,
                        since_version,
                        rpc_resp.code,
                        rpc_resp.message,
                        data_preview
                    );
                }
                return Err(Self::sync_rpc_rejection(
                    &format!(
                        "sync_entities entity_type={} scope={:?} since_version={}",
                        entity_type_for_apply, scope_for_apply, since_version
                    ),
                    rpc_resp.code,
                    rpc_resp.message,
                ));
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
                .apply_sync_entities(
                    &batch.entity_type,
                    batch.scope.as_deref(),
                    &batch.items,
                    false,
                )
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
        self.invalidate_cache_for_events(&entity_events);
        self.last_sync_entity_events = entity_events;
        if scope_for_apply.is_none() {
            self.clear_resume_repair_key(Self::resume_repair_entity_key(&entity_type_for_apply))
                .await;
        }
        Ok(applied)
    }

    async fn send_session_ready(&mut self) -> Result<()> {
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
            return Err(Self::sync_rpc_rejection(
                "session_ready",
                rpc_resp.code,
                rpc_resp.message,
            ));
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

    async fn get_difference(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        last_pts: u64,
        limit: Option<u32>,
    ) -> Result<GetDifferenceResponse> {
        let timeout = self.timeout();
        let req = GetDifferenceRequest {
            channel_id,
            channel_type: u8::try_from(channel_type).unwrap_or(1),
            last_pts,
            limit,
        };
        let request = RpcRequest {
            route: routes::sync::GET_DIFFERENCE.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode get_difference body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode get_difference rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc get_difference",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode get_difference rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Self::sync_rpc_rejection(
                &format!(
                    "get_difference channel_id={} channel_type={} last_pts={}",
                    channel_id, channel_type, last_pts
                ),
                rpc_resp.code,
                rpc_resp.message,
            ));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty get_difference data".into()))?;
        serde_json::from_value(body)
            .map_err(|e| Error::Serialization(format!("decode get_difference response: {e}")))
    }

    async fn get_channel_pts(&mut self, channel_id: u64, channel_type: i32) -> Result<u64> {
        let timeout = self.timeout();
        let req = GetChannelPtsRequest {
            channel_id,
            channel_type: u8::try_from(channel_type).unwrap_or(1),
        };
        let request = RpcRequest {
            route: routes::sync::GET_CHANNEL_PTS.to_string(),
            body: serde_json::to_value(req)
                .map_err(|e| Error::Serialization(format!("encode get_channel_pts body: {e}")))?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode get_channel_pts rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc get_channel_pts",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode get_channel_pts rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Self::sync_rpc_rejection(
                &format!(
                    "get_channel_pts channel_id={} channel_type={}",
                    channel_id, channel_type
                ),
                rpc_resp.code,
                rpc_resp.message,
            ));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty get_channel_pts data".into()))?;
        let resp: GetChannelPtsResponse = serde_json::from_value(body)
            .map_err(|e| Error::Serialization(format!("decode get_channel_pts response: {e}")))?;
        Ok(resp.current_pts)
    }

    async fn hydrate_channel_messages_from_history(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        limit: u32,
    ) -> Result<usize> {
        let req = MessageHistoryGetRequest {
            user_id: 0,
            channel_id,
            before_server_message_id: None,
            limit: Some(limit),
        };
        let resp: MessageHistoryResponse = self
            .rpc_call_typed(routes::message_history::GET, &req)
            .await?;
        if resp.messages.is_empty() {
            return Ok(0);
        }

        let normalized_channel_type = if channel_type == 0 { 1 } else { channel_type };
        let mut applied = 0usize;
        for item in resp.messages {
            let timestamp_ms = i64::try_from(item.timestamp).unwrap_or(i64::MAX);
            let message_type = match item.message_type.as_str() {
                "image" => i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(0),
                "audio" => i32::try_from(ContentMessageType::Audio.as_u32()).unwrap_or(0),
                "video" => i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(0),
                "file" => i32::try_from(ContentMessageType::File.as_u32()).unwrap_or(0),
                _ => i32::try_from(ContentMessageType::Text.as_u32()).unwrap_or(0),
            };
            let message_id = self
                .storage
                .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                    server_message_id: item.message_id,
                    local_message_id: 0,
                    channel_id: item.channel_id,
                    channel_type: normalized_channel_type,
                    timestamp: timestamp_ms,
                    from_uid: item.sender_id,
                    message_type,
                    content: item.content.clone(),
                    status: 2,
                    pts: 0,
                    setting: 0,
                    order_seq: 0,
                    searchable_word: String::new(),
                    extra: String::new(),
                })
                .await?
                .message_id;
            let _ = self
                .update_channel_last_message(
                    item.channel_id,
                    normalized_channel_type,
                    &item.content,
                    timestamp_ms,
                    message_id,
                    Some(item.sender_id),
                    false,
                )
                .await;
            applied += 1;
        }
        Ok(applied)
    }

    async fn resume_channel_difference(
        &mut self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<usize> {
        let scope = Some(format!("{channel_type}:{channel_id}"));
        let mut last_pts = self
            .storage
            .max_message_pts(channel_id, channel_type)
            .await?;
        if let Some(cursor_pts) = self.load_resume_channel_pts(channel_id, channel_type).await {
            last_pts = last_pts.max(cursor_pts);
        }
        let mut total_applied = 0usize;
        let mut pages = 0usize;
        loop {
            pages += 1;
            if pages > 64 {
                return Err(Error::InvalidState(format!(
                    "resume_channel_difference exceeded max paging iterations: channel_id={} channel_type={}",
                    channel_id, channel_type
                )));
            }
            let response = match self
                .get_difference(channel_id, channel_type, last_pts, Some(200))
                .await
            {
                Ok(resp) => resp,
                Err(err) => {
                    if Self::classify_resume_error(&err)
                        == ResumeFailureClass::ChannelResyncRequired
                    {
                        let current_pts = self.get_channel_pts(channel_id, channel_type).await?;
                        self.save_resume_channel_pts(channel_id, channel_type, current_pts)
                            .await?;
                        let recovered = self
                            .hydrate_channel_messages_from_history(channel_id, channel_type, 100)
                            .await
                            .unwrap_or(0);
                        return Ok(total_applied + recovered);
                    }
                    return Err(err);
                }
            };
            if response.commits.is_empty() {
                break;
            }
            for commit in response.commits.iter() {
                let (entity_type, item) = Self::sync_item_from_difference_commit(commit);
                let applied = self
                    .enqueue_and_apply_sync_items(entity_type.clone(), scope.clone(), vec![item])
                    .await?;
                total_applied += applied;
                self.queue_last_sync_events(entity_type, scope.clone(), applied);
            }
            let next_last_pts = response
                .commits
                .iter()
                .map(|commit| commit.pts)
                .max()
                .unwrap_or(last_pts);
            if next_last_pts <= last_pts {
                break;
            }
            last_pts = next_last_pts;
            self.save_resume_channel_pts(channel_id, channel_type, last_pts)
                .await?;
            if !response.has_more {
                break;
            }
        }
        self.reconcile_channel_unread_after_difference(channel_id, channel_type)
            .await?;
        Ok(total_applied)
    }

    async fn reconcile_channel_unread_after_difference(
        &mut self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<()> {
        let Some(channel) = self.storage.get_channel_by_id(channel_id).await? else {
            return Ok(());
        };
        let Some(extra) = self
            .storage
            .get_channel_extra(channel_id, channel_type)
            .await?
        else {
            return Ok(());
        };
        let max_pts = self
            .storage
            .max_message_pts(channel_id, channel_type)
            .await?;
        if max_pts == 0 || extra.keep_pts < max_pts {
            return Ok(());
        }
        let exact_unread = self
            .storage
            .count_materialized_unread(channel_id, channel_type)
            .await?;
        if channel.unread_count == exact_unread {
            return Ok(());
        }
        self.storage
            .upsert_channel(UpsertChannelInput {
                channel_id: channel.channel_id,
                channel_type: channel.channel_type,
                channel_name: channel.channel_name,
                channel_remark: channel.channel_remark,
                avatar: channel.avatar,
                unread_count: exact_unread,
                top: channel.top,
                mute: channel.mute,
                last_msg_timestamp: channel.last_msg_timestamp,
                last_local_message_id: channel.last_local_message_id,
                last_msg_content: channel.last_msg_content,
                version: channel.version,
            })
            .await?;
        Ok(())
    }

    async fn run_resume_sync(&mut self) -> Result<()> {
        if self.session_state != SessionState::Authenticated {
            let err =
                Error::InvalidState("run_resume_sync requires authenticated state".to_string());
            let _ = self
                .handle_resume_failure(ResumeFailureTarget::Global, &err)
                .await;
            return Err(err);
        }
        if !self.bootstrap_completed {
            return Ok(());
        }
        let mut stats = ResumeRunStats::default();
        self.queue_resume_started();
        if let Err(err) = self.send_session_ready().await {
            let _ = self
                .handle_resume_failure(ResumeFailureTarget::Global, &err)
                .await;
            return Err(err);
        }

        let entity_order = ["friend", "group", "channel", "user", "channel_read_cursor"];
        for entity_type in entity_order {
            match self.sync_entities(entity_type.to_string(), None).await {
                Ok(applied) => {
                    stats.entity_types_synced += 1;
                    self.queue_last_sync_events(entity_type.to_string(), None, applied);
                }
                Err(e) if Self::is_unsupported_entity_error(&e) => {
                    self.log_unsupported_sync_skip("resume sync", entity_type, None, &e);
                }
                Err(e) => {
                    let handling = self
                        .handle_resume_failure(
                            ResumeFailureTarget::EntityType(entity_type.to_string()),
                            &e,
                        )
                        .await;
                    if handling == ResumeFailureHandling::Abort {
                        return Err(e);
                    }
                }
            }
        }

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
                match self
                    .sync_entities("group_member".to_string(), Some(group.group_id.to_string()))
                    .await
                {
                    Ok(applied) => {
                        stats.entity_types_synced += 1;
                        self.queue_last_sync_events(
                            "group_member".to_string(),
                            Some(group.group_id.to_string()),
                            applied,
                        );
                    }
                    Err(e) if Self::is_unsupported_entity_error(&e) => {
                        self.log_unsupported_sync_skip(
                            "resume sync",
                            "group_member",
                            Some(group.group_id.to_string()),
                            &e,
                        );
                    }
                    Err(e) => {
                        let handling = self
                            .handle_resume_failure(
                                ResumeFailureTarget::EntityType("group_member".to_string()),
                                &e,
                            )
                            .await;
                        if handling == ResumeFailureHandling::Abort {
                            return Err(e);
                        }
                    }
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
                stats.channels_scanned += 1;
                self.pending_events
                    .push(SdkEvent::ResumeSyncChannelStarted {
                        channel_id: channel.channel_id,
                        channel_type: channel.channel_type,
                    });
                match self
                    .resume_channel_difference(channel.channel_id, channel.channel_type)
                    .await
                {
                    Ok(applied) => {
                        if applied > 0 {
                            stats.channels_applied += 1;
                            self.pending_events.push(SdkEvent::SyncChannelApplied {
                                channel_id: channel.channel_id,
                                channel_type: channel.channel_type,
                                applied,
                            });
                        }
                        self.pending_events
                            .push(SdkEvent::ResumeSyncChannelCompleted {
                                channel_id: channel.channel_id,
                                channel_type: channel.channel_type,
                                applied,
                            });
                    }
                    Err(err) => {
                        stats.channel_failures += 1;
                        let handling = self
                            .handle_resume_failure(
                                ResumeFailureTarget::Channel {
                                    channel_id: channel.channel_id,
                                    channel_type: channel.channel_type,
                                },
                                &err,
                            )
                            .await;
                        if handling == ResumeFailureHandling::Abort {
                            return Err(err);
                        }
                    }
                }
            }
            if channels.len() < channel_page_size {
                break;
            }
            channel_offset += channel_page_size;
        }
        self.queue_resume_completed(stats);
        Ok(())
    }

    async fn full_rebuild_required(&self) -> bool {
        self.storage
            .kv_get(Self::resume_repair_full_rebuild_key())
            .await
            .ok()
            .flatten()
            .is_some()
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
                .apply_sync_entities(
                    &batch.entity_type,
                    batch.scope.as_deref(),
                    &batch.items,
                    true,
                )
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
        self.invalidate_cache_for_events(&entity_events);
        self.last_sync_entity_events = entity_events;
        Ok(applied)
    }

    async fn handle_inbound_frame(&mut self, biz_type: u8, data: Vec<u8>) -> Result<usize> {
        if inbound_logs_enabled() {
            eprintln!(
                "[SDK.inbound] frame biz_type={} len={}",
                biz_type,
                data.len()
            );
        }
        let message_type = MessageType::from(biz_type);
        if inbound_logs_enabled() {
            eprintln!("[SDK.inbound] frame message_type={:?}", message_type);
        }
        Self::log_inbound_decoded(message_type, &data);
        // Do not drop server push frames during login->authenticate gap.
        // current_uid is set by login, while session_state may still be LoggedIn.
        // If we gate strictly by Authenticated, login notice pushes are lost and only
        // channel preview is updated by sync, leaving message table empty.
        if self.current_uid.is_none() {
            match message_type {
                MessageType::SendMessageRequest
                | MessageType::PushMessageRequest
                | MessageType::PushBatchRequest
                | MessageType::PublishRequest => {
                    if self.pending_prelogin_inbound_frames.len() >= 256 {
                        let _ = self.pending_prelogin_inbound_frames.remove(0);
                    }
                    self.pending_prelogin_inbound_frames.push((biz_type, data));
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] queued pre-login frame message_type={:?} queued={}",
                            message_type,
                            self.pending_prelogin_inbound_frames.len()
                        );
                    }
                }
                _ => {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] skip frame before login message_type={:?} biz_type={}",
                            message_type, biz_type
                        );
                    }
                }
            }
            return Ok(0);
        }
        let mut message_items = Vec::new();
        let mut read_cursor_items = Vec::new();
        match message_type {
            MessageType::SendMessageRequest => {
                let req: SendMessageRequest = decode_message(&data).map_err(|e| {
                    Error::Serialization(format!("decode send message request: {e}"))
                })?;
                let current_user_id = self
                    .current_uid
                    .as_ref()
                    .and_then(|v| v.parse::<u64>().ok());
                // Accept self-echo SendMessageRequest as a reconciliation signal.
                // In some transport paths the outbound request can be committed on server
                // while response correlation is dropped; processing self-echo lets local
                // status converge to sent instead of remaining failed.
                if inbound_logs_enabled() && current_user_id == Some(req.from_uid) {
                    eprintln!(
                        "[SDK.inbound] process self SendMessageRequest echo: from_uid={} local_message_id={}",
                        req.from_uid, req.local_message_id
                    );
                }
                let channel_type = self
                    .storage
                    .get_channel_by_id(req.channel_id)
                    .await?
                    .map(|ch| ch.channel_type as u8)
                    .unwrap_or(1);
                if let Some(item) = Self::send_message_to_sync_item(req, channel_type) {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] mapped SendMessageRequest -> sync item channel_type={}",
                            channel_type
                        );
                    }
                    message_items.push(item);
                }
            }
            MessageType::PushMessageRequest => {
                let req: PushMessageRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode push message: {e}")))?;
                if let Some(status_item) = Self::push_message_to_status_sync_item(&req) {
                    read_cursor_items.push(status_item);
                } else {
                    // deleted=true 时 push_message_to_sync_item 会透传给 SyncEntityItem.deleted，
                    // 进而触发 "message" 实体处理里的 set_message_revoke 路径
                    message_items.push(Self::push_message_to_sync_item(req));
                }
            }
            MessageType::PushBatchRequest => {
                let req: PushBatchRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode push batch: {e}")))?;
                for push in req.messages {
                    if let Some(status_item) = Self::push_message_to_status_sync_item(&push) {
                        read_cursor_items.push(status_item);
                    } else {
                        message_items.push(Self::push_message_to_sync_item(push));
                    }
                }
            }
            MessageType::PublishRequest => {
                let req: PublishRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode publish request: {e}")))?;
                if let Ok(push) = decode_message::<PushMessageRequest>(&req.payload) {
                    // IM 场景：payload 是 PushMessageRequest（私聊/群聊消息走 sync pipeline）
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] publish payload decoded as PushMessageRequest channel_id={}",
                            push.channel_id
                        );
                    }
                    if let Some(status_item) = Self::push_message_to_status_sync_item(&push) {
                        read_cursor_items.push(status_item);
                    } else {
                        message_items.push(Self::push_message_to_sync_item(push));
                    }
                } else if let Ok(batch) = decode_message::<PushBatchRequest>(&req.payload) {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] publish payload decoded as PushBatchRequest count={}",
                            batch.messages.len()
                        );
                    }
                    for push in batch.messages {
                        if let Some(status_item) = Self::push_message_to_status_sync_item(&push) {
                            read_cursor_items.push(status_item);
                        } else {
                            message_items.push(Self::push_message_to_sync_item(push));
                        }
                    }
                } else {
                    // Room 场景：payload 是原始内容（纯网络，不走本地数据库）
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] room publish channel_id={} topic={:?} payload_len={}",
                            req.channel_id,
                            req.topic,
                            req.payload.len()
                        );
                    }
                    if req.topic.as_deref() == Some("presence_changed") {
                        self.apply_presence_changed_payload(&req.payload);
                    }
                    self.pending_events
                        .push(SdkEvent::SubscriptionMessageReceived {
                            channel_id: req.channel_id,
                            topic: req.topic,
                            payload: req.payload,
                            publisher: req.publisher,
                            server_message_id: req.server_message_id,
                            timestamp: req.timestamp,
                        });
                }
            }
            _ => {
                if inbound_logs_enabled() {
                    eprintln!(
                        "[SDK.inbound] ignore inbound message_type={:?} biz_type={}",
                        message_type, biz_type
                    );
                }
                return Ok(0);
            }
        }

        let mut applied = 0usize;
        if !message_items.is_empty() {
            applied += self
                .enqueue_and_apply_sync_items("message".to_string(), None, message_items)
                .await?;
        }
        if !read_cursor_items.is_empty() {
            applied += self
                .enqueue_and_apply_sync_items(
                    "channel_read_cursor".to_string(),
                    None,
                    read_cursor_items,
                )
                .await?;
        }
        Ok(applied)
    }

    async fn replay_prelogin_inbound_frames(&mut self) -> Result<usize> {
        if self.current_uid.is_none() || self.pending_prelogin_inbound_frames.is_empty() {
            return Ok(0);
        }
        let pending = std::mem::take(&mut self.pending_prelogin_inbound_frames);
        let mut applied = 0usize;
        for (biz_type, data) in pending {
            applied += self.handle_inbound_frame(biz_type, data).await?;
        }
        if inbound_logs_enabled() {
            eprintln!(
                "[SDK.inbound] replayed pre-login frames applied={}",
                applied
            );
        }
        Ok(applied)
    }

    async fn run_bootstrap_sync(&mut self) -> Result<()> {
        if self.session_state != SessionState::Authenticated {
            return Err(Error::InvalidState(
                "run_bootstrap_sync requires authenticated state".to_string(),
            ));
        }

        if self.bootstrap_completed && !self.full_rebuild_required().await {
            match self.run_resume_sync().await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    if !self.full_rebuild_required().await {
                        return Err(err);
                    }
                    eprintln!(
                        "[SDK.actor] bootstrap sync escalate to full rebuild after resume failure: {}",
                        err
                    );
                }
            }
        }

        let core_entities = ["friend", "group", "channel", "user", "channel_read_cursor"];
        let optional_entities = ["user_block", "channel_extra", "channel_unread"];
        if actor_logs_enabled() {
            eprintln!(
                "[SDK.actor] bootstrap sync plan core={:?} optional={:?}",
                core_entities, optional_entities
            );
        }
        let order = [
            "friend",
            "group",
            "channel",
            "user",
            "channel_read_cursor",
            "user_block",
        ];
        for entity_type in order {
            match self.sync_entities(entity_type.to_string(), None).await {
                Ok(count) => {
                    if actor_logs_enabled() {
                        eprintln!(
                            "[SDK.actor] bootstrap sync entity={} count={}",
                            entity_type, count
                        );
                    }
                }
                Err(e) if Self::is_unsupported_entity_error(&e) => {
                    self.log_unsupported_sync_skip("bootstrap sync", entity_type, None, &e);
                }
                Err(e) => return Err(e),
            }
        }

        // Scoped member sync (best effort but still inside bootstrap critical path):
        // - group_member scoped by group_id
        // NOTE:
        //   ENTITY_SYNC_V1 core controlled enum is `group_member`.
        //   `channel_member` is not guaranteed by current server deployment and is not
        //   part of old stable bootstrap path, so we do not treat it as bootstrap core.
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
                        if actor_logs_enabled() {
                            eprintln!(
                                "[SDK.actor] bootstrap sync entity=group_member scope=group:{} count={}",
                                group.group_id, count
                            );
                        }
                    }
                    Err(e) if Self::is_unsupported_entity_error(&e) => {
                        self.log_unsupported_sync_skip(
                            "bootstrap sync",
                            "group_member",
                            Some(format!("group:{}", group.group_id)),
                            &e,
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
        self.bootstrap_completed = true;
        if let Some(uid) = &self.current_uid {
            self.storage
                .set_bootstrap_completed(uid.clone(), true)
                .await?;
        }
        self.clear_resume_repair_key(Self::resume_repair_full_rebuild_key())
            .await;

        // Notify server that bootstrap is finished and this session is ready
        // to receive catch-up and realtime pushes. This endpoint is idempotent.
        self.send_session_ready().await?;

        self.hydrate_system_channel_messages_from_history().await?;

        Ok(())
    }

    async fn hydrate_system_channel_messages_from_history(&mut self) -> Result<()> {
        let channels = self.storage.list_channels(200, 0).await?;
        for channel in channels {
            if channel.channel_type != 1 && channel.channel_type != 0 {
                continue;
            }
            let normalized_channel_type = if channel.channel_type == 0 {
                1
            } else {
                channel.channel_type
            };
            let is_system_channel = channel.channel_name == "1"
                || channel.channel_remark == "1"
                || channel.channel_name == "__system_1__";
            if !is_system_channel {
                continue;
            }
            eprintln!(
                "[SDK.actor] bootstrap hydrate system channel={} channel_type={} last_msg='{}'",
                channel.channel_id, normalized_channel_type, channel.last_msg_content
            );
            let existing = self
                .storage
                .list_messages(channel.channel_id, normalized_channel_type, 1, 0)
                .await?;
            if !existing.is_empty() {
                eprintln!(
                    "[SDK.actor] bootstrap hydrate skip channel={} existing_local_messages={}",
                    channel.channel_id,
                    existing.len()
                );
                continue;
            }

            let req = MessageHistoryGetRequest {
                user_id: 0,
                channel_id: channel.channel_id,
                before_server_message_id: None,
                limit: Some(20),
            };
            let resp: MessageHistoryResponse = self
                .rpc_call_typed(routes::message_history::GET, &req)
                .await?;
            eprintln!(
                "[SDK.actor] bootstrap hydrate history channel={} messages={}",
                channel.channel_id,
                resp.messages.len()
            );
            if resp.messages.is_empty() {
                continue;
            }

            for item in resp.messages {
                let timestamp_ms = i64::try_from(item.timestamp).unwrap_or(i64::MAX);
                let message_type = match item.message_type.as_str() {
                    "image" => i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(0),
                    "audio" => i32::try_from(ContentMessageType::Audio.as_u32()).unwrap_or(0),
                    "video" => i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(0),
                    "file" => i32::try_from(ContentMessageType::File.as_u32()).unwrap_or(0),
                    _ => i32::try_from(ContentMessageType::Text.as_u32()).unwrap_or(0),
                };
                let message_id = self
                    .storage
                    .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                        server_message_id: item.message_id,
                        local_message_id: 0,
                        channel_id: item.channel_id,
                        channel_type: normalized_channel_type,
                        timestamp: timestamp_ms,
                        from_uid: item.sender_id,
                        message_type,
                        content: item.content.clone(),
                        status: 2,
                        pts: 0,
                        setting: 0,
                        order_seq: 0,
                        searchable_word: String::new(),
                        extra: String::new(),
                    })
                    .await?
                    .message_id;
                // Bootstrap history hydration should not bump unread —
                // Bootstrap history hydration should not bump unread.
                // Unread is derived locally from message timeline + read cursor projection.
                let _ = self
                    .update_channel_last_message(
                        item.channel_id,
                        normalized_channel_type,
                        &item.content,
                        timestamp_ms,
                        message_id,
                        Some(item.sender_id),
                        false,
                    )
                    .await;
            }
        }
        Ok(())
    }

    async fn sync_channel(&mut self, channel_id: u64, channel_type: i32) -> Result<usize> {
        // Keep channel-targeted sync aligned with the existing minimum set:
        // channel state + timeline + read cursor + group member hydration.
        // `user` remains a collection-scoped entity family and should not be fetched with a
        // channel scope, otherwise the SDK just emits unsupported/no-op requests.
        // Timeline recovery must keep using `get_difference`; `message` is not a sync_entities
        // family in the approved existing-data sync model.
        let scope = Some(format!("{channel_type}:{channel_id}"));
        let mut total = 0usize;
        total += self
            .sync_entities("channel".to_string(), scope.clone())
            .await?;
        match self
            .sync_entities("channel_read_cursor".to_string(), scope.clone())
            .await
        {
            Ok(v) => total += v,
            Err(e) if Self::is_unsupported_entity_error(&e) => {
                self.log_unsupported_sync_skip("sync_channel", "channel_read_cursor", scope, &e);
            }
            Err(e) => return Err(e),
        }
        total += self
            .resume_channel_difference(channel_id, channel_type)
            .await?;
        if channel_type == 2 {
            match self
                .sync_entities("group_member".to_string(), Some(channel_id.to_string()))
                .await
            {
                Ok(v) => total += v,
                Err(e) if Self::is_unsupported_entity_error(&e) => {
                    self.log_unsupported_sync_skip(
                        "sync_channel",
                        "group_member",
                        Some(channel_id.to_string()),
                        &e,
                    );
                }
                Err(e) => return Err(e),
            }
        }
        self.clear_resume_repair_key(Self::resume_repair_channel_key(channel_id, channel_type))
            .await;
        Ok(total)
    }

    async fn sync_all_channels(&mut self) -> Result<usize> {
        let mut total = 0usize;
        total += self.sync_entities("friend".to_string(), None).await?;
        total += self.sync_entities("group".to_string(), None).await?;
        total += self.sync_entities("channel".to_string(), None).await?;
        total += self.sync_entities("user".to_string(), None).await?;
        match self
            .sync_entities("channel_read_cursor".to_string(), None)
            .await
        {
            Ok(v) => total += v,
            Err(e) if Self::is_unsupported_entity_error(&e) => {
                self.log_unsupported_sync_skip(
                    "sync_all_channels",
                    "channel_read_cursor",
                    None,
                    &e,
                );
            }
            Err(e) => return Err(e),
        }
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
                match self
                    .sync_entities("group_member".to_string(), Some(group.group_id.to_string()))
                    .await
                {
                    Ok(v) => total += v,
                    Err(e) if Self::is_unsupported_entity_error(&e) => {
                        self.log_unsupported_sync_skip(
                            "sync_all_channels",
                            "group_member",
                            Some(group.group_id.to_string()),
                            &e,
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
        Ok(total)
    }

    async fn batch_get_presence(&mut self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let timeout = self.timeout();
        let req = PresenceBatchStatusRequest { user_ids };
        let request = RpcRequest {
            route: routes::presence::STATUS_GET.to_string(),
            body: serde_json::to_value(req).map_err(|e| {
                Error::Serialization(format!("encode batch_get_presence body: {e}"))
            })?,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode batch_get_presence rpc: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                "rpc batch_get_presence",
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode batch_get_presence rpc: {e}")))?;
        if rpc_resp.code != 0 {
            return Err(Error::Auth(rpc_resp.message));
        }
        let body = rpc_resp
            .data
            .ok_or_else(|| Error::Serialization("empty batch_get_presence data".into()))?;
        let response: PresenceBatchStatusResponse = serde_json::from_value(body).map_err(|e| {
            Error::Serialization(format!("decode batch_get_presence response: {e}"))
        })?;
        Ok(self.cache_presence_response(response))
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

    /// 订阅频道事件（typing / presence 等状态事件通过此通道接收）
    /// token: 可选，Room 类型订阅时传入业务 API 签发的 ticket（JWT）
    async fn subscribe_channel(
        &mut self,
        channel_id: u64,
        channel_type: u8,
        token: Option<String>,
    ) -> Result<()> {
        let timeout = self.timeout();
        let req = SubscribeRequest {
            setting: 0,
            local_message_id: 0,
            channel_id,
            channel_type,
            action: 1, // SUBSCRIBE
            param: token.unwrap_or_default(),
        };
        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode subscribe_channel: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::SubscribeRequest as u8,
                timeout,
                "subscribe_channel",
            )
            .await?;
        let resp: SubscribeResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode subscribe_channel resp: {e}")))?;
        if resp.reason_code != 0 {
            return Err(Error::Auth(format!(
                "subscribe_channel failed: reason_code={}",
                resp.reason_code
            )));
        }
        Ok(())
    }

    /// 取消订阅频道事件
    async fn unsubscribe_channel(&mut self, channel_id: u64, channel_type: u8) -> Result<()> {
        let timeout = self.timeout();
        let req = SubscribeRequest {
            setting: 0,
            local_message_id: 0,
            channel_id,
            channel_type,
            action: 2, // UNSUBSCRIBE
            param: String::new(),
        };
        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode unsubscribe_channel: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::SubscribeRequest as u8,
                timeout,
                "unsubscribe_channel",
            )
            .await?;
        let resp: SubscribeResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode unsubscribe_channel resp: {e}")))?;
        if resp.reason_code != 0 {
            return Err(Error::Auth(format!(
                "unsubscribe_channel failed: reason_code={}",
                resp.reason_code
            )));
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
        let mut envelope = MessagePayloadEnvelope {
            content: content.clone(),
            metadata: serde_json::from_str::<serde_json::Value>(&message.extra)
                .ok()
                .and_then(|value| if value.is_null() { None } else { Some(value) }),
            reply_to_message_id: None,
            mentioned_user_ids: None,
            message_source: None,
        };

        if let Ok(content_json) = serde_json::from_str::<serde_json::Value>(&content) {
            // Only treat as canonical envelope when payload explicitly carries envelope keys.
            let looks_like_envelope = content_json
                .as_object()
                .map(|obj| {
                    obj.contains_key("content")
                        || obj.contains_key("metadata")
                        || obj.contains_key("reply_to_message_id")
                        || obj.contains_key("mentioned_user_ids")
                        || obj.contains_key("message_source")
                })
                .unwrap_or(false);

            if looks_like_envelope {
                if let Ok(parsed_envelope) =
                    serde_json::from_value::<MessagePayloadEnvelope>(content_json.clone())
                {
                    envelope = parsed_envelope;
                }
            } else if content_json
                .get("file_id")
                .or_else(|| content_json.get("thumbnail_file_id"))
                .is_some()
            {
                // Backward compatibility: attachment callers may still pass raw metadata json.
                let file_type = content_json
                    .get("file_type")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        content_json
                            .get("mime_type")
                            .and_then(|v| v.as_str())
                            .and_then(|mime| {
                                if mime.starts_with("image/") {
                                    Some("image")
                                } else if mime.starts_with("video/") {
                                    Some("video")
                                } else {
                                    None
                                }
                            })
                    })
                    .unwrap_or("file");
                envelope.content = Self::attachment_placeholder_text(file_type).to_string();
                envelope.metadata = Some(content_json);
            }
        }

        let payload = serde_json::to_vec(&envelope)
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
    ) -> Result<SendMessageResponse> {
        if req.local_message_id == 0 {
            return Err(Error::InvalidState(
                "local_message_id must be non-zero".to_string(),
            ));
        }
        let request_data = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode send request: {e}")))?;
        let timeout = self.timeout();
        let transport = match self.transport.as_mut() {
            Some(t) => t,
            None => {
                let transition = self.apply_transport_health(false);
                self.push_connection_transition_event(transition);
                return Err(self.network_disconnected_error());
            }
        };
        let opt = TransportOptions::new()
            .with_biz_type(MessageType::SendMessageRequest as u8)
            .with_timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(request_data), opt)
            .await
            .map_err(|e| {
                eprintln!("[SDK.actor] direct send message transport error: {e}");
                let transition = self.apply_transport_health(false);
                self.push_connection_transition_event(transition);
                self.network_disconnected_error()
            })?;
        let resp: SendMessageResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode send response: {e}")))?;
        if resp.reason_code != 0 {
            return Err(Error::Transport(format!(
                "send message failed: reason_code={}",
                resp.reason_code
            )));
        }
        if resp.server_message_id == 0 {
            return Err(Error::Serialization(
                "send message response missing server_message_id".to_string(),
            ));
        }
        Ok(resp)
    }

    fn guess_file_type(message_type: i32, filename: &str, mime: &str) -> &'static str {
        let image_type = privchat_protocol::message::ContentMessageType::Image as i32;
        let video_type = privchat_protocol::message::ContentMessageType::Video as i32;
        let audio_type = privchat_protocol::message::ContentMessageType::Audio as i32;
        if message_type == image_type || mime.starts_with("image/") {
            return "image";
        }
        if message_type == video_type || mime.starts_with("video/") {
            return "video";
        }
        if message_type == audio_type || mime.starts_with("audio/") {
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

    fn attachment_placeholder_text(file_type: &str) -> &'static str {
        match file_type {
            "image" => "[图片]",
            "video" => "[视频]",
            _ => "[文件]",
        }
    }

    fn is_formatted_preview(content: &str) -> bool {
        matches!(
            content,
            s if s.starts_with("[图片]")
                || s.starts_with("[视频]")
                || s.starts_with("[语音]")
                || s.starts_with("[文件]")
                || s.starts_with("[位置]")
                || s.starts_with("[红包]")
                || s.starts_with("[名片]")
                || s.starts_with("[表情]")
        )
    }

    fn preview_type_from_json(value: &serde_json::Value) -> Option<String> {
        Self::json_get_string(value, &["type"])
            .or_else(|| Self::json_get_string(value, &["content", "type"]))
            .or_else(|| Self::json_get_string(value, &["metadata", "type"]))
            .map(|v| v.to_ascii_lowercase())
    }

    fn preview_text_from_json(value: &serde_json::Value) -> Option<String> {
        Self::json_get_string(value, &["content"])
            .or_else(|| Self::json_get_string(value, &["text"]))
            .or_else(|| Self::json_get_string(value, &["body"]))
            .or_else(|| Self::json_get_string(value, &["content", "text"]))
            .or_else(|| Self::json_get_string(value, &["content", "body"]))
            .or_else(|| Self::json_get_string(value, &["tip"]))
    }

    fn preview_duration_from_json(value: &serde_json::Value) -> Option<i64> {
        Self::json_get_i64(value, &["duration"])
            .or_else(|| Self::json_get_i64(value, &["content", "duration"]))
            .or_else(|| Self::json_get_i64(value, &["metadata", "duration"]))
    }

    fn infer_preview_kind(
        message_type: Option<i32>,
        content_json: Option<&serde_json::Value>,
        extra_json: Option<&serde_json::Value>,
    ) -> &'static str {
        let image_type = i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(-1);
        let file_type = i32::try_from(ContentMessageType::File.as_u32()).unwrap_or(-1);
        let voice_type = i32::try_from(ContentMessageType::Voice.as_u32()).unwrap_or(-1);
        let video_type = i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(-1);
        let system_type = i32::try_from(ContentMessageType::System.as_u32()).unwrap_or(-1);
        let audio_type = i32::try_from(ContentMessageType::Audio.as_u32()).unwrap_or(-1);
        let location_type = i32::try_from(ContentMessageType::Location.as_u32()).unwrap_or(-1);
        let contact_type = i32::try_from(ContentMessageType::ContactCard.as_u32()).unwrap_or(-1);
        let sticker_type = i32::try_from(ContentMessageType::Sticker.as_u32()).unwrap_or(-1);

        match message_type {
            Some(v) if v == image_type => return "image",
            Some(v) if v == file_type => {
                let mime = content_json
                    .and_then(|value| Self::json_get_string(value, &["mime_type"]))
                    .or_else(|| {
                        extra_json.and_then(|value| Self::json_get_string(value, &["mime_type"]))
                    })
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                return if mime.starts_with("image/") {
                    "image"
                } else if mime.starts_with("video/") {
                    "video"
                } else if mime.starts_with("audio/") {
                    "voice"
                } else {
                    "file"
                };
            }
            Some(v) if v == voice_type || v == audio_type => return "voice",
            Some(v) if v == video_type => return "video",
            Some(v) if v == system_type => return "system",
            Some(v) if v == location_type => return "location",
            Some(v) if v == contact_type => return "contact_card",
            Some(v) if v == sticker_type => return "sticker",
            _ => {}
        }

        let detected = content_json
            .and_then(Self::preview_type_from_json)
            .or_else(|| extra_json.and_then(Self::preview_type_from_json));
        match detected.as_deref() {
            Some("image") => "image",
            Some("video") => "video",
            Some("voice") | Some("audio") => "voice",
            Some("file") => "file",
            Some("location") => "location",
            Some("contact_card") | Some("contactcard") | Some("card") => "contact_card",
            Some("red_packet") | Some("redpacket") | Some("hongbao") => "red_packet",
            Some("sticker") | Some("emoji") => "sticker",
            Some("system") | Some("tip") => "system",
            Some("revoked") => "revoked",
            _ => {
                let revoked = content_json
                    .and_then(|value| Self::json_get_bool(value, &["revoked"]))
                    .or_else(|| {
                        extra_json.and_then(|value| Self::json_get_bool(value, &["revoked"]))
                    })
                    .unwrap_or(false);
                if revoked {
                    "revoked"
                } else {
                    "text"
                }
            }
        }
    }

    fn format_conversation_preview(
        message_type: Option<i32>,
        content: &str,
        extra: Option<&str>,
    ) -> String {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        if Self::is_formatted_preview(trimmed) {
            return trimmed.to_string();
        }

        let content_json = serde_json::from_str::<serde_json::Value>(trimmed).ok();
        let extra_json = extra
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
        let preview_kind =
            Self::infer_preview_kind(message_type, content_json.as_ref(), extra_json.as_ref());

        match preview_kind {
            "image" => "[图片]".to_string(),
            "video" => "[视频]".to_string(),
            "voice" => {
                let duration = content_json
                    .as_ref()
                    .and_then(Self::preview_duration_from_json)
                    .or_else(|| {
                        extra_json
                            .as_ref()
                            .and_then(Self::preview_duration_from_json)
                    })
                    .unwrap_or_default();
                if duration > 0 {
                    format!("[语音]{duration}\"")
                } else {
                    "[语音]".to_string()
                }
            }
            "file" => "[文件]".to_string(),
            "location" => "[位置]".to_string(),
            "contact_card" => "[名片]".to_string(),
            "red_packet" => "[红包]".to_string(),
            "sticker" => "[表情]".to_string(),
            "system" => content_json
                .as_ref()
                .and_then(Self::preview_text_from_json)
                .or_else(|| extra_json.as_ref().and_then(Self::preview_text_from_json))
                .unwrap_or_else(|| trimmed.to_string()),
            "revoked" => "撤回了一条消息".to_string(),
            _ => content_json
                .as_ref()
                .and_then(Self::preview_text_from_json)
                .or_else(|| extra_json.as_ref().and_then(Self::preview_text_from_json))
                .unwrap_or_else(|| trimmed.to_string()),
        }
    }

    async fn materialize_channel_preview(&self, mut channel: StoredChannel) -> StoredChannel {
        let preview = if channel.last_local_message_id > 0 {
            match self
                .storage
                .get_message_by_id(channel.last_local_message_id)
                .await
            {
                Ok(Some(message))
                    if message.channel_id == channel.channel_id
                        && message.channel_type == channel.channel_type =>
                {
                    Self::format_conversation_preview(
                        Some(message.message_type),
                        &message.content,
                        Some(&message.extra),
                    )
                }
                _ => Self::format_conversation_preview(None, &channel.last_msg_content, None),
            }
        } else {
            Self::format_conversation_preview(None, &channel.last_msg_content, None)
        };
        channel.last_msg_content = preview;
        channel
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
        // JPEG does not support RGBA pixel format on this encoder path.
        // Convert to RGB explicitly to avoid "Rgba8 not supported" failures.
        let rgb = thumb.to_rgb8();
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
                rgb.as_raw(),
                thumb.width(),
                thumb.height(),
                image::ExtendedColorType::Rgb8,
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
        let response: FileRequestUploadTokenResponse = self
            .rpc_call_typed(routes::file::REQUEST_UPLOAD_TOKEN, &payload)
            .await?;
        if response.token.trim().is_empty() {
            return Err(Error::Serialization(
                "decode file/request_upload_token response: missing token".to_string(),
            ));
        }
        if response.upload_url.trim().is_empty() {
            return Err(Error::Serialization(
                "decode file/request_upload_token response: missing upload_url".to_string(),
            ));
        }
        Ok(response)
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
            .map_err(|e| Error::Serialization(format!("invalid mime_type for upload part: {e}")))?;
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
        let file_url = value
            .get("file_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let thumbnail_url = value
            .get("thumbnail_url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let file_size = value.get("file_size").and_then(|v| v.as_u64()).unwrap_or(0);
        let original_size = value.get("original_size").and_then(|v| v.as_u64());
        let width = value
            .get("width")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let height = value
            .get("height")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let resp_mime = value
            .get("mime_type")
            .and_then(|v| v.as_str())
            .unwrap_or(mime_type)
            .to_string();
        Ok(UploadedFileInfo {
            file_id,
            storage_source_id,
            file_url,
            thumbnail_url,
            file_size,
            original_size,
            width,
            height,
            mime_type: resp_mime,
        })
    }

    async fn upload_callback(
        &mut self,
        user_id: u64,
        token: &str,
        uploaded: &UploadedFileInfo,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "token": token,
            "file_id": uploaded.file_id,
            "file_url": uploaded.file_url,
            "thumbnail_url": uploaded.thumbnail_url,
            "file_size": uploaded.file_size,
            "original_size": uploaded.original_size,
            "mime_type": uploaded.mime_type,
            "width": uploaded.width,
            "height": uploaded.height,
            "user_id": user_id,
            "status": "uploaded",
        });
        let value: serde_json::Value = self
            .rpc_call_json(routes::file::UPLOAD_CALLBACK.to_string(), payload)
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
    ) -> Result<SendMessageResponse> {
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

        let uploaded_thumbnail = if let Some((thumb_path, thumb_mime, thumb_name)) = thumb_upload {
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
            self.upload_callback(message.from_uid, &thumb_token.token, &uploaded_thumb)
                .await?;
            Some(uploaded_thumb)
        } else {
            None
        };

        let token = self
            .request_upload_token(
                message.from_uid,
                upload_filename.clone(),
                upload_payload.len() as i64,
                mime_type.clone(),
                file_type.clone(),
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
        self.upload_callback(message.from_uid, &token.token, &uploaded)
            .await?;

        let uploaded_file_id = uploaded.file_id.parse::<u64>().map_err(|_| {
            Error::Serialization(format!(
                "upload response invalid numeric file_id: {}",
                uploaded.file_id
            ))
        })?;
        let thumbnail_file_id_u64 = uploaded_thumbnail
            .as_ref()
            .map(|uploaded| {
                uploaded.file_id.parse::<u64>().map_err(|_| {
                    Error::Serialization(format!(
                        "upload response invalid numeric thumbnail_file_id: {}",
                        uploaded.file_id
                    ))
                })
            })
            .transpose()?;

        let attachment_content = if let Some(thumb_file_id) = thumbnail_file_id_u64 {
            let thumbnail_url = uploaded_thumbnail
                .as_ref()
                .map(|v| v.file_url.clone())
                .unwrap_or_default();
            serde_json::json!({
                "file_type": file_type,
                "file_id": uploaded_file_id,
                "thumbnail_file_id": thumb_file_id,
                "filename": upload_filename,
                "mime_type": mime_type,
                "storage_source_id": uploaded.storage_source_id,
                "file_size": uploaded.file_size,
                "file_url": uploaded.file_url,
                "thumbnail_url": thumbnail_url,
            })
        } else {
            serde_json::json!({
                "file_type": file_type,
                "file_id": uploaded_file_id,
                "filename": upload_filename,
                "mime_type": mime_type,
                "storage_source_id": uploaded.storage_source_id,
                "file_size": uploaded.file_size,
                "file_url": uploaded.file_url,
                "thumbnail_url": uploaded.thumbnail_url,
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

    async fn rpc_call_typed<Req, Resp>(&mut self, route: &str, body: &Req) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let value = self
            .rpc_call_json(
                route.to_string(),
                serde_json::to_value(body)
                    .map_err(|e| Error::Serialization(format!("encode {route} body: {e}")))?,
            )
            .await?;
        serde_json::from_value::<Resp>(value)
            .map_err(|e| Error::Serialization(format!("decode {route} response: {e}")))
    }

    async fn update_channel_last_message(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        content: &str,
        timestamp_ms: i64,
        message_id: u64,
        from_uid: Option<u64>,
        bump_unread: bool,
    ) -> Result<()> {
        let existing = self.storage.get_channel_by_id(channel_id).await?;
        let unread_before = existing.as_ref().map(|c| c.unread_count).unwrap_or(0);
        let (channel_name, channel_remark, avatar, top, mute, unread_count) =
            if let Some(c) = existing {
                (
                    c.channel_name,
                    c.channel_remark,
                    c.avatar,
                    c.top,
                    c.mute,
                    if bump_unread {
                        c.unread_count.saturating_add(1)
                    } else {
                        c.unread_count
                    },
                )
            } else {
                let current_uid = self
                    .current_uid
                    .as_ref()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or_default();
                let inferred_dm_name = if channel_type == 1 {
                    match from_uid {
                        Some(1) => "System Message".to_string(),
                        Some(uid) if uid > 0 && uid != current_uid => uid.to_string(),
                        _ => String::new(),
                    }
                } else {
                    channel_id.to_string()
                };
                (
                    inferred_dm_name,
                    String::new(),
                    String::new(),
                    0,
                    0,
                    if bump_unread { 1 } else { 0 },
                )
            };
        if inbound_logs_enabled() {
            eprintln!(
                "[SDK.unread] update_channel_last_message: channel_id={} channel_type={} bump_unread={} unread_before={} unread_after={} message_id={} from_uid={:?}",
                channel_id,
                channel_type,
                bump_unread,
                unread_before,
                unread_count,
                message_id,
                from_uid
            );
        }
        let existing_version = self
            .storage
            .get_channel_by_id(channel_id)
            .await?
            .map(|c| c.version)
            .unwrap_or(0);
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
                // Timeline preview updates must not mint a synthetic entity version.
                // Keep the existing channel entity version so later sync_entities(channel)
                // payloads can still apply top/mute/name changes from the server.
                version: existing_version,
            })
            .await?;
        Ok(())
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
    snowflake: Arc<snowflake_me::Snowflake>,
    presence_cache: Arc<StdMutex<HashMap<u64, PresenceStatus>>>,
    typing_throttle: Arc<StdMutex<HashMap<(u64, bool, u8), std::time::Instant>>>,
}

impl PrivchatSdk {
    fn is_timeline_like_event(event: &SdkEvent) -> bool {
        matches!(
            event,
            SdkEvent::TimelineUpdated { .. }
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
            SdkEvent::ConnectionStateChanged { .. }
                | SdkEvent::NetworkHintChanged { .. }
                | SdkEvent::ResumeSyncStarted
                | SdkEvent::ResumeSyncCompleted { .. }
                | SdkEvent::ResumeSyncFailed { .. }
                | SdkEvent::ResumeSyncEscalated { .. }
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
        let presence_cache = Arc::new(StdMutex::new(HashMap::new()));
        let actor_presence_cache = presence_cache.clone();
        let machine_id: u16 = (std::process::id() as u16) & 0x1f;
        let data_center_id: u16 = (chrono::Utc::now().timestamp_millis() as u16) & 0x1f;
        let snowflake = snowflake_me::Snowflake::builder()
            .machine_id(&|| Ok(machine_id))
            .data_center_id(&|| Ok(data_center_id))
            .finalize()
            .map(Arc::new)
            .unwrap_or_else(|_| {
                // Fallback: use defaults
                Arc::new(
                    snowflake_me::Snowflake::builder()
                        .finalize()
                        .expect("default snowflake must work"),
                )
            });
        let actor_snowflake = snowflake.clone();
        let actor_task = runtime_provider.spawn(async move {
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] loop: started");
                if configured_data_dir.trim().is_empty() {
                    eprintln!("[SDK.actor] storage base: <default>");
                } else {
                    eprintln!("[SDK.actor] storage base: {}", configured_data_dir);
                }
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
            let snowflake = actor_snowflake;
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
                message_cache_policy: MessageCachePolicy::default(),
                channel_message_cache: HashMap::new(),
                channel_cache_generation: HashMap::new(),
                channel_cache_lru: VecDeque::new(),
                channel_cache_total_bytes: 0,
                cache_debug_log: std::env::var("PRIVCHAT_CACHE_LOG").ok().as_deref() == Some("1"),
                cache_hit_count: 0,
                cache_miss_count: 0,
                pending_prelogin_inbound_frames: Vec::new(),
                presence_cache: actor_presence_cache,
            };
            let mut inbound_task: Option<tokio::task::JoinHandle<()>> = None;
            let mut health_tick = interval(Duration::from_secs(15));
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
                                    if state.session_state == SessionState::Authenticated
                                        && state.bootstrap_completed
                                    {
                                        if let Err(err) = state.run_resume_sync().await {
                                            eprintln!("[SDK.actor] resume sync failed after reconnect: {err}");
                                        }
                                    }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd connect");
                        }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd disconnect");
                        }
                        state.should_auto_reconnect = false;
                        stop_inbound_task(&mut inbound_task).await;
                        let from_state = state.session_state.as_connection_state();
                        let result = state.disconnect().await;
                        if result.is_ok() && state.session_state != SessionState::Shutdown {
                            state.clear_presence_cache();
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
                                    if state.session_state == SessionState::Authenticated
                                        && state.bootstrap_completed
                                    {
                                        if let Err(err) = state.run_resume_sync().await {
                                            eprintln!("[SDK.actor] resume sync failed after network recovery: {err}");
                                        }
                                    }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd login");
                        }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd register");
                        }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd authenticate");
                        }
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd sync_entities");
                        }
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
                    Command::BatchGetPresence { user_ids, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(15),
                                state.batch_get_presence(user_ids),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => {
                                    Err(Error::Transport("batch_get_presence timeout".to_string()))
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
                    Command::Subscribe { channel_id, channel_type, token, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(10),
                                state.subscribe_channel(channel_id, channel_type, token),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(Error::Transport("subscribe_channel timeout".to_string())),
                            },
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::Unsubscribe { channel_id, channel_type, resp } => {
                        let result = match state.session_state.can(Action::Authenticate) {
                            Ok(_) => match timeout(
                                Duration::from_secs(10),
                                state.unsubscribe_channel(channel_id, channel_type),
                            )
                            .await
                            {
                                Ok(r) => r,
                                Err(_) => Err(Error::Transport("unsubscribe_channel timeout".to_string())),
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd run_bootstrap_sync");
                        }
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
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::MessageSendStatusChanged {
                                    message_id,
                                    status: 1,
                                    server_message_id: None,
                                },
                            );
                            let _ = actor_cmd_tx.try_send(Command::KickOutboundDrain);
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
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::MessageSendStatusChanged {
                                    message_id,
                                    status: 1,
                                    server_message_id: None,
                                },
                            );
                            let _ = actor_cmd_tx.try_send(Command::KickOutboundDrain);
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
                    Command::KickOutboundDrain => {
                        if state.should_process_outbound_queue() {
                            let _ = state.drain_outbound_queues().await;
                        }
                    }
                    Command::CreateLocalMessage { input, local_message_id, resp } => {
                        let channel_id = input.channel_id;
                        let channel_type = input.channel_type;
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                let lid = match local_message_id {
                                    Some(id) => Ok(id),
                                    None => state.next_local_message_id(),
                                };
                                match lid {
                                    Ok(local_message_id) => {
                                        state
                                            .storage
                                            .create_local_message(local_message_id, input)
                                            .await
                                    }
                                    Err(e) => Err(e),
                                }
                            },
                            Err(e) => Err(e),
                        };
                        if let Ok(message_id) = result {
                            state.invalidate_channel_cache_with_reason(
                                channel_id,
                                channel_type,
                                "create_local_message",
                            );
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
                            Ok(_) => state
                                .query_timeline_snapshot(channel_id, channel_type, limit, offset)
                                .await
                                .map(|snapshot| snapshot.messages),
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::QueryTimelineSnapshot {
                        channel_id,
                        channel_type,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .query_timeline_snapshot(channel_id, channel_type, limit, offset)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::SetMessageCachePolicy { policy, resp } => {
                        state.set_message_cache_policy(policy);
                        let _ = resp.send(Ok(()));
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
                            Ok(_) => match state.storage.get_channel_by_id(channel_id).await {
                                Ok(Some(channel)) => Ok(Some(state.materialize_channel_preview(channel).await)),
                                Ok(None) => Ok(None),
                                Err(e) => Err(e),
                            },
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
                            Ok(_) => match state.storage.list_channels(limit, offset).await {
                                Ok(channels) => {
                                    let mut formatted = Vec::with_capacity(channels.len());
                                    for channel in channels {
                                        formatted.push(state.materialize_channel_preview(channel).await);
                                    }
                                    Ok(formatted)
                                }
                                Err(e) => Err(e),
                            },
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
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => state
                                .storage
                                .mark_message_sent(message_id, server_message_id)
                                .await,
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            if let Some(msg) = &message_ctx {
                                state.invalidate_channel_cache_with_reason(
                                    msg.channel_id,
                                    msg.channel_type,
                                    "mark_message_sent",
                                );
                            }
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
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.update_message_status(message_id, status).await,
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            if let Some(msg) = &message_ctx {
                                state.invalidate_channel_cache_with_reason(
                                    msg.channel_id,
                                    msg.channel_type,
                                    "update_message_status",
                                );
                            }
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
                            if let Some(msg) = &message_ctx {
                                state.invalidate_channel_cache_with_reason(
                                    msg.channel_id,
                                    msg.channel_type,
                                    "set_message_revoke",
                                );
                            }
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
                            if let Some(msg) = &message_ctx {
                                state.invalidate_channel_cache_with_reason(
                                    msg.channel_id,
                                    msg.channel_type,
                                    "edit_message",
                                );
                            }
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
                            if let Some(msg) = &message_ctx {
                                state.invalidate_channel_cache_with_reason(
                                    msg.channel_id,
                                    msg.channel_type,
                                    "set_message_pinned",
                                );
                            }
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
                    Command::ProjectChannelReadCursor {
                        channel_id,
                        channel_type,
                        last_read_pts,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .project_channel_read_cursor(
                                        channel_id,
                                        channel_type,
                                        last_read_pts,
                                    )
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
                        state.clear_presence_cache();
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
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd shutdown");
                        }
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
                        state.clear_presence_cache();
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
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] loop: receiver closed");
            }
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
            snowflake,
            presence_cache,
            typing_throttle: Arc::new(StdMutex::new(HashMap::new())),
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
        if actor_logs_enabled() {
            eprintln!("[SDK.api] connect: send");
        }
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Connect { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        let out = resp_rx.await.map_err(|_| self.actor_channel_error())?;
        if actor_logs_enabled() {
            eprintln!("[SDK.api] connect: recv");
        }
        out
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Disconnect { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        let out = resp_rx.await.map_err(|_| self.actor_channel_error())?;
        if out.is_ok() {
            self.clear_presence_cache();
        }
        out
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
        if actor_logs_enabled() {
            eprintln!("[SDK.api] login: send");
        }
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
        if actor_logs_enabled() {
            eprintln!("[SDK.api] login: recv");
        }
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
        if actor_logs_enabled() {
            eprintln!("[SDK.api] authenticate: send");
        }
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
        if actor_logs_enabled() {
            eprintln!("[SDK.api] authenticate: recv");
        }
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

    pub async fn batch_get_presence(&self, user_ids: Vec<u64>) -> Result<Vec<PresenceStatus>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::BatchGetPresence {
                user_ids,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn get_presence(&self, user_id: u64) -> Result<Option<PresenceStatus>> {
        let mut out = self.batch_get_presence(vec![user_id]).await?;
        Ok(out.pop())
    }

    pub fn batch_get_cached_presence(&self, user_ids: Vec<u64>) -> Vec<PresenceStatus> {
        let mut out = self
            .presence_cache
            .lock()
            .ok()
            .map(|locked| {
                user_ids
                    .iter()
                    .filter_map(|user_id| locked.get(user_id).cloned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.sort_by_key(|v| v.user_id);
        out
    }

    pub fn get_cached_presence(&self, user_id: u64) -> Option<PresenceStatus> {
        self.presence_cache
            .lock()
            .ok()
            .and_then(|locked| locked.get(&user_id).cloned())
    }

    pub fn clear_presence_cache(&self) {
        if let Ok(mut locked) = self.presence_cache.lock() {
            locked.clear();
        }
    }

    pub async fn send_typing(
        &self,
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        action_type: TypingActionType,
    ) -> Result<()> {
        self.ensure_running()?;

        // 1 秒节流：如果同一频道在 1 秒内已经上报过相同状态，则直接跳过
        let key = (channel_id, is_typing, action_type.clone() as u8);
        let now = std::time::Instant::now();
        {
            let mut locked = self.typing_throttle.lock().unwrap();
            if let Some(last_sent) = locked.get(&key) {
                if now.duration_since(*last_sent).as_millis() < 1000 {
                    return Ok(());
                }
            }
            locked.insert(key, now);
        }

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

    /// 订阅频道事件（进入聊天页面时调用，接收 typing / presence 等状态事件）
    /// channel_type: 0=Private, 1=Group, 2=Room
    /// token: 可选，Room 类型订阅时传入业务 API 签发的 ticket（JWT）
    pub async fn subscribe_channel(
        &self,
        channel_id: u64,
        channel_type: u8,
        token: Option<String>,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Subscribe {
                channel_id,
                channel_type,
                token,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 取消订阅频道事件（离开聊天页面时调用）
    /// channel_type: 0=Private, 1=Group, 2=Room
    pub async fn unsubscribe_channel(&self, channel_id: u64, channel_type: u8) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Unsubscribe {
                channel_id,
                channel_type,
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

    pub async fn rpc_call_typed<Req, Resp>(&self, route: &str, req: &Req) -> Result<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let body_json = serde_json::to_string(req)
            .map_err(|e| Error::Serialization(format!("encode {route} body: {e}")))?;
        let raw = self.rpc_call(route.to_string(), body_json).await?;
        self.apply_rpc_side_effects(route, &raw).await?;
        serde_json::from_str(&raw)
            .map_err(|e| Error::Serialization(format!("decode {route} response: {e}; raw={raw}")))
    }

    async fn apply_rpc_side_effects(&self, route: &str, raw: &str) -> Result<()> {
        if route == routes::friend::PENDING {
            let parsed = serde_json::from_str::<FriendPendingResponse>(raw).map_err(|e| {
                Error::Serialization(format!(
                    "decode {} side-effect response: {e}; raw={raw}",
                    routes::friend::PENDING
                ))
            })?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            for item in parsed.requests {
                self.upsert_user(UpsertUserInput {
                    user_id: item.user.user_id,
                    username: Some(item.user.username),
                    nickname: Some(item.user.nickname),
                    alias: None,
                    avatar: item.user.avatar_url.unwrap_or_default(),
                    user_type: item.user.user_type as i32,
                    is_deleted: false,
                    channel_id: String::new(),
                    version: 0,
                    updated_at: now_ms,
                })
                .await?;
            }
            return Ok(());
        }

        if route == routes::friend::ACCEPT {
            // Keep local entities fresh so direct-channel title can resolve from user table.
            let channel_id = serde_json::from_str::<u64>(raw).map_err(|e| {
                Error::Serialization(format!(
                    "decode {} side-effect response: {e}; raw={raw}",
                    routes::friend::ACCEPT
                ))
            })?;
            if channel_id > 0 {
                let _ = self.sync_channel(channel_id, 1).await;
            }
            let _ = self.sync_entities("friend".to_string(), None).await;
            let _ = self.sync_entities("user".to_string(), None).await;
        }

        Ok(())
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

    pub fn generate_local_message_id(&self) -> Result<u64> {
        self.snowflake
            .next_id()
            .map_err(|e| Error::Storage(format!("generate local_message_id failed: {e:?}")))
    }

    pub async fn create_local_message(&self, input: NewMessage) -> Result<u64> {
        self.create_local_message_with_id(input, None).await
    }

    pub async fn create_local_message_with_id(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
    ) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::CreateLocalMessage {
                input,
                local_message_id,
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

    pub async fn query_timeline_snapshot(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<TimelineSnapshot> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::QueryTimelineSnapshot {
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

    pub async fn set_message_cache_policy(&self, policy: MessageCachePolicy) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetMessageCachePolicy {
                policy,
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

    pub async fn project_channel_read_cursor(
        &self,
        channel_id: u64,
        channel_type: i32,
        last_read_pts: u64,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ProjectChannelReadCursor {
                channel_id,
                channel_type,
                last_read_pts,
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

    async fn kv_put_local(&self, key: String, value: Vec<u8>) -> Result<()> {
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

    async fn kv_get_local(&self, key: String) -> Result<Option<Vec<u8>>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::KvGet { key, resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn set_channel_notification_mode_pref(
        &self,
        channel_id: u64,
        channel_type: i32,
        mode: i32,
    ) -> Result<()> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get_local(key.clone()).await?;
        let mut state = decode_channel_prefs(raw);
        state.notification_mode = mode;
        self.kv_put_local(
            key,
            serde_json::to_vec(&state).map_err(|e| {
                Error::Serialization(format!(
                    "encode channel prefs notification_mode failed: {e}"
                ))
            })?,
        )
        .await
    }

    pub async fn channel_notification_mode_pref(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get_local(key).await?;
        Ok(decode_channel_prefs(raw).notification_mode)
    }

    pub async fn set_channel_favourite_pref(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<()> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get_local(key.clone()).await?;
        let mut state = decode_channel_prefs(raw);
        state.favourite = enabled;
        self.kv_put_local(
            key,
            serde_json::to_vec(&state).map_err(|e| {
                Error::Serialization(format!("encode channel prefs favourite failed: {e}"))
            })?,
        )
        .await
    }

    pub async fn set_channel_low_priority_pref(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<()> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get_local(key.clone()).await?;
        let mut state = decode_channel_prefs(raw);
        state.low_priority = enabled;
        self.kv_put_local(
            key,
            serde_json::to_vec(&state).map_err(|e| {
                Error::Serialization(format!("encode channel prefs low_priority failed: {e}"))
            })?,
        )
        .await
    }

    pub async fn channel_tags_pref(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Vec<String>> {
        let key = channel_prefs_key(channel_id, channel_type);
        let raw = self.kv_get_local(key).await?;
        Ok(decode_channel_prefs(raw).tags)
    }

    pub async fn cache_group_settings_json(
        &self,
        group_id: u64,
        payload_json: String,
    ) -> Result<()> {
        self.kv_put_local(group_settings_key(group_id), payload_json.into_bytes())
            .await
    }

    pub async fn update_group_mute_all_cache(&self, group_id: u64, enabled: bool) -> Result<()> {
        let key = group_settings_key(group_id);
        let raw = self.kv_get_local(key.clone()).await?;
        let mut state = decode_group_settings_cache(raw);
        state.group_id = group_id;
        state.mute_all = enabled;
        self.kv_put_local(
            key,
            serde_json::to_vec(&state).map_err(|e| {
                Error::Serialization(format!("encode group settings cache failed: {e}"))
            })?,
        )
        .await
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
        channel_prefs_key, decode_channel_prefs, decode_group_settings_cache, error_codes,
        group_settings_key, Action, ConnectionState, ContentMessageType, Error, ErrorCode,
        LoginResult, MessageCachePolicy, NetworkHint, PresenceStatus, PrivchatConfig, PrivchatSdk,
        ResumeEscalationScope, ResumeFailureClass, ResumeFailureTarget, SdkEvent, SessionState,
        State, UpsertChannelInput, UpsertFriendInput, UpsertGroupInput, UpsertGroupMemberInput,
        UpsertMessageReactionInput, UpsertRemoteMessageInput, UpsertUserInput,
        NETWORK_DISCONNECTED_MESSAGE,
    };
    use crate::local_store::LocalStore;
    use crate::receive_pipeline::ReceivePipeline;
    use crate::storage_actor::StorageHandle;
    use privchat_protocol::presence::{
        PresenceBatchStatusResponse, PresenceChangedNotification, PresenceSnapshot,
    };
    use privchat_protocol::rpc::sync::SyncEntityItem;
    use privchat_protocol::PushMessageRequest;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "privchat-sdk-{name}-{}-{nanos}",
            std::process::id()
        ))
    }

    async fn new_seeded_sdk(name: &str) -> (PrivchatSdk, PathBuf) {
        let dir = unique_test_dir(name);
        let store = LocalStore::open_at(dir.clone()).expect("open local store");
        let login = LoginResult {
            user_id: 10001,
            token: "token".to_string(),
            device_id: "device".to_string(),
            refresh_token: None,
            expires_at: 0,
        };
        store.save_login("10001", &login).expect("seed login");
        store
            .set_bootstrap_completed("10001", true)
            .expect("seed bootstrap completed");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("restore seeded current uid");

        for _ in 0..20 {
            if sdk
                .is_bootstrap_completed()
                .await
                .expect("query bootstrap completed")
            {
                return (sdk, dir);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("sdk actor did not load seeded bootstrap state");
    }

    async fn new_seeded_state(name: &str) -> (State, PathBuf) {
        let dir = unique_test_dir(name);
        let storage = StorageHandle::start_at(dir.clone()).expect("start storage at test dir");
        let login = LoginResult {
            user_id: 10001,
            token: "token".to_string(),
            device_id: "device".to_string(),
            refresh_token: None,
            expires_at: 0,
        };
        storage
            .save_login("10001".to_string(), login)
            .await
            .expect("seed login");
        storage
            .set_bootstrap_completed("10001".to_string(), true)
            .await
            .expect("seed bootstrap completed");
        storage
            .save_current_uid("10001".to_string())
            .await
            .expect("seed current uid");

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let state = State {
            config,
            transport: None,
            session_state: SessionState::Authenticated,
            bootstrap_completed: true,
            snowflake: Arc::new(
                snowflake_me::Snowflake::builder()
                    .finalize()
                    .expect("test snowflake"),
            ),
            storage,
            current_uid: Some("10001".to_string()),
            should_auto_reconnect: false,
            network_hint: NetworkHint::Unknown,
            receive_pipeline: ReceivePipeline::default(),
            last_sync_queued: 0,
            last_sync_dropped_duplicates: 0,
            last_sync_entity_events: Vec::new(),
            video_process_hook: None,
            last_tmp_cleanup_day: None,
            pending_events: Vec::new(),
            message_cache_policy: MessageCachePolicy::default(),
            channel_message_cache: HashMap::new(),
            channel_cache_generation: HashMap::new(),
            channel_cache_lru: VecDeque::new(),
            channel_cache_total_bytes: 0,
            cache_debug_log: false,
            cache_hit_count: 0,
            cache_miss_count: 0,
            pending_prelogin_inbound_frames: Vec::new(),
            presence_cache: Arc::new(StdMutex::new(HashMap::new())),
            typing_throttle: Arc::new(StdMutex::new(HashMap::new())),
        };
        (state, dir)
    }

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
    fn channel_unread_prefers_local_projection_once_materialized() {
        assert_eq!(State::resolve_channel_unread_count(None, Some(7)), 7);
        assert_eq!(State::resolve_channel_unread_count(Some(3), Some(7)), 3);
        assert_eq!(State::resolve_channel_unread_count(Some(-2), Some(7)), 0);
        assert_eq!(State::resolve_channel_unread_count(None, Some(-5)), 0);
    }

    #[test]
    fn difference_commit_maps_revoke_to_message_extra() {
        let commit = privchat_protocol::rpc::sync::ServerCommit {
            pts: 12,
            server_msg_id: 9001,
            local_message_id: None,
            channel_id: 100,
            channel_type: 1,
            message_type: "message.revoke".to_string(),
            content: serde_json::json!({
                "message_id": 777,
                "revoke": true,
                "revoked_by": 42,
            }),
            server_timestamp: 1_700_000_000_000,
            sender_id: 42,
            sender_info: None,
        };

        let (entity_type, item) = State::sync_item_from_difference_commit(&commit);
        assert_eq!(entity_type, "message_extra");
        assert_eq!(item.version, 12);
        let payload = item.payload.expect("payload");
        assert_eq!(
            payload.get("message_id").and_then(|v| v.as_u64()),
            Some(777)
        );
        assert_eq!(
            payload.get("channel_id").and_then(|v| v.as_u64()),
            Some(100)
        );
        assert_eq!(payload.get("revoke").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn difference_commit_maps_reaction_to_message_reaction() {
        let commit = privchat_protocol::rpc::sync::ServerCommit {
            pts: 34,
            server_msg_id: 9002,
            local_message_id: None,
            channel_id: 101,
            channel_type: 1,
            message_type: "message_reaction".to_string(),
            content: serde_json::json!({
                "message_id": 778,
                "uid": 43,
                "emoji": "👍",
                "deleted": true,
            }),
            server_timestamp: 1_700_000_100_000,
            sender_id: 43,
            sender_info: None,
        };

        let (entity_type, item) = State::sync_item_from_difference_commit(&commit);
        assert_eq!(entity_type, "message_reaction");
        assert_eq!(item.version, 34);
        assert_eq!(item.entity_id, "778:43:👍");
        let payload = item.payload.expect("payload");
        assert_eq!(
            payload.get("message_id").and_then(|v| v.as_u64()),
            Some(778)
        );
        assert_eq!(payload.get("uid").and_then(|v| v.as_u64()), Some(43));
        assert_eq!(payload.get("deleted").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn stable_sdk_error_codes_are_mapped() {
        assert_eq!(
            Error::Transport("x".to_string()).sdk_code(),
            error_codes::TRANSPORT_FAILURE
        );
        assert_eq!(
            Error::NotConnected.sdk_code(),
            error_codes::NETWORK_DISCONNECTED
        );
        assert_eq!(
            Error::Storage("x".to_string()).sdk_code(),
            error_codes::STORAGE_FAILURE
        );
        assert_eq!(
            Error::Auth("x".to_string()).sdk_code(),
            error_codes::AUTH_FAILURE
        );
        assert_eq!(Error::Shutdown.sdk_code(), error_codes::SHUTDOWN);
        assert_eq!(
            Error::InvalidState("x".to_string()).sdk_code(),
            error_codes::INVALID_STATE
        );
        assert_eq!(
            ResumeFailureClass::RetryableTemporaryError.sdk_code(),
            error_codes::RESUME_RETRYABLE_TEMPORARY
        );
        assert_eq!(
            ResumeFailureClass::ChannelResyncRequired.sdk_code(),
            error_codes::RESUME_CHANNEL_RESYNC_REQUIRED
        );
        assert_eq!(
            ResumeFailureClass::EntityResyncRequired.sdk_code(),
            error_codes::RESUME_ENTITY_RESYNC_REQUIRED
        );
        assert_eq!(
            ResumeFailureClass::FullRebuildRequired.sdk_code(),
            error_codes::RESUME_FULL_REBUILD_REQUIRED
        );
        assert_eq!(
            ResumeFailureClass::FatalProtocolError.sdk_code(),
            error_codes::RESUME_FATAL_PROTOCOL_ERROR
        );
    }

    #[test]
    fn resume_error_classification_prefers_channel_gap() {
        let err = Error::Auth("get_difference rejected: pts too old, gap detected".to_string());
        assert_eq!(
            State::classify_resume_error(&err),
            ResumeFailureClass::ChannelResyncRequired
        );
    }

    #[test]
    fn resume_error_classification_uses_sync_protocol_codes() {
        let err = State::sync_rpc_rejection(
            "get_difference",
            ErrorCode::SyncChannelResyncRequired.code() as i32,
            "message history window unavailable".to_string(),
        );
        assert_eq!(
            State::classify_resume_error(&err),
            ResumeFailureClass::ChannelResyncRequired
        );

        let err = State::sync_rpc_rejection(
            "sync_entities",
            ErrorCode::SyncFullRebuildRequired.code() as i32,
            "session rebuild required".to_string(),
        );
        assert_eq!(
            State::classify_resume_error(&err),
            ResumeFailureClass::FullRebuildRequired
        );
    }

    #[test]
    fn channel_resume_failure_isolated_as_channel_scoped_resync() {
        let err = Error::Serialization("decode get_difference response: bad schema".to_string());
        let classification = State::classify_resume_error(&err);
        let scope = State::resume_escalation_scope(
            classification,
            &ResumeFailureTarget::Channel {
                channel_id: 42,
                channel_type: 1,
            },
        );
        assert_eq!(classification, ResumeFailureClass::FatalProtocolError);
        assert_eq!(scope, ResumeEscalationScope::ChannelScopedResync);
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
        let sdk = PrivchatSdk::new(PrivchatConfig::default());
        sdk.shutdown().await;
        sdk.shutdown().await;

        let err = sdk
            .connect()
            .await
            .expect_err("connect should fail after shutdown");
        assert!(matches!(err, Error::Shutdown));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn network_hint_offline_emits_event_and_blocks_connect() {
        let sdk = PrivchatSdk::new(PrivchatConfig::default());
        let baseline = sdk.last_event_sequence_id();

        sdk.set_network_hint(NetworkHint::Offline)
            .await
            .expect("set_network_hint should succeed");

        let network_events = sdk.network_events_since(baseline, 16);
        assert!(
            network_events
                .iter()
                .any(|evt| matches!(evt.event, SdkEvent::NetworkHintChanged { .. })),
            "expected NetworkHintChanged in replay events"
        );

        let err = sdk
            .connect()
            .await
            .expect_err("connect must fail while offline");
        assert!(
            err.to_string().contains(NETWORK_DISCONNECTED_MESSAGE),
            "unexpected error: {err}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn network_hint_recovery_is_replayable_without_polling() {
        let sdk = PrivchatSdk::new(PrivchatConfig::default());

        sdk.set_network_hint(NetworkHint::Offline)
            .await
            .expect("set offline");
        let after_offline = sdk.last_event_sequence_id();

        sdk.set_network_hint(NetworkHint::Unknown)
            .await
            .expect("set unknown");

        let events = sdk.network_events_since(after_offline, 16);
        assert!(
            events.iter().any(|evt| matches!(
                evt.event,
                SdkEvent::NetworkHintChanged {
                    from: NetworkHint::Offline,
                    to: NetworkHint::Unknown
                }
            )),
            "expected offline->unknown network hint event"
        );
    }

    #[test]
    fn entity_version_gate_rejects_stale_payloads() {
        assert!(State::should_apply_entity_version(None, 1));
        assert!(State::should_apply_entity_version(Some(5), 5));
        assert!(State::should_apply_entity_version(Some(5), 6));
        assert!(!State::should_apply_entity_version(Some(5), 4));
    }

    #[test]
    fn attachment_placeholder_text_is_protocol_aligned() {
        assert_eq!(State::attachment_placeholder_text("image"), "[图片]");
        assert_eq!(State::attachment_placeholder_text("video"), "[视频]");
        assert_eq!(State::attachment_placeholder_text("file"), "[文件]");
        assert_eq!(State::attachment_placeholder_text("audio"), "[文件]");
        assert_eq!(State::attachment_placeholder_text("unknown"), "[文件]");
    }

    #[test]
    fn conversation_preview_is_rendered_in_sdk_layer() {
        assert_eq!(
            State::format_conversation_preview(
                Some(i32::try_from(ContentMessageType::Voice.as_u32()).unwrap_or(0)),
                "{\"type\":\"voice\",\"duration\":3}",
                Some("{}"),
            ),
            "[语音]3\""
        );
        assert_eq!(
            State::format_conversation_preview(
                Some(i32::try_from(ContentMessageType::Text.as_u32()).unwrap_or(0)),
                "{\"content\":\"hello\"}",
                Some("{}"),
            ),
            "hello"
        );
        assert_eq!(
            State::format_conversation_preview(None, "{\"type\":\"contact_card\"}", None),
            "[名片]"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_prefs_roundtrip_through_semantic_api() {
        let (sdk, dir) = new_seeded_sdk("channel-prefs").await;

        sdk.set_channel_notification_mode_pref(42, 1, 3)
            .await
            .expect("set notification mode");
        sdk.set_channel_favourite_pref(42, 1, true)
            .await
            .expect("set favourite");
        sdk.set_channel_low_priority_pref(42, 1, true)
            .await
            .expect("set low priority");

        let mode = sdk
            .channel_notification_mode_pref(42, 1)
            .await
            .expect("get notification mode");
        assert_eq!(mode, 3);

        let raw = sdk
            .kv_get_local(channel_prefs_key(42, 1))
            .await
            .expect("read channel prefs");
        let state = decode_channel_prefs(raw);
        assert!(state.favourite);
        assert!(state.low_priority);
        assert_eq!(state.notification_mode, 3);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn group_settings_cache_updates_mute_all_without_extra_model() {
        let (sdk, dir) = new_seeded_sdk("group-settings").await;

        sdk.cache_group_settings_json(
            7,
            serde_json::json!({
                "group_id": 7,
                "name": "team",
                "mute_all": false
            })
            .to_string(),
        )
        .await
        .expect("cache group settings");
        sdk.update_group_mute_all_cache(7, true)
            .await
            .expect("update mute all");

        let raw = sdk
            .kv_get_local(group_settings_key(7))
            .await
            .expect("read group settings cache");
        let state = decode_group_settings_cache(raw);
        assert_eq!(state.group_id, 7);
        assert!(state.mute_all);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn group_member_tombstone_removes_local_member() {
        let (mut state, dir) = new_seeded_state("group-member-tombstone").await;

        state
            .storage
            .upsert_group_member(UpsertGroupMemberInput {
                group_id: 30001,
                user_id: 20001,
                role: 0,
                status: 0,
                alias: Some("owner".to_string()),
                is_muted: false,
                joined_at: 1000,
                version: 10,
                updated_at: 1000,
            })
            .await
            .expect("seed group member");

        let deleted = SyncEntityItem {
            entity_id: "30001:20001".to_string(),
            version: 11,
            deleted: true,
            payload: Some(serde_json::json!({
                "group_id": 30001,
                "user_id": 20001
            })),
        };
        state
            .apply_sync_entities("group_member", Some("30001"), &[deleted], false)
            .await
            .expect("apply group member tombstone");

        let members = state
            .storage
            .list_group_members(30001, 20, 0)
            .await
            .expect("list group members");
        assert!(
            members.is_empty(),
            "group member tombstone should remove local member"
        );

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn friend_tombstone_removes_local_friend() {
        let (mut state, dir) = new_seeded_state("friend-tombstone").await;

        state
            .storage
            .upsert_user(UpsertUserInput {
                user_id: 20001,
                username: Some("bob".to_string()),
                nickname: Some("Bob".to_string()),
                alias: Some("B".to_string()),
                avatar: "avatar://bob".to_string(),
                user_type: 0,
                is_deleted: false,
                channel_id: String::new(),
                version: 9,
                updated_at: 900,
            })
            .await
            .expect("seed friend user");
        state
            .storage
            .upsert_friend(UpsertFriendInput {
                user_id: 20001,
                tags: Some("work".to_string()),
                is_pinned: false,
                created_at: 800,
                version: 10,
                updated_at: 900,
            })
            .await
            .expect("seed friend row");

        let deleted = SyncEntityItem {
            entity_id: "20001".to_string(),
            version: 11,
            deleted: true,
            payload: None,
        };
        state
            .apply_sync_entities("friend", None, &[deleted], false)
            .await
            .expect("apply friend tombstone");

        let friends = state
            .storage
            .list_friends(20, 0)
            .await
            .expect("list friends after tombstone");
        assert!(
            friends.is_empty(),
            "friend tombstone should remove local friend row"
        );

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn batch_get_presence_response_populates_rust_presence_cache() {
        let (state, dir) = new_seeded_state("presence-cache-response").await;

        let out = state.cache_presence_response(PresenceBatchStatusResponse {
            items: vec![
                PresenceSnapshot {
                    user_id: 20002,
                    is_online: false,
                    last_seen_at: 1_710_000_002,
                    device_count: 0,
                    version: 3,
                },
                PresenceSnapshot {
                    user_id: 20001,
                    is_online: true,
                    last_seen_at: 1_710_000_001,
                    device_count: 2,
                    version: 7,
                },
            ],
            denied_user_ids: vec![20003],
        });

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].user_id, 20001);
        assert_eq!(out[1].user_id, 20002);

        let cached = state.presence_cache.lock().expect("presence cache").clone();
        assert_eq!(cached.len(), 2);
        assert_eq!(cached.get(&20001).map(|v| v.version), Some(7));
        assert_eq!(cached.get(&20002).map(|v| v.version), Some(3));

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn presence_changed_updates_rust_presence_cache_without_regressing_version() {
        let (state, dir) = new_seeded_state("presence-cache-event").await;

        state.update_presence_cache(&[PresenceStatus {
            user_id: 20001,
            is_online: false,
            last_seen_at: 1_710_000_000,
            device_count: 0,
            version: 4,
        }]);

        let newer = PresenceChangedNotification {
            user_id: 20001,
            version: 6,
            snapshot: PresenceSnapshot {
                user_id: 20001,
                is_online: true,
                last_seen_at: 1_710_000_010,
                device_count: 1,
                version: 6,
            },
        };
        state.apply_presence_changed_payload(
            &serde_json::to_vec(&newer).expect("encode presence_changed newer"),
        );

        let after_newer = state
            .presence_cache
            .lock()
            .expect("presence cache after newer")
            .get(&20001)
            .cloned()
            .expect("cached presence after newer");
        assert_eq!(after_newer.version, 6);
        assert!(after_newer.is_online);
        assert_eq!(after_newer.device_count, 1);

        let older = PresenceChangedNotification {
            user_id: 20001,
            version: 5,
            snapshot: PresenceSnapshot {
                user_id: 20001,
                is_online: false,
                last_seen_at: 1_710_000_020,
                device_count: 0,
                version: 5,
            },
        };
        state.apply_presence_changed_payload(
            &serde_json::to_vec(&older).expect("encode presence_changed older"),
        );

        let after_older = state
            .presence_cache
            .lock()
            .expect("presence cache after older")
            .get(&20001)
            .cloned()
            .expect("cached presence after older");
        assert_eq!(after_older.version, 6);
        assert!(after_older.is_online);
        assert_eq!(after_older.device_count, 1);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_sync_uses_server_unread_for_cold_start() {
        let (mut state, dir) = new_seeded_state("channel-cold-start").await;

        let item = SyncEntityItem {
            entity_id: "90001".to_string(),
            version: 5,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 90001,
                "channel_type": 2,
                "channel_name": "project-room",
                "avatar": "https://example.com/room.png",
                "unread_count": 7,
                "top": 1,
                "mute": 1
            })),
        };
        state
            .apply_sync_entities("channel", None, &[item], false)
            .await
            .expect("apply cold-start channel");

        let channel = state
            .storage
            .get_channel_by_id(90001)
            .await
            .expect("read channel")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "project-room");
        assert_eq!(channel.unread_count, 7);
        assert_eq!(channel.top, 1);
        assert_eq!(channel.mute, 1);
        assert_eq!(channel.version, 5);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_sync_rejects_stale_version_and_keeps_local_projection() {
        let (mut state, dir) = new_seeded_state("channel-stale-sync").await;

        state
            .storage
            .upsert_channel(UpsertChannelInput {
                channel_id: 91001,
                channel_type: 2,
                channel_name: "latest-room".to_string(),
                channel_remark: "latest-remark".to_string(),
                avatar: "https://example.com/latest.png".to_string(),
                unread_count: 3,
                top: 1,
                mute: 0,
                last_msg_timestamp: 111,
                last_local_message_id: 9,
                last_msg_content: "latest".to_string(),
                version: 10,
            })
            .await
            .expect("seed current channel");

        let stale = SyncEntityItem {
            entity_id: "91001".to_string(),
            version: 9,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 91001,
                "channel_type": 2,
                "channel_name": "stale-room",
                "avatar": "https://example.com/stale.png",
                "unread_count": 99,
                "top": 0,
                "mute": 1
            })),
        };
        state
            .apply_sync_entities("channel", None, &[stale], false)
            .await
            .expect("apply stale channel");

        let channel = state
            .storage
            .get_channel_by_id(91001)
            .await
            .expect("read channel after stale sync")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "latest-room");
        assert_eq!(channel.avatar, "https://example.com/latest.png");
        assert_eq!(channel.unread_count, 3);
        assert_eq!(channel.top, 1);
        assert_eq!(channel.mute, 0);
        assert_eq!(channel.version, 10);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resume_like_recovery_applies_entities_then_timeline_mutations() {
        let (mut state, dir) = new_seeded_state("resume-like-recovery").await;

        let channel = SyncEntityItem {
            entity_id: "92001".to_string(),
            version: 12,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 92001,
                "channel_type": 2,
                "channel_name": "resume-room",
                "avatar": "https://example.com/resume-room.png",
                "unread_count": 4,
                "top": 1,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[channel], false)
            .await
            .expect("apply channel entity");

        let seeded = state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 70001,
                local_message_id: 0,
                channel_id: 92001,
                channel_type: 2,
                timestamp: 1_709_999_999_000,
                from_uid: 20001,
                message_type: 1,
                content: "{\"content\":\"hello\"}".to_string(),
                status: 2,
                pts: 20,
                setting: 0,
                order_seq: 20,
                searchable_word: "hello".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed remote message");
        let local_message_id = seeded.message_id;

        let revoke_commit = privchat_protocol::rpc::sync::ServerCommit {
            pts: 21,
            server_msg_id: 70001,
            local_message_id: None,
            channel_id: 92001,
            channel_type: 2,
            message_type: "message.revoke".to_string(),
            content: serde_json::json!({
                "message_id": 70001,
                "revoke": true,
                "revoked_by": 10001
            }),
            server_timestamp: 1_710_000_000_000,
            sender_id: 10001,
            sender_info: None,
        };
        let (entity_type, item) = State::sync_item_from_difference_commit(&revoke_commit);
        state
            .enqueue_and_apply_sync_items(entity_type, Some("2:92001".to_string()), vec![item])
            .await
            .expect("apply revoke difference");

        let reaction_commit = privchat_protocol::rpc::sync::ServerCommit {
            pts: 22,
            server_msg_id: 70002,
            local_message_id: None,
            channel_id: 92001,
            channel_type: 2,
            message_type: "message_reaction".to_string(),
            content: serde_json::json!({
                "message_id": 70001,
                "uid": 20001,
                "emoji": "👍",
                "deleted": false
            }),
            server_timestamp: 1_710_000_000_100,
            sender_id: 20001,
            sender_info: None,
        };
        let (entity_type, item) = State::sync_item_from_difference_commit(&reaction_commit);
        state
            .enqueue_and_apply_sync_items(entity_type, Some("2:92001".to_string()), vec![item])
            .await
            .expect("apply reaction difference");

        let channel = state
            .storage
            .get_channel_by_id(92001)
            .await
            .expect("read channel")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "resume-room");
        assert_eq!(channel.unread_count, 4);
        assert_eq!(channel.top, 1);
        assert_eq!(channel.version, 12);

        let extra = state
            .storage
            .get_message_extra(local_message_id)
            .await
            .expect("read message extra")
            .expect("message extra exists");
        assert!(extra.revoke);

        let reactions = state
            .storage
            .list_message_reactions(local_message_id, 20, 0)
            .await
            .expect("list reactions");
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].emoji, "👍");
        assert!(!reactions[0].is_deleted);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_read_cursor_projection_takes_over_unread_after_materialization() {
        let (mut state, dir) = new_seeded_state("channel-read-cursor-unread").await;

        let channel = SyncEntityItem {
            entity_id: "93001".to_string(),
            version: 30,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 93001,
                "channel_type": 2,
                "channel_name": "cursor-room",
                "unread_count": 7,
                "top": 0,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[channel], false)
            .await
            .expect("apply channel baseline");

        state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 71001,
                local_message_id: 0,
                channel_id: 93001,
                channel_type: 2,
                timestamp: 1_710_100_000_001,
                from_uid: 20001,
                message_type: 1,
                content: "{\"content\":\"m1\"}".to_string(),
                status: 2,
                pts: 10,
                setting: 0,
                order_seq: 10,
                searchable_word: "m1".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed m1");
        state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 71002,
                local_message_id: 0,
                channel_id: 93001,
                channel_type: 2,
                timestamp: 1_710_100_000_002,
                from_uid: 20001,
                message_type: 1,
                content: "{\"content\":\"m2\"}".to_string(),
                status: 2,
                pts: 20,
                setting: 0,
                order_seq: 20,
                searchable_word: "m2".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed m2");
        state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 71003,
                local_message_id: 0,
                channel_id: 93001,
                channel_type: 2,
                timestamp: 1_710_100_000_003,
                from_uid: 20001,
                message_type: 1,
                content: "{\"content\":\"m3\"}".to_string(),
                status: 2,
                pts: 30,
                setting: 0,
                order_seq: 30,
                searchable_word: "m3".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed m3");

        let cursor = SyncEntityItem {
            entity_id: "93001:10001".to_string(),
            version: 31,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 93001,
                "channel_type": 2,
                "reader_id": 10001,
                "last_read_pts": 20
            })),
        };
        state
            .apply_sync_entities("channel_read_cursor", None, &[cursor], false)
            .await
            .expect("apply read cursor");

        let channel = state
            .storage
            .get_channel_by_id(93001)
            .await
            .expect("read channel after cursor")
            .expect("channel exists");
        assert_eq!(channel.unread_count, 5);

        let stale_channel = SyncEntityItem {
            entity_id: "93001".to_string(),
            version: 29,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 93001,
                "channel_type": 2,
                "channel_name": "stale-cursor-room",
                "unread_count": 99,
                "top": 1,
                "mute": 1
            })),
        };
        state
            .apply_sync_entities("channel", None, &[stale_channel], false)
            .await
            .expect("apply stale channel payload");

        let channel = state
            .storage
            .get_channel_by_id(93001)
            .await
            .expect("read channel after stale payload")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "cursor-room");
        assert_eq!(channel.unread_count, 5);
        assert_eq!(channel.top, 0);
        assert_eq!(channel.mute, 0);
        assert_eq!(channel.version, 30);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stale_channel_read_cursor_does_not_regress_local_unread_projection() {
        let (mut state, dir) = new_seeded_state("stale-read-cursor").await;

        let channel = SyncEntityItem {
            entity_id: "94001".to_string(),
            version: 40,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 94001,
                "channel_type": 2,
                "channel_name": "stale-cursor-room",
                "unread_count": 6,
                "top": 0,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[channel], false)
            .await
            .expect("apply channel baseline");

        for (server_message_id, pts) in [
            (72001_u64, 10_i64),
            (72002_u64, 20_i64),
            (72003_u64, 30_i64),
        ] {
            state
                .storage
                .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                    server_message_id,
                    local_message_id: 0,
                    channel_id: 94001,
                    channel_type: 2,
                    timestamp: 1_710_200_000_000 + pts,
                    from_uid: 20001,
                    message_type: 1,
                    content: format!("{{\"content\":\"m{pts}\"}}"),
                    status: 2,
                    pts,
                    setting: 0,
                    order_seq: pts,
                    searchable_word: format!("m{pts}"),
                    extra: "{}".to_string(),
                })
                .await
                .expect("seed message");
        }

        let fresh_cursor = SyncEntityItem {
            entity_id: "94001:10001".to_string(),
            version: 41,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 94001,
                "channel_type": 2,
                "reader_id": 10001,
                "last_read_pts": 20
            })),
        };
        state
            .apply_sync_entities("channel_read_cursor", None, &[fresh_cursor], false)
            .await
            .expect("apply fresh cursor");

        let channel = state
            .storage
            .get_channel_by_id(94001)
            .await
            .expect("read channel after fresh cursor")
            .expect("channel exists");
        assert_eq!(channel.unread_count, 4);

        let stale_cursor = SyncEntityItem {
            entity_id: "94001:10001".to_string(),
            version: 42,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 94001,
                "channel_type": 2,
                "reader_id": 10001,
                "last_read_pts": 10
            })),
        };
        state
            .apply_sync_entities("channel_read_cursor", None, &[stale_cursor], false)
            .await
            .expect("apply stale cursor");

        let channel = state
            .storage
            .get_channel_by_id(94001)
            .await
            .expect("read channel after stale cursor")
            .expect("channel exists");
        assert_eq!(channel.unread_count, 4);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn newer_channel_payload_keeps_materialized_local_unread_projection() {
        let (mut state, dir) = new_seeded_state("newer-channel-keeps-unread").await;

        let channel = SyncEntityItem {
            entity_id: "95001".to_string(),
            version: 50,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 95001,
                "channel_type": 2,
                "channel_name": "initial-room",
                "unread_count": 8,
                "top": 0,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[channel], false)
            .await
            .expect("apply initial channel");

        for (server_message_id, pts) in [
            (73001_u64, 10_i64),
            (73002_u64, 20_i64),
            (73003_u64, 30_i64),
        ] {
            state
                .storage
                .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                    server_message_id,
                    local_message_id: 0,
                    channel_id: 95001,
                    channel_type: 2,
                    timestamp: 1_710_300_000_000 + pts,
                    from_uid: 20001,
                    message_type: 1,
                    content: format!("{{\"content\":\"m{pts}\"}}"),
                    status: 2,
                    pts,
                    setting: 0,
                    order_seq: pts,
                    searchable_word: format!("m{pts}"),
                    extra: "{}".to_string(),
                })
                .await
                .expect("seed message");
        }

        let cursor = SyncEntityItem {
            entity_id: "95001:10001".to_string(),
            version: 51,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 95001,
                "channel_type": 2,
                "reader_id": 10001,
                "last_read_pts": 20
            })),
        };
        state
            .apply_sync_entities("channel_read_cursor", None, &[cursor], false)
            .await
            .expect("apply cursor");

        let newer_channel = SyncEntityItem {
            entity_id: "95001".to_string(),
            version: 52,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 95001,
                "channel_type": 2,
                "channel_name": "renamed-room",
                "unread_count": 99,
                "top": 1,
                "mute": 1
            })),
        };
        state
            .apply_sync_entities("channel", None, &[newer_channel], false)
            .await
            .expect("apply newer channel");

        let channel = state
            .storage
            .get_channel_by_id(95001)
            .await
            .expect("read channel")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "renamed-room");
        assert_eq!(channel.unread_count, 6);
        assert_eq!(channel.top, 1);
        assert_eq!(channel.mute, 1);
        assert_eq!(channel.version, 52);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_last_message_refresh_does_not_block_newer_channel_entity_state() {
        let (mut state, dir) = new_seeded_state("channel-entity-after-local-preview").await;

        let baseline = SyncEntityItem {
            entity_id: "96001".to_string(),
            version: 60,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 96001,
                "channel_type": 1,
                "channel_name": "alice",
                "unread_count": 0,
                "top": 0,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[baseline], false)
            .await
            .expect("apply baseline channel");

        state
            .update_channel_last_message(
                96001,
                1,
                "{\"content\":\"hello\"}",
                1_710_400_000_000,
                12345,
                Some(20001),
                true,
            )
            .await
            .expect("update local last message");

        let after_preview = state
            .storage
            .get_channel_by_id(96001)
            .await
            .expect("read channel after preview")
            .expect("channel exists");
        assert_eq!(after_preview.version, 60);

        let newer = SyncEntityItem {
            entity_id: "96001".to_string(),
            version: 61,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 96001,
                "channel_type": 1,
                "channel_name": "alice-renamed",
                "unread_count": 9,
                "top": 1,
                "mute": 1
            })),
        };
        state
            .apply_sync_entities("channel", None, &[newer], false)
            .await
            .expect("apply newer channel entity");

        let final_channel = state
            .storage
            .get_channel_by_id(96001)
            .await
            .expect("read final channel")
            .expect("channel exists");
        assert_eq!(final_channel.channel_name, "alice-renamed");
        assert_eq!(final_channel.top, 1);
        assert_eq!(final_channel.mute, 1);
        assert_eq!(final_channel.version, 61);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn channel_sync_zero_unread_heals_stale_local_unread_when_materialized_projection_is_zero(
    ) {
        let (mut state, dir) = new_seeded_state("channel-zero-unread-heal").await;

        let baseline = SyncEntityItem {
            entity_id: "96011".to_string(),
            version: 60,
            deleted: false,
            payload: Some(serde_json::json!({
                "channel_id": 96011,
                "channel_type": 1,
                "channel_name": "alice",
                "unread_count": 1,
                "top": 0,
                "mute": 0
            })),
        };
        state
            .apply_sync_entities("channel", None, &[baseline], false)
            .await
            .expect("apply baseline channel");

        state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 896011,
                local_message_id: 0,
                channel_id: 96011,
                channel_type: 1,
                timestamp: 1_710_500_000_010,
                from_uid: 20001,
                message_type: 1,
                content: "{\"content\":\"peer\"}".to_string(),
                status: 2,
                pts: 10,
                setting: 0,
                order_seq: 10,
                searchable_word: "peer".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed peer message");
        state
            .storage
            .upsert_remote_message_with_result(UpsertRemoteMessageInput {
                server_message_id: 896012,
                local_message_id: 0,
                channel_id: 96011,
                channel_type: 1,
                timestamp: 1_710_500_000_020,
                from_uid: 10001,
                message_type: 1,
                content: "{\"content\":\"self\"}".to_string(),
                status: 2,
                pts: 20,
                setting: 0,
                order_seq: 20,
                searchable_word: "self".to_string(),
                extra: "{}".to_string(),
            })
            .await
            .expect("seed self message");
        state
            .apply_sync_entities(
                "channel_read_cursor",
                None,
                &[SyncEntityItem {
                    entity_id: "96011:10001".to_string(),
                    version: 61,
                    deleted: false,
                    payload: Some(serde_json::json!({
                        "channel_id": 96011,
                        "channel_type": 1,
                        "reader_id": 10001,
                        "last_read_pts": 20
                    })),
                }],
                false,
            )
            .await
            .expect("apply read cursor");

        state
            .storage
            .upsert_channel(UpsertChannelInput {
                channel_id: 96011,
                channel_type: 1,
                channel_name: "alice".to_string(),
                channel_remark: String::new(),
                avatar: String::new(),
                unread_count: 1,
                top: 0,
                mute: 0,
                last_msg_timestamp: 1_710_500_000_020,
                last_local_message_id: 0,
                last_msg_content: "{\"content\":\"self\"}".to_string(),
                version: 61,
            })
            .await
            .expect("reinject stale local unread");

        state
            .apply_sync_entities(
                "channel",
                None,
                &[SyncEntityItem {
                    entity_id: "96011".to_string(),
                    version: 62,
                    deleted: false,
                    payload: Some(serde_json::json!({
                        "channel_id": 96011,
                        "channel_type": 1,
                        "channel_name": "alice",
                        "unread_count": 0,
                        "top": 0,
                        "mute": 0
                    })),
                }],
                false,
            )
            .await
            .expect("apply zero unread channel sync");

        let channel = state
            .storage
            .get_channel_by_id(96011)
            .await
            .expect("read healed channel")
            .expect("channel exists");
        assert_eq!(channel.unread_count, 0);
        assert_eq!(channel.version, 62);

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_channel_methods_return_synced_preview_baseline() {
        let (sdk, dir) = new_seeded_sdk("sdk-channel-preview-baseline").await;
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");

        store
            .upsert_channel(
                "10001",
                &UpsertChannelInput {
                    channel_id: 97001,
                    channel_type: 2,
                    channel_name: "sdk-preview-room".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 2,
                    top: 1,
                    mute: 0,
                    last_msg_timestamp: 1_710_500_000_000,
                    last_local_message_id: 0,
                    last_msg_content: "synced-preview-baseline".to_string(),
                    version: 70,
                },
            )
            .expect("seed synced channel");
        drop(store);

        let channel = sdk
            .get_channel_by_id(97001)
            .await
            .expect("sdk get channel")
            .expect("channel exists");
        assert_eq!(channel.channel_name, "sdk-preview-room");
        assert_eq!(channel.last_msg_timestamp, 1_710_500_000_000);
        assert_eq!(channel.last_msg_content, "synced-preview-baseline");
        assert_eq!(channel.last_local_message_id, 0);

        let channels = sdk.list_channels(20, 0).await.expect("sdk list channels");
        let listed = channels
            .into_iter()
            .find(|row| row.channel_id == 97001)
            .expect("listed channel exists");
        assert_eq!(listed.last_msg_timestamp, 1_710_500_000_000);
        assert_eq!(listed.last_msg_content, "synced-preview-baseline");

        let messages = sdk
            .list_messages(97001, 2, 20, 0)
            .await
            .expect("sdk list messages");
        assert!(messages.is_empty());

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_channel_methods_prefer_materialized_local_message() {
        let (sdk, dir) = new_seeded_sdk("sdk-channel-materialized-message").await;
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");

        store
            .upsert_channel(
                "10001",
                &UpsertChannelInput {
                    channel_id: 97002,
                    channel_type: 2,
                    channel_name: "sdk-materialized-room".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 1,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 1,
                    last_local_message_id: 0,
                    last_msg_content: "old-synced-preview".to_string(),
                    version: 71,
                },
            )
            .expect("seed channel");
        let inserted = store
            .upsert_remote_message_with_result(
                "10001",
                &UpsertRemoteMessageInput {
                    server_message_id: 89001,
                    local_message_id: 0,
                    channel_id: 97002,
                    channel_type: 2,
                    timestamp: 1_710_600_000_000,
                    from_uid: 20001,
                    message_type: 1,
                    content: "{\"content\":\"materialized-message\"}".to_string(),
                    status: 2,
                    pts: 33,
                    setting: 0,
                    order_seq: 33,
                    searchable_word: "materialized message".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("seed materialized message");
        drop(store);

        let messages = sdk
            .list_messages(97002, 2, 20, 0)
            .await
            .expect("sdk list messages");
        assert_eq!(messages.len(), 1);

        let channel = sdk
            .get_channel_by_id(97002)
            .await
            .expect("sdk get channel")
            .expect("channel exists");
        assert_eq!(channel.last_msg_timestamp, messages[0].created_at);
        assert_eq!(channel.last_msg_content, "[图片]");
        assert_eq!(channel.last_local_message_id, inserted.message_id);

        let channels = sdk.list_channels(20, 0).await.expect("sdk list channels");
        let listed = channels
            .into_iter()
            .find(|row| row.channel_id == 97002)
            .expect("listed channel exists");
        assert_eq!(listed.last_msg_timestamp, messages[0].created_at);
        assert_eq!(listed.last_msg_content, "[图片]");
        assert_eq!(listed.last_local_message_id, inserted.message_id);

        assert_eq!(messages[0].message_id, inserted.message_id);
        assert_eq!(
            messages[0].content,
            "{\"content\":\"materialized-message\"}"
        );

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_friend_methods_return_materialized_friend_view() {
        let (sdk, dir) = new_seeded_sdk("sdk-friend-view").await;
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");

        store
            .upsert_user(
                "10001",
                &UpsertUserInput {
                    user_id: 88001,
                    username: Some("alice".to_string()),
                    nickname: Some("Alice".to_string()),
                    alias: Some("A".to_string()),
                    avatar: "avatar://alice".to_string(),
                    user_type: 0,
                    is_deleted: false,
                    channel_id: "friend-88001".to_string(),
                    version: 101,
                    updated_at: 101,
                },
            )
            .expect("seed user");
        store
            .upsert_friend(
                "10001",
                &UpsertFriendInput {
                    user_id: 88001,
                    tags: Some("work".to_string()),
                    is_pinned: true,
                    created_at: 200,
                    version: 202,
                    updated_at: 202,
                },
            )
            .expect("seed friend");
        drop(store);

        let friends = sdk.list_friends(20, 0).await.expect("sdk list friends");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].user_id, 88001);
        assert_eq!(friends[0].username.as_deref(), Some("alice"));
        assert_eq!(friends[0].nickname.as_deref(), Some("Alice"));
        assert_eq!(friends[0].alias.as_deref(), Some("A"));
        assert_eq!(friends[0].tags.as_deref(), Some("work"));
        assert!(friends[0].is_pinned);
        assert_eq!(friends[0].version, 202);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_group_methods_return_materialized_group_view() {
        let (sdk, dir) = new_seeded_sdk("sdk-group-view").await;
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");

        store
            .upsert_group(
                "10001",
                &UpsertGroupInput {
                    group_id: 88002,
                    name: Some("group-a".to_string()),
                    avatar: "avatar://group-a".to_string(),
                    owner_id: Some(88001),
                    is_dismissed: false,
                    created_at: 300,
                    version: 303,
                    updated_at: 303,
                },
            )
            .expect("seed group");
        drop(store);

        let groups = sdk.list_groups(20, 0).await.expect("sdk list groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, 88002);
        assert_eq!(groups[0].name.as_deref(), Some("group-a"));
        assert_eq!(groups[0].owner_id, Some(88001));
        assert!(!groups[0].is_dismissed);
        assert_eq!(groups[0].version, 303);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_group_member_methods_return_materialized_member_view() {
        let (sdk, dir) = new_seeded_sdk("sdk-group-member-view").await;
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");

        store
            .upsert_group_member(
                "10001",
                &UpsertGroupMemberInput {
                    group_id: 88003,
                    user_id: 88001,
                    role: 1,
                    status: 0,
                    alias: Some("captain".to_string()),
                    is_muted: true,
                    joined_at: 400,
                    version: 404,
                    updated_at: 404,
                },
            )
            .expect("seed group member");
        drop(store);

        let members = sdk
            .list_group_members(88003, 20, 0)
            .await
            .expect("sdk list group members");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].group_id, 88003);
        assert_eq!(members[0].user_id, 88001);
        assert_eq!(members[0].role, 1);
        assert_eq!(members[0].status, 0);
        assert_eq!(members[0].alias.as_deref(), Some("captain"));
        assert!(members[0].is_muted);
        assert_eq!(members[0].version, 404);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_user_methods_return_materialized_user_views() {
        let (sdk, dir) = new_seeded_sdk("sdk-user-views").await;

        sdk.upsert_user(UpsertUserInput {
            user_id: 88101,
            username: Some("u-one".to_string()),
            nickname: Some("User One".to_string()),
            alias: Some("UNO".to_string()),
            avatar: "avatar://u-one".to_string(),
            user_type: 0,
            is_deleted: false,
            channel_id: "friend-88101".to_string(),
            version: 501,
            updated_at: 501,
        })
        .await
        .expect("upsert user one");
        sdk.upsert_user(UpsertUserInput {
            user_id: 88102,
            username: Some("u-two".to_string()),
            nickname: Some("User Two".to_string()),
            alias: None,
            avatar: "avatar://u-two".to_string(),
            user_type: 0,
            is_deleted: false,
            channel_id: "friend-88102".to_string(),
            version: 502,
            updated_at: 502,
        })
        .await
        .expect("upsert user two");

        let one = sdk
            .get_user_by_id(88101)
            .await
            .expect("get user by id")
            .expect("user one exists");
        assert_eq!(one.user_id, 88101);
        assert_eq!(one.username.as_deref(), Some("u-one"));
        assert_eq!(one.nickname.as_deref(), Some("User One"));
        assert_eq!(one.alias.as_deref(), Some("UNO"));
        assert_eq!(one.version, 501);

        let users = sdk
            .list_users_by_ids(vec![88102, 88101])
            .await
            .expect("list users by ids");
        assert_eq!(users.len(), 2);
        let first = users
            .iter()
            .find(|row| row.user_id == 88101)
            .expect("first user");
        let second = users
            .iter()
            .find(|row| row.user_id == 88102)
            .expect("second user");
        assert_eq!(first.nickname.as_deref(), Some("User One"));
        assert_eq!(second.nickname.as_deref(), Some("User Two"));

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_read_cursor_methods_return_projected_unread_views() {
        let dir = unique_test_dir("sdk-read-cursor-views");
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");
        let login = LoginResult {
            user_id: 10001,
            token: "token".to_string(),
            device_id: "device".to_string(),
            refresh_token: None,
            expires_at: 0,
        };
        store.save_login("10001", &login).expect("seed login");
        store
            .set_bootstrap_completed("10001", true)
            .expect("seed bootstrap completed");
        store
            .upsert_channel(
                "10001",
                &UpsertChannelInput {
                    channel_id: 97101,
                    channel_type: 2,
                    channel_name: "cursor-room".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 2,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 1,
                    last_local_message_id: 0,
                    last_msg_content: "cursor baseline".to_string(),
                    version: 601,
                },
            )
            .expect("seed channel");
        store
            .upsert_remote_message_with_result(
                "10001",
                &UpsertRemoteMessageInput {
                    server_message_id: 89101,
                    local_message_id: 0,
                    channel_id: 97101,
                    channel_type: 2,
                    timestamp: 1_710_700_000_000,
                    from_uid: 20001,
                    message_type: 1,
                    content: "{\"content\":\"cursor-message-1\"}".to_string(),
                    status: 2,
                    pts: 10,
                    setting: 0,
                    order_seq: 10,
                    searchable_word: "cursor message 1".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("seed message one");
        store
            .upsert_remote_message_with_result(
                "10001",
                &UpsertRemoteMessageInput {
                    server_message_id: 89102,
                    local_message_id: 0,
                    channel_id: 97101,
                    channel_type: 2,
                    timestamp: 1_710_700_100_000,
                    from_uid: 20002,
                    message_type: 1,
                    content: "{\"content\":\"cursor-message-2\"}".to_string(),
                    status: 2,
                    pts: 20,
                    setting: 0,
                    order_seq: 20,
                    searchable_word: "cursor message 2".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("seed message two");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("restore seeded current uid");

        assert_eq!(
            sdk.get_channel_unread_count(97101, 2)
                .await
                .expect("initial channel unread"),
            2
        );
        assert_eq!(
            sdk.get_total_unread_count(false)
                .await
                .expect("initial total unread"),
            2
        );

        sdk.project_channel_read_cursor(97101, 2, 10)
            .await
            .expect("project cursor");

        assert_eq!(
            sdk.get_channel_unread_count(97101, 2)
                .await
                .expect("projected channel unread"),
            1
        );
        assert_eq!(
            sdk.get_total_unread_count(false)
                .await
                .expect("projected total unread"),
            1
        );

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_message_extra_methods_return_materialized_revoke_view() {
        let dir = unique_test_dir("sdk-message-extra-view");
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");
        let login = LoginResult {
            user_id: 10001,
            token: "token".to_string(),
            device_id: "device".to_string(),
            refresh_token: None,
            expires_at: 0,
        };
        store.save_login("10001", &login).expect("seed login");
        store
            .set_bootstrap_completed("10001", true)
            .expect("seed bootstrap completed");
        store
            .upsert_channel(
                "10001",
                &UpsertChannelInput {
                    channel_id: 97102,
                    channel_type: 2,
                    channel_name: "extra-room".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 1,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 1,
                    last_local_message_id: 0,
                    last_msg_content: "extra baseline".to_string(),
                    version: 602,
                },
            )
            .expect("seed channel");
        let inserted = store
            .upsert_remote_message_with_result(
                "10001",
                &UpsertRemoteMessageInput {
                    server_message_id: 89201,
                    local_message_id: 0,
                    channel_id: 97102,
                    channel_type: 2,
                    timestamp: 1_710_701_000_000,
                    from_uid: 20001,
                    message_type: 1,
                    content: "{\"content\":\"revoke-target\"}".to_string(),
                    status: 2,
                    pts: 30,
                    setting: 0,
                    order_seq: 30,
                    searchable_word: "revoke target".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("seed message");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("restore seeded current uid");

        sdk.set_message_revoke(inserted.message_id, true, Some(30001))
            .await
            .expect("set revoke");

        let extra = sdk
            .get_message_extra(inserted.message_id)
            .await
            .expect("get message extra")
            .expect("message extra exists");
        assert!(extra.revoke);
        assert_eq!(extra.revoker, Some(30001));
        assert_eq!(extra.message_id, inserted.message_id);
        assert_eq!(extra.channel_id, 97102);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sdk_message_reaction_methods_return_materialized_reaction_view() {
        let dir = unique_test_dir("sdk-message-reaction-view");
        let store = LocalStore::open_at(dir.clone()).expect("open local store for seeding");
        let login = LoginResult {
            user_id: 10001,
            token: "token".to_string(),
            device_id: "device".to_string(),
            refresh_token: None,
            expires_at: 0,
        };
        store.save_login("10001", &login).expect("seed login");
        store
            .set_bootstrap_completed("10001", true)
            .expect("seed bootstrap completed");
        store
            .upsert_channel(
                "10001",
                &UpsertChannelInput {
                    channel_id: 97103,
                    channel_type: 2,
                    channel_name: "reaction-room".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 1,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 1,
                    last_local_message_id: 0,
                    last_msg_content: "reaction baseline".to_string(),
                    version: 603,
                },
            )
            .expect("seed channel");
        let inserted = store
            .upsert_remote_message_with_result(
                "10001",
                &UpsertRemoteMessageInput {
                    server_message_id: 89301,
                    local_message_id: 0,
                    channel_id: 97103,
                    channel_type: 2,
                    timestamp: 1_710_702_000_000,
                    from_uid: 20001,
                    message_type: 1,
                    content: "{\"content\":\"reaction-target\"}".to_string(),
                    status: 2,
                    pts: 40,
                    setting: 0,
                    order_seq: 40,
                    searchable_word: "reaction target".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("seed message");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("restore seeded current uid");

        sdk.upsert_message_reaction(UpsertMessageReactionInput {
            channel_id: 97103,
            channel_type: 2,
            uid: 10001,
            name: "thumbs-up".to_string(),
            emoji: "👍".to_string(),
            message_id: inserted.message_id,
            seq: 901,
            is_deleted: false,
            created_at: 1_710_702_100_000,
        })
        .await
        .expect("upsert reaction");

        let reactions = sdk
            .list_message_reactions(inserted.message_id, 20, 0)
            .await
            .expect("list message reactions");
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].message_id, inserted.message_id);
        assert_eq!(reactions[0].uid, 10001);
        assert_eq!(reactions[0].emoji, "👍");
        assert_eq!(reactions[0].name, "thumbs-up");
        assert!(!reactions[0].is_deleted);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
