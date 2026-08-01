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

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use msgtrans::{ClientEvent, TransportError};
// msgtrans 2.0 把 TransportOptions 拆成了 SendOptions / RequestOptions
// （上游 54da0b0）。这三处都是请求-响应，用 RequestOptions；构造器同名。
use msgtrans::RequestOptions;
use msgtrans::{QuicClientConfig, TcpClientConfig, WebSocketClientConfig};
use msgtrans::{TransportClient, TransportClientBuilder};
use privchat_protocol::message::LocalMessagePayloadEnvelope;
use privchat_protocol::presence::{
    PresenceBatchStatusRequest, PresenceBatchStatusResponse, PresenceChangedNotification,
    TypingActionType as ProtoTypingActionType, TypingIndicatorRequest,
};
use privchat_protocol::rpc::auth::{AuthLoginRequest, AuthResponse, UserRegisterRequest};
use privchat_protocol::rpc::contact::friend::FriendPendingResponse;
use privchat_protocol::rpc::file::upload::{
    FileGetUrlRequest, FileGetUrlResponse, FileRequestUploadTokenRequest,
    FileRequestUploadTokenResponse,
};
use privchat_protocol::rpc::message::history::{
    MessageHistoryAroundRequest, MessageHistoryAroundResponse, MessageHistoryGetRequest,
    MessageHistoryItem, MessageHistoryResponse,
};
use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::sync::{
    BatchGetChannelPtsRequest, BatchGetChannelPtsResponse, ChannelExtraSyncPayload,
    ChannelIdentifier, ChannelMemberSyncPayload, ChannelReadCursorSyncPayload, ChannelSyncPayload,
    FriendSyncPayload, GetChannelPtsRequest, GetChannelPtsResponse, GetDifferenceRequest,
    GetDifferenceResponse, GroupMemberSyncPayload, GroupSyncPayload, MessageStatusSyncPayload,
    MessageSyncPayload, ServerCommit, SyncEntityItem,
};
use privchat_protocol::MessagePayloadEnvelope;
use privchat_protocol::{
    decode_message, encode_message, AuthType, AuthorizationRequest, AuthorizationResponse,
    CanonicalTimelineEvent, ClientInfo, ContactCardMetadata, ContentMessageType, DeviceInfo,
    DeviceType, DisconnectRequest, DisconnectResponse, EntityInvalidationBatch, ErrorCode,
    FlatBufferMessage, LinkMetadata, LocationMetadata, MessageMetadata, MessageType, PingRequest,
    PongResponse, PublishRequest, PublishResponse, PushBatchRequest, PushBatchResponse,
    PushMessageRequest, PushMessageResponse, RpcRequest, RpcResponse, SendMessageRequest,
    SendMessageResponse, SubscribeRequest, SubscribeResponse, TransferRequest, TransferResponse,
    CANONICAL_TIMELINE_PUSH_TOPIC_V1, ENTITY_INVALIDATION_PUSH_TOPIC_V1,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::{interval, sleep, timeout, MissedTickBehavior};

/// 一次读取最多顺带修几条:repair 走网络,不能让打开会话变成一次批量拉取。
/// 没修完的下次读取继续。
const REPAIR_BATCH_LIMIT: usize = 5;
/// 队列上限。排不下的留给下一次读取重新发现——损坏是从数据本身发现的,不会丢。
const REPAIR_QUEUE_LIMIT: usize = 64;
/// 单条 repair 的超时。卡住的请求不该拖住整个 tick。
const REPAIR_TIMEOUT_MS: u64 = 15_000;
/// 退避基数与最大指数(2^6 × 2s ≈ 2 分钟封顶)。
const REPAIR_BACKOFF_BASE_MS: u64 = 2_000;
const REPAIR_BACKOFF_MAX_SHIFT: u32 = 6;

pub mod attachment_crypto;
mod avatar_cache;
pub mod canonical_inbound;
pub mod error_codes;
mod local_store;
pub mod media_download;
pub mod media_store;
mod receive_pipeline;
mod runtime;
mod storage_actor;
mod sync_commit_applier;
mod sync_coordinator;
mod task;
use receive_pipeline::ReceivePipeline;
use runtime::runtime_provider::RuntimeProvider;
use storage_actor::StorageHandle;
use sync_commit_applier::SyncCommitApplier;
use sync_coordinator::SyncCoordinator;
// Convergence 刻意不导出：它是 SDK 内部维度，不进公共 API / FFI ABI。
pub use sync_coordinator::{
    CriticalFailureCode, Readiness, SyncPhase, SyncRunKind, SyncStateSnapshot,
};
use task::task_registry::TaskRegistry;

/// 下载票据：下载前由 `file/get_url` 解析（file_id 路径），或由 legacy file_url 构造
/// （`encryption_version=0, cek=None`）。DownloadManager / run_download 只认这个，不关心来源。
#[derive(Debug, Clone)]
pub struct ResolvedFileDownload {
    pub url: String,
    pub encryption_version: i32,
    pub cek: Option<String>,
}

impl ResolvedFileDownload {
    /// 旧消息只有 file_url 时的 legacy 明文票据。
    pub fn legacy_url(url: String) -> Self {
        Self {
            url,
            encryption_version: 0,
            cek: None,
        }
    }
}

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

static QUIC_ACCEPT_SELF_SIGNED_FOR_TESTING: AtomicBool = AtomicBool::new(false);
static SKIP_INBOUND_MATERIALIZATION_FOR_LOAD_TESTING: AtomicBool = AtomicBool::new(false);
static CANONICAL_LEGACY_MISMATCH_COUNT: AtomicU64 = AtomicU64::new(0);
static CANONICAL_DECODE_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);

/// Allow QUIC clients to accept self-signed certificates.
///
/// This is intended for local development and load tests only. Production
/// clients should keep certificate verification enabled and configure a valid
/// CA/server name instead.
pub fn set_quic_accept_self_signed_for_testing(enabled: bool) {
    QUIC_ACCEPT_SELF_SIGNED_FOR_TESTING.store(enabled, Ordering::Release);
}

/// Disables local persistence of realtime timeline pushes for SDK instances
/// created after this call. This process-global switch exists only for the
/// server backpressure load gate, where hundreds of logical clients share one
/// machine and per-client SQLite writes would benchmark the generator instead
/// of server delivery. Transport receive and request ACK behavior are unchanged.
/// Production applications must never enable this mode.
pub fn set_skip_inbound_materialization_for_load_testing(enabled: bool) {
    SKIP_INBOUND_MATERIALIZATION_FOR_LOAD_TESTING.store(enabled, Ordering::SeqCst);
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

fn non_empty_trimmed(value: String) -> Option<String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn env_flag_enabled(key: &str) -> bool {
    matches!(
        env_var_trimmed(key)
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn quic_accept_self_signed_for_testing_enabled() -> bool {
    QUIC_ACCEPT_SELF_SIGNED_FOR_TESTING.load(Ordering::Acquire)
        || env_flag_enabled("PRIVCHAT_QUIC_ACCEPT_SELF_SIGNED")
        || env_flag_enabled("PRIVCHAT_QUIC_INSECURE_SKIP_VERIFY")
}

/// 一次性大声警告：QUIC 证书校验被跳过。**仅限本地开发 / 压测**，生产绝不可触发
/// （接受任意服务端证书 = MITM 风险）。在关闭校验的**唯一**入口打印，便于审计。
fn warn_quic_insecure_verification_disabled_once() {
    static WARN_ONCE: std::sync::Once = std::sync::Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  [SECURITY] QUIC certificate verification is DISABLED via testing flag \
             (set_quic_accept_self_signed_for_testing / PRIVCHAT_QUIC_ACCEPT_SELF_SIGNED / \
             PRIVCHAT_QUIC_INSECURE_SKIP_VERIFY). LOCAL DEV / LOAD TEST ONLY — MUST NEVER be set \
             in production (accepts any server cert; MITM risk)."
        );
        tracing::warn!(
            "QUIC certificate verification DISABLED (testing-only self-signed accept); never enable in production"
        );
    });
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

/// Channel Transfer client-side response (decoded from wire `TransferResponse`).
/// See `02-server/CHANNEL_TRANSFER_SPEC.md` and `07-application/BOT_INTERACTION_SPEC.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferReply {
    pub request_id: String,
    pub channel_id: u64,
    pub code: i32,
    pub message: String,
    pub data: Vec<u8>,
}

/// 会话状态快照：精确阶段 + 它属于哪个账号的哪一次会话。
///
/// 三个字段必须一起读：宿主拿到阶段后再去问「当前是谁」是不安全的，两次读之间
/// 账号可能已经换过，而同一个 client 会原地切号（`switch_local_account`），
/// 所以 client 身份并不等于账号身份。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionStatus {
    pub state: ConnectionState,
    /// 当前账号 uid。未登录为 `None`。
    pub account_uid: Option<String>,
    /// 见 actor state 上的 `session_epoch` 注释：只有显式建立/废弃会话才自增。
    pub session_epoch: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConnectionState {
    New,
    Connected,
    LoggedIn,
    Authenticated,
    /// 服务端判定本次登录态不可自愈（token 过期/撤销/设备不匹配等）。
    /// SDK 已断开 transport 并禁用自动重连；UI 必须清本地 token 重新登录
    /// 才能回到 New。本字段从 `ForcedLogout` 事件派生，debug/metrics 可直接读。
    Terminated,
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
pub enum MediaDownloadState {
    Idle,
    Downloading { bytes: u64, total: Option<u64> },
    Paused { bytes: u64, total: Option<u64> },
    Done { path: String },
    Failed { code: u32, message: String },
}

/// `SdkEvent::ForcedLogout` 触发来源。
/// 前端可据此决定提示语 / 是否清理本地登录态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ForcedLogoutSource {
    /// CONNECT 阶段服务端判定 Terminal（token 过期、设备被踢等）。
    ConnectAuth,
    /// 认证后 RPC 调用侧判定 Terminal（e.g. 10000-段 auth-required）。
    RpcAuth,
    /// 手动 `authenticate()` 调用拿到 Terminal 错——当前不走自动重连路径，
    /// 但语义一致，统一走这里给 UI 一个出口。
    Manual,
}

/// 最近一次 ForcedLogout 的原因快照，供 debug / metrics / 冷启动诊断使用。
/// `connect()` 成功后清空；同一次生命周期内只保留最后一次。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalReason {
    /// `privchat_protocol::ErrorCode` 对应的 u32 码；未携带时为 0。
    pub code: u32,
    pub message: String,
    pub source: ForcedLogoutSource,
    /// UTC 毫秒时间戳。
    pub at_ms: i64,
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
    SyncStateChanged {
        state: SyncStateSnapshot,
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
    PeerReadPtsAdvanced {
        channel_id: u64,
        channel_type: i32,
        reader_id: u64,
        read_pts: u64,
    },
    MessageDelivered {
        channel_id: u64,
        channel_type: i32,
        /// Local SQLite primary key used by client state and UI identity.
        message_id: u64,
        /// Server-assigned network identity retained for diagnostics and sync.
        server_message_id: u64,
        delivered_at: u64,
    },
    MediaDownloadStateChanged {
        message_id: u64,
        state: MediaDownloadState,
    },
    /// Plan 2 异步媒体作业请求。Rust 发起后挂起 oneshot，等待宿主通过
    /// `PrivchatSdk::submit_media_job_result(job_id, result)` 回传。
    /// 超时（`timeout_ms`）内未回传，走 `thumb_status=3` 兜底。
    /// 当前支持 `job_kind = "video_thumbnail"`：宿主从 `source_path`
    /// 抽取首帧写入 `output_path`（JPEG），SDK 再转 WebP。
    MediaJobRequested {
        job_id: String,
        job_kind: String,
        source_path: String,
        output_path: String,
        mime_type: String,
        message_id: u64,
        timeout_ms: u64,
    },
    /// Access token 已由 SDK 自动续期成功（Phase B1）。
    ///
    /// 不携带 token 内容——token 是敏感凭证，不通过 broadcast 事件扩散。
    /// 如果宿主需要读取新 access_token（例如拿去调其它业务 HTTP），
    /// 请调用 `PrivchatSdk::get_current_access_token()` 主动拉取当前权威值。
    ///
    /// 宿主**可以**忽略此事件——SDK 内部已原子替换 token 并继续连接，
    /// 订阅仅用于调试 / metrics / 刷新 UI 上的"会话有效期"显示。
    TokenRefreshed {
        /// 新 access_token 的过期时间（Unix 毫秒，服务端下发）。
        expires_at: u64,
    },
    /// auto-reconnect 握手撞到 Recoverable auth 错（典型 10002 AccessTokenExpired），
    /// SDK 已暂停 auto-reconnect 并保留 transport，等业务层走 refresh + authenticate。
    ///
    /// **业务层契约**（详见 [`TOKEN_REFRESH_SPEC`](../../privchat-docs/spec/03-protocol-sdk/TOKEN_REFRESH_SPEC.md) §3.1）：
    /// 收到事件后调用自家 mode-aware refresh 入口（privchat-app `recoverFromTokenExpired`）：
    /// 模式 A 走 `sdk.refreshAccessToken` → `sdk.authenticate`；
    /// 模式 B/C 走业务后台 refresh endpoint → `sdk.authenticate`。
    ///
    /// SDK **不**自调任何后台、**不**持 refresh_token、**不**改 ConnectionState、**不**断 transport。
    ///
    /// 幂等性：每次 retry 撞 Recoverable 都发一次。业务层 `authenticate` 成功后 `should_auto_reconnect`
    /// 重置为 true，下一轮 token 过期再触发一次。
    AccessTokenRefreshNeeded {
        /// 服务端原始错误码，典型 10002（AccessTokenExpired）。
        code: u32,
        /// 服务端原始 message；仅作日志/审计，业务层不应解析 message 走分支。
        message: String,
    },
    /// 服务端/协议栈判定本次登录态不可自愈（token 过期/撤销、设备不匹配等）。
    /// SDK 已停止自动重连并断开当前 session，宿主收到该事件后应：
    /// 1) 清理本地登录态（token / user_id / device_id）；
    /// 2) 跳回登录页，避免冷启动继续用过期 token。
    /// 同一次生命周期内 SDK 保证只发一次，由 `State::auth_terminal_fired` 幂等闸门保证。
    ForcedLogout {
        /// `privchat_protocol::ErrorCode` 对应的 u32 码；未携带时为 0。
        code: u32,
        message: String,
        source: ForcedLogoutSource,
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

/// Plan 2 媒体作业结果。Kotlin/iOS 宿主完成工作后，通过
/// [`PrivchatSdk::submit_media_job_result`] 回传。`ok=true` 时 `output_path`
/// 必须指向已写入的文件（由发布方保证路径存在）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaJobResult {
    pub ok: bool,
    pub output_path: Option<String>,
    pub error: Option<String>,
}

/// 网址预览抓取结果（应用层实现）：URL → 标题 / 描述 / 本地缩略图文件路径。
/// 任一字段可缺省；缩略图字段若存在，SDK 随后会将该本地文件作为普通图片上传得到 `file_id`。
#[derive(Debug, Clone, Default)]
pub struct LinkPreviewResult {
    pub title: Option<String>,
    pub description: Option<String>,
    /// 本地缩略图路径；SDK 负责后续上传并填入 `LinkMetadata.thumbnail_file_id`。
    pub thumbnail_path: Option<std::path::PathBuf>,
}

/// 网址预览回调（应用层实现）。类比 [`VideoProcessHook`]：SDK 传入 URL，由宿主 App 抓取
/// 网页 meta 和 og:image，写入本地临时路径后返回结果；宿主未注册时 SDK 不会发起抓取，
/// `LinkMetadata` 各可选字段保持 `None`（客户端 UI 兜底空白预览）。
pub type LinkPreviewHook =
    Arc<dyn Fn(&str) -> std::result::Result<LinkPreviewResult, String> + Send + Sync>;

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
        if realtime_trace_enabled() {
            eprintln!("[SDK_INBOUND_TASK_END] aborting old inbound task");
        }
        handle.abort();
        let _ = handle.await;
    }
}

/// 把一次 Terminal 认证错误收敛成一次 ForcedLogout 事件。
///
/// 调用方必须把 `err` 判定为 `is_auth_terminal()` 后再进来；
/// 本函数只负责 side-effect 编排，不做分类。
///
/// 严格顺序：
/// 1. 幂等闸门 (`auth_terminal_fired`) —— 只触发一次；
/// 2. 停掉 inbound 任务并 bump `inbound_epoch`，丢弃 mpsc 里任何遗留帧；
/// 3. 断开 transport（失败不阻塞流程，继续清状态）；
/// 4. 切 `session_state = Terminated` / 关自动重连 / 清 next_reconnect_at / 重置 backoff；
/// 5. 记录 `last_terminal_reason`，方便 debug / 冷启动诊断；
/// 6. emit `SdkEvent::ForcedLogout`——发给 UI 的唯一出口事件。
///
/// 我们不在这里再发 `ConnectionStateChanged { to: Terminated }`：
/// ForcedLogout 本身就是比 Disconnected 更强的 UI 信号；Terminated 状态可通过
/// `connection_state()` 随时读到，避免 UI 写两套跳转逻辑。
async fn trigger_forced_logout(
    state: &mut State,
    inbound_task: &mut Option<tokio::task::JoinHandle<()>>,
    actor_event_tx: &broadcast::Sender<SdkEvent>,
    actor_event_history: &StdMutex<VecDeque<SequencedSdkEvent>>,
    actor_event_seq: &AtomicU64,
    event_history_limit: usize,
    err: &Error,
    source: ForcedLogoutSource,
) {
    if state.auth_terminal_fired {
        return;
    }
    state.auth_terminal_fired = true;

    let code = err.auth_error_code().unwrap_or(0);
    let message = match err {
        Error::Auth(msg) => msg.clone(),
        _ => err.to_string(),
    };

    eprintln!(
        "[SDK.actor] forced_logout source={:?} code={} msg={}",
        source, code, message
    );

    stop_inbound_task(inbound_task).await;
    // 即便 abort 了任务，mpsc 里可能还有已发送的 InboundFrame；bump epoch 让
    // actor loop 下个 tick 比对时丢弃。
    state.inbound_epoch = state.inbound_epoch.wrapping_add(1);

    if let Err(e) = state.disconnect().await {
        eprintln!("[SDK.actor] forced_logout disconnect failed: {e}");
    }

    state.session_state = SessionState::Terminated;
    state.should_auto_reconnect = false;
    state.next_reconnect_at = None;
    state.reset_reconnect_backoff();
    // 强制登出（token terminal / 被踢）→ 清订阅注册表，避免跨账号 replay 泄漏。
    state.active_subscriptions.clear();

    state.last_terminal_reason = Some(TerminalReason {
        code,
        message: message.clone(),
        source,
        at_ms: chrono::Utc::now().timestamp_millis(),
    });

    emit_sequenced_event(
        actor_event_tx,
        actor_event_history,
        actor_event_seq,
        event_history_limit,
        SdkEvent::ForcedLogout {
            code,
            message,
            source,
        },
    );
}

fn inbound_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_INBOUND_LOG").ok().as_deref() == Some("1"))
}

/// RC: 实时链路诊断日志（重连 / inbound task / push 帧 / 本地落库）的统一开关。
/// 默认 **关闭** —— release / TestFlight 必须安静。设 `PRIVCHAT_TRACE_REALTIME=1`
/// （或复用 `PRIVCHAT_INBOUND_LOG=1`）开启。绝不打印 token / cek / message body。
fn realtime_trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("PRIVCHAT_TRACE_REALTIME").ok().as_deref() == Some("1")
            || std::env::var("PRIVCHAT_INBOUND_LOG").ok().as_deref() == Some("1")
    })
}

fn rpc_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_RPC_LOG").ok().as_deref() == Some("1"))
}

fn actor_logs_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PRIVCHAT_ACTOR_LOG").ok().as_deref() == Some("1"))
}

/// CODEX-8：雪花 worker 位（machine_id/data_center_id 各 5 bit）取自持久化 installation id
/// （`<data_dir>/installation_id`），替代「pid + 启动毫秒」的临时派生 —— 重启/升级后 worker 位
/// 稳定不漂移。配合服务端 `(sender, device, local_message_id)` 幂等命名空间，跨设备/跨用户碰撞
/// 不再互相判重；单设备内唯一性由雪花毫秒+序列保证。`data_dir` 为空时退回旧的 pid/时间派生。
///
/// 复审要点：
///   - **固定哈希**：用 FNV-1a（算法恒定，跨 Rust 版本稳定）而非 `DefaultHasher`（标准库不保证
///     其算法跨版本稳定，不能作持久派生契约）。
///   - **原子发布**：写完整临时文件后 `hard_link` 发布（no-clobber，见 [read_or_create_installation_id]）。
fn stable_snowflake_worker_bits(data_dir: &str) -> (u16, u16) {
    fn fnv1a_64(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    let installation_id: Option<String> = if data_dir.trim().is_empty() {
        None
    } else {
        let dir = std::path::Path::new(data_dir);
        let path = dir.join("installation_id");
        let _ = std::fs::create_dir_all(dir);
        match read_or_create_installation_id(dir, &path) {
            Ok(id) => Some(id),
            // 持久化失败→降级 ephemeral（重启后 worker 可能变化）必须**对外可见留痕**，不能静默吞掉
            // （对齐 Codex 复审 P2：至少 warning/metric）。注意影响面：服务端 dedup 已按 device 隔离，
            // 故受影响的是**同一 device 命名空间内本地雪花 ID 的稳定性/碰撞风险**，而非跨设备幂等本身。
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    data_dir = %data_dir,
                    "installation_id 持久化失败，降级为非稳定 ephemeral 雪花 worker 位（重启后 worker 可能\
                     变化：同一 device 命名空间内本地雪花 ID 稳定性下降、碰撞风险上升；服务端按 device 隔离\
                     的 dedup 不受影响）"
                );
                None
            }
        }
    };
    match installation_id {
        Some(id) => {
            let v = fnv1a_64(id.as_bytes());
            ((((v >> 5) & 0x1f) as u16), ((v & 0x1f) as u16))
        }
        None => (
            (std::process::id() as u16) & 0x1f,
            (chrono::Utc::now().timestamp_millis() as u16) & 0x1f,
        ),
    }
}

/// fsync 目录，使其中的目录项变更（新 hard link / 删除）挺过**掉电/系统崩溃**（而不只是进程崩溃）。
/// POSIX 上把目录作为 `File` 打开再 `sync_all`。返回 `Err` = 掉电持久性**未确认**（如无法把目录当
/// File 打开的平台，或 fsync 失败）—— 调用方应告警但**不应据此另换 id**（final path 已发布可用）。
fn sync_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// 获取/创建持久 installation id（派生稳定雪花 worker 位）。**并发 + 崩溃 + 掉电安全**：
/// 1. 把完整随机 id 写入**唯一命名**的临时文件并 `sync_all`（fsync）；
/// 2. 用 `hard_link(tmp, path)` **原子发布** —— 目标已存在时返回 `AlreadyExists`（no-clobber），
///    故**恰好一个写者胜出**，且已发布文件**永不会被观察到半写/空**（读者只读到完整 id）——
///    连崩溃也只会残留一个孤儿临时文件，绝不产生空的 `path`（临时文件先写满再 link）；
/// 3. 发布后 `sync_dir` **fsync 父目录**，让 `path` 的目录项挺过掉电（POSIX；见 [sync_dir]）。
///
/// 失败语义（对齐 Codex 复审 P1）：写入或 `sync_all` 失败 → 删除临时文件并**返回 Err**（不谎报成功、
/// 不残留脏文件）。损坏 `path`（**空**文件，或**非 UTF-8**/不可解析内容 —— 新实现永不发布这类文件）
/// 一律**隔离回收后重建**，否则每次启动都会因读失败而降级到 ephemeral worker。
fn read_or_create_installation_id(
    dir: &std::path::Path,
    path: &std::path::Path,
) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind, Write};
    for _ in 0..64 {
        // 1) 快路径：读回已发布 id（因 hard_link 发布，绝不空/半写）。
        match std::fs::read_to_string(path) {
            Ok(s) => {
                let t = s.trim();
                if !t.is_empty() {
                    return Ok(t.to_string());
                }
                // 空 path = 旧实现/外部篡改留下的损坏占位（新实现永不发布空文件）→ 回收后重建。
                let _ = std::fs::remove_file(path);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            // 非 UTF-8 / 不可解析内容 = 损坏文件 → 隔离重建（否则每次启动都读失败降级 ephemeral）。
            Err(e) if e.kind() == ErrorKind::InvalidData => {
                let _ = std::fs::remove_file(path);
            }
            // 权限 / IO 错误 → 无法恢复，上抛（由调用方降级并告警）。
            Err(e) => return Err(e),
        }
        // 2) 发布尝试：真随机 id（rand，非 RandomState 冒充）→ 写满临时文件 fsync → 原子 hard_link。
        let candidate = format!("{:032x}", rand::random::<u128>());
        let tmp = dir.join(format!(
            "installation_id.tmp.{:016x}",
            rand::random::<u64>()
        ));
        if let Err(e) = (|| -> std::io::Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(candidate.as_bytes())?;
            f.sync_all()?;
            Ok(())
        })() {
            let _ = std::fs::remove_file(&tmp); // 失败不残留半写临时文件
            return Err(e); // 失败上抛，绝不谎报成功
        }
        match std::fs::hard_link(&tmp, path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                // 掉电持久：flush 父目录（新 hard link + tmp 移除）。失败 = 持久性未确认，但 id 已发布
                // 可用 —— 告警不另换 id（另换会引入不稳定的第二个 id）。
                if let Err(e) = sync_dir(dir) {
                    tracing::warn!(
                        error = %e,
                        dir = %dir.display(),
                        "installation_id durability_not_confirmed：父目录 fsync 失败，进程崩溃安全但掉电后目录项可能丢失"
                    );
                }
                return Ok(candidate);
            }
            // 已有写者胜出 → 清理临时文件，下一轮读回其（完整）id。
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
    }
    Err(Error::new(ErrorKind::Other, "installation id 初始化未收敛"))
}

#[cfg(test)]
mod payload_fallback_tests {
    // 生产乱码事故回归（web→app 前导符号）：FlatBuffers/未知二进制 payload 在 FB+JSON 都解不了时，
    // 绝不能 lossy-stringify 当 content 渲染（会显示不可见控制符+长度字节+正文）。对齐 TS
    // decodePlainTextPayload 的二进制守卫：解不了的二进制 → 空 content（等 resync/升级恢复）。
    use super::State;

    #[test]
    fn plain_text_passthrough() {
        let text = "第一行\n\t第二行";
        let (content, extra) = State::payload_bytes_to_message_content_and_extra(text.as_bytes());
        assert_eq!(content, text);
        assert!(extra.is_none());
    }

    #[test]
    fn fb_envelope_decodes_content() {
        let bytes = privchat_protocol::encode_message(&privchat_protocol::MessagePayloadEnvelope {
            content: "hi".to_string(),
            ..Default::default()
        })
        .expect("encode");
        let (content, _extra) = State::payload_bytes_to_message_content_and_extra(&bytes);
        assert_eq!(content, "hi");
    }

    #[test]
    fn undecodable_binary_yields_empty_not_mojibake() {
        let mut bytes =
            privchat_protocol::encode_message(&privchat_protocol::MessagePayloadEnvelope {
                content: "大家记住一句话，天下没有白吃的苦，没有白走的路！".to_string(),
                ..Default::default()
            })
            .expect("encode");
        bytes[0] = 0xF0;
        bytes[1] = 0xFF;
        bytes[2] = 0xFF;
        bytes[3] = 0xFF; // 损坏 root offset → decode 失败
        let (content, extra) = State::payload_bytes_to_message_content_and_extra(&bytes);
        assert_eq!(
            content, "",
            "binary garbage must not be rendered as mojibake content"
        );
        assert!(extra.is_none());
    }
}

#[cfg(test)]
mod snowflake_worker_bits_tests {
    use super::read_or_create_installation_id;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "pc_sdk_instid_{}_{}_{}",
            tag,
            std::process::id(),
            chrono::Utc::now().timestamp_micros()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn stable_across_reinit_and_persisted() {
        let dir = temp_dir("stable");
        let dir_s = dir.to_string_lossy().to_string();
        let first = super::stable_snowflake_worker_bits(&dir_s);
        assert!(dir.join("installation_id").exists()); // 已持久化
        let second = super::stable_snowflake_worker_bits(&dir_s); // 重入（模拟重启）
        assert_eq!(first, second, "worker bits must be stable across reinit");
        assert!(first.0 <= 0x1f && first.1 <= 0x1f); // 各 5 bit
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_init_agrees_on_one_installation_id() {
        // N 线程并发初始化同一空目录 → hard_link 发布保证恰好一个 id 落地；所有线程必须取得**同一
        // installation ID 字符串**（Codex 复审#3：直接断言 id，不能只比 10-bit worker bits）。
        let dir = temp_dir("conc");
        let path = dir.join("installation_id");
        let n = 32;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let (d, p, b) = (dir.clone(), path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    read_or_create_installation_id(&d, &p).unwrap()
                })
            })
            .collect();
        let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let winner = std::fs::read_to_string(&path).unwrap().trim().to_string();
        assert!(!winner.is_empty());
        assert!(
            ids.iter().all(|id| *id == winner),
            "installation ids diverged: winner={winner} got={ids:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovers_from_empty_installation_id_file() {
        // 崩溃/旧实现遗留的**空** installation_id → 必须回收重建为稳定非空 id（不能永久回退临时 id）。
        let dir = temp_dir("empty");
        let path = dir.join("installation_id");
        std::fs::write(&path, "").unwrap(); // 损坏占位
        let id1 = read_or_create_installation_id(&dir, &path).unwrap();
        assert!(
            !id1.is_empty(),
            "empty file must be recovered to a non-empty id"
        );
        let id2 = read_or_create_installation_id(&dir, &path).unwrap(); // 重入稳定
        assert_eq!(id1, id2, "recovered id must be stable across reinit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovers_from_non_utf8_installation_id_file() {
        // 非 UTF-8 损坏内容 → read_to_string 得 InvalidData → 必须隔离重建，而非每次启动降级 ephemeral。
        let dir = temp_dir("utf8");
        let path = dir.join("installation_id");
        std::fs::write(&path, [0xffu8, 0xfe, 0x00, 0x99]).unwrap(); // 非法 UTF-8
        let id1 = read_or_create_installation_id(&dir, &path).unwrap();
        assert!(
            !id1.is_empty(),
            "corrupt non-utf8 file must be rebuilt to a valid id"
        );
        let id2 = read_or_create_installation_id(&dir, &path).unwrap(); // 重入稳定
        assert_eq!(id1, id2, "rebuilt id must be stable across reinit");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_failure_returns_err_without_dirty_file() {
        // **结构性**持久化失败：父路径组件是普通文件 → 任何创建都 ENOTDIR，**确定性失败且不依赖运行
        // 用户权限**（root/额外 capability 亦失败；对齐 Codex 复审#4 P2）。必须返回 Err，不发布 id。
        let base = temp_dir("nopersist");
        let blocker = base.join("blocker");
        std::fs::write(&blocker, b"x").unwrap(); // 普通文件（非目录）
        let dir = blocker.join("inner"); // dir 的父组件是文件 → 其下任何 open/create 都 ENOTDIR
        let path = dir.join("installation_id");
        let r = read_or_create_installation_id(&dir, &path);
        assert!(
            r.is_err(),
            "structural persist failure must surface as Err, not faked success"
        );
        assert!(
            !path.exists(),
            "must not publish an installation_id on failure"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// 服务端 unauth 白名单路由：自带凭证校验（refresh_token JWT / 密码 / 二维码场景 token），
/// 不依赖 access_token 已认证的 IM session。SDK 在 Connected 状态下应允许这些路由。
///
/// 与 server `auth/whitelist.rs` 的 unauth allowlist 对齐；新增 unauth 路由必须在此同步登记。
fn is_unauth_rpc_route(route: &str) -> bool {
    use privchat_protocol::rpc::routes;
    matches!(
        route,
        routes::auth::LOGIN
            | routes::auth::REFRESH
            | routes::account_user::REGISTER
            | routes::qr_login::CREATE_SCENE
    )
}

async fn start_inbound_task(
    state: &mut State,
    actor_tx: mpsc::Sender<Command>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    stop_inbound_task(task).await;
    // 新 inbound 任务 = 新 epoch。任何 stop_inbound_task 之后遗留在 mpsc 队列里的
    // 旧帧都会被 actor loop 按 epoch 比对丢弃。
    state.inbound_epoch = state.inbound_epoch.wrapping_add(1);
    let epoch = state.inbound_epoch;
    let Some(_transport) = state.transport.as_ref() else {
        if realtime_trace_enabled() {
            eprintln!(
                "[SDK_INBOUND_TASK_START] epoch={} ABORTED transport=None uid={:?}",
                epoch, state.current_uid
            );
        }
        return;
    };
    if realtime_trace_enabled() {
        eprintln!(
            "[SDK_INBOUND_TASK_START] epoch={} uid={:?} (订阅新 transport events)",
            epoch, state.current_uid
        );
    }
    let events_slot = state.transport_events.clone();
    let current_uid_for_log = state.current_uid.clone();
    *task = Some(tokio::spawn(async move {
        // 借用事件流。上一个 inbound task 已被 stop_inbound_task abort + await，
        // 所以这里不会真的争锁。
        let mut slot = events_slot.lock().await;
        let Some(event_rx) = slot.as_mut() else {
            eprintln!(
                "[SDK_INBOUND_TASK_START] epoch={} ABORTED events=None uid={:?}",
                epoch, current_uid_for_log
            );
            return;
        };
        loop {
            let event = match event_rx.next().await {
                Some(event) => event,
                None => {
                    eprintln!("[SDK.inbound] event stream closed");
                    break;
                }
            };
            // msgtrans 2.0 拆分了客户端事件：`Message` 是单向数据（无回复义务），
            // `Request` 是消费式请求（必须且只能应答一次）。两者都归一到
            // (biz_type, payload) 后走同一条 InboundFrame 入队路径。
            let (biz_type, data): (u8, bytes::Bytes) = match event {
                ClientEvent::Message(msg) => {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] message received biz_type={} len={}",
                            msg.biz_type(),
                            msg.payload().len()
                        );
                    }
                    (msg.biz_type(), msg.into_payload())
                }
                ClientEvent::Request(req) => {
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] request received biz_type={} len={}",
                            req.biz_type(),
                            req.payload().len()
                        );
                    }
                    let biz_type = req.biz_type();
                    let payload = req.payload().clone();
                    // 对 Request 类型的包回复传输层 ACK（PushMessageResponse）。
                    // 用消费式 `respond_detached` 保持原有 fire-and-forget 语义，
                    // 不阻塞 inbound 循环。
                    let ack = PushMessageResponse {
                        succeed: true,
                        message: None,
                    };
                    if let Ok(ack_bytes) = encode_message(&ack) {
                        req.respond_detached(ack_bytes);
                    }
                    (biz_type, payload)
                }
                ClientEvent::Disconnected { .. } => {
                    eprintln!("[SDK.inbound] transport disconnected");
                    let _ = actor_tx.send(Command::InboundDisconnected { epoch }).await;
                    break;
                }
                _ => continue,
            };
            if SKIP_INBOUND_MATERIALIZATION_FOR_LOAD_TESTING.load(Ordering::Relaxed)
                && matches!(
                    MessageType::from(biz_type),
                    MessageType::SendMessageRequest
                        | MessageType::PushMessageRequest
                        | MessageType::PushBatchRequest
                        | MessageType::PublishRequest
                )
            {
                // The transport request has already been acknowledged.
                // Do not enqueue server-timeline traffic behind load-generator
                // RPC commands; doing so would benchmark one host's synthetic
                // SDK actors rather than server slow-consumer behavior.
                continue;
            }
            if actor_tx
                .send(Command::InboundFrame {
                    epoch,
                    biz_type,
                    // payload 为 Bytes(零拷贝);actor 命令面仍是 Vec<u8>,此处物化一次。
                    data: data.to_vec(),
                })
                .await
                .is_err()
            {
                break;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewMessage {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub searchable_word: String,
    pub setting: i32,
    pub extra: String,
    /// 媒体 MIME 类型，纯文本消息为 None
    pub mime_type: Option<String>,
    /// 主附件文件是否已在本地就绪（发送时为 true）
    pub media_downloaded: bool,
    /// 缩略图状态：0=missing, 1=ready, 2=failed
    pub thumb_status: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredSendOptions {
    pub in_reply_to_message_id: Option<u64>,
    pub mentioned_user_ids: Vec<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinkMessageInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub thumbnail_file_id: Option<u64>,
    pub options: StructuredSendOptions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocationMessageInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub latitude: f64,
    pub longitude: f64,
    pub coordinate_system: Option<String>,
    pub name: Option<String>,
    pub address: Option<String>,
    pub poi_id: Option<String>,
    pub poi_source: Option<String>,
    pub thumbnail_file_id: Option<u64>,
    pub options: StructuredSendOptions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContactCardMessageInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub user_id: u64,
    pub options: StructuredSendOptions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpsertRemoteMessageInput {
    pub server_message_id: u64,
    pub local_message_id: u64,
    pub channel_id: u64,
    pub channel_type: i32,
    pub timestamp: i64,
    /// `timestamp` 原本的精度。**必须由来源适配器给**，不能靠数值量级反推:
    /// adapter 已经把秒乘成毫秒了,到这里再看量级只会一律判成毫秒,精度信息就丢了
    /// ——那正是「history 的 .317 被后到的 push 改成 .000」能复活的路径。
    pub timestamp_precision: crate::canonical_inbound::TimePrecision,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    pub status: i32,
    pub pts: i64,
    pub setting: i32,
    pub order_seq: i64,
    pub searchable_word: String,
    pub extra: String,
    /// 媒体 MIME 类型（从 content/extra 中提取）
    pub mime_type: Option<String>,
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
    /// 媒体 MIME 类型（如 image/jpeg），纯文本为 None
    pub mime_type: Option<String>,
    /// 主附件文件是否已下载到本地 canonical 目录
    pub media_downloaded: bool,
    /// 缩略图状态：0=missing, 1=ready, 2=failed
    pub thumb_status: i32,
    /// 是否已送达对端（from message_extra.delivered）
    pub delivered: bool,
    /// per-channel 消息序号（用于 read cursor 投影: pts <= peer_read_pts）
    pub pts: Option<u64>,
}

/// 出站队列可否排空的**纯判据**。
///
/// 刻意做成自由函数并且只接收这三个事实：签名里**没有 `NetworkHint`**，所以「系统可达性」
/// 在类型层面就无法再混进这个决策（2026-07-26 生产事故的结构性防回归）。
/// 可达性只允许影响探测/重试的频率，不能决定「做不做事」。

/// 附件类消息（其发送必须经过 file queue 的上传管线）。
pub(crate) fn is_attachment_message_type(message_type: i32) -> bool {
    matches!(
        message_type,
        t if t == privchat_protocol::ContentMessageType::Image as i32
            || t == privchat_protocol::ContentMessageType::Video as i32
            || t == privchat_protocol::ContentMessageType::Voice as i32
            || t == privchat_protocol::ContentMessageType::File as i32
    )
}

/// 从已存消息的 content 取回可重传的本地文件路径。首发失败的附件其 content 仍是本地
/// 路径（可能带 `file://` 前缀）；已上传成功的消息 content 是 caption，不是路径，这时
/// 返回 None，调用方按「源文件缺失」处理。
pub(crate) fn attachment_local_path(content: &str) -> Option<String> {
    let raw = content.strip_prefix("file://").unwrap_or(content);
    if raw.is_empty() {
        return None;
    }
    let path = std::path::Path::new(raw);
    if path.is_absolute() && path.is_file() {
        Some(raw.to_string())
    } else {
        None
    }
}

fn outbound_queue_ready(
    session_state: SessionState,
    has_current_uid: bool,
    has_transport: bool,
) -> bool {
    session_state == SessionState::Authenticated && has_current_uid && has_transport
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineSnapshot {
    pub messages: Vec<StoredMessage>,
    pub newest_message_id: Option<u64>,
    pub oldest_message_id: Option<u64>,
    pub has_more_before: bool,
    pub from_cache: bool,
}

/// SDK-HISTORY-5：上滑加载更早历史一页的结果。[messages]=本次回填的更早消息（本地重查、
/// 显示序 DESC）；[has_more_before]=服务端是否还有更早（来自 SDK 持久化 gap 态，UI 据此
/// 决定是否继续上滑加载，false=到顶）。
#[derive(Debug, Clone)]
pub struct OlderHistoryPage {
    pub messages: Vec<StoredMessage>,
    pub has_more_before: bool,
}

/// 打开会话时返回的最新窗口（SDK-HISTORY-7）。
#[derive(Debug, Clone)]
pub struct OpenConversationPage {
    /// 本地重读的最新窗口（显示序）。空 = 这个会话确实一条消息都没有。
    pub messages: Vec<StoredMessage>,
    /// 服务端是否还有更早（供 UI 决定是否允许继续上滑）。
    pub has_more_before: bool,
    /// 本次是否真的打了网络。仅供诊断，不参与渲染判断。
    pub fetched_from_server: bool,
}

/// 一条待补缩略图的消息。字段是入队时**拷贝**下来的，消费时不再回查消息表。
#[derive(Debug, Clone)]
struct ThumbnailBackfillItem {
    /// 入队时的会话世代。消费前再校验一次：清队列与「已经取出、正要消费」之间
    /// 仍有一个窗口，光靠 [`reset_session_scoped_state`] 清空挡不住它。
    session_epoch: u64,
    message_id: u64,
    channel_id: u64,
    channel_type: i32,
    created_at_ms: i64,
    extra: String,
}

/// 队列上限：超出就丢弃。缩略图不是必需品——UI 在气泡进入可视区时会用
/// `ensure_message_thumbnail` 单独补，那条路是用户驱动的、也是即时的。
const THUMBNAIL_BACKFILL_QUEUE_LIMIT: usize = 512;
/// 每个 tick（2s）最多补几条。这是**给 actor 留出处理命令的余地**，不是吞吐目标。
const THUMBNAIL_BACKFILL_BATCH_LIMIT: usize = 3;

/// per-channel 历史 gap 水位持久化态（KV `__hist_gap__:<ct>:<cid>`，§2.5.1 V1 最小契约）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistGapState {
    has_more_before: bool,
}

/// 「这个会话补过最新窗口了吗」（KV `__hist_hydrated__:<ct>:<cid>`，SDK-HISTORY-7）。
///
/// 必须与「本地是否为空」分开。只看本地为空的话，「从没补过」和「补过、结果就是空」
/// 无法区分，真空会话每次打开都会白打一次网络。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistHydratedState {
    hydrated: bool,
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
    /// DM 对端 user_id，来自 channel 同步下发，持久化到 channel.peer_user_id。
    /// 仅私聊设置，群聊为 None。
    pub peer_user_id: Option<u64>,
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
    /// 最后一条消息的**原始 content**（spec/05-feature/SYSTEM_MESSAGE_SPEC §3：
    /// TEXT 时是纯文本，其它类型是结构化 JSON）。**SDK 不再做 preview 文案改写**，
    /// preview 完全由 UI 层基于 [`last_message_type`] + content + i18n 决定。
    pub last_msg_content: String,
    pub version: i64,
    pub updated_at: i64,
    /// DM 会话的对端用户 ID。仅 channel_type==1 时有值，其余为 None。
    /// 派生自 channel_member 表（排除当前用户后的唯一成员）。
    pub peer_user_id: Option<u64>,
    /// 最后一条消息的 ContentMessageType 值（i32），用于让 UI 层正确渲染
    /// `[图片] / [语音] N''` 等本地化预览。None 表示该 channel 还没有消息或类型未知。
    pub last_message_type: Option<i32>,
    /// 最后一条消息是否已被撤回。撤回后 UI 应统一显示"X 撤回了一条消息"占位。
    pub last_message_is_revoked: bool,
    /// 群成员数（仅群会话有意义，来自 group 实体缓存；DM/未知为 0）。
    /// 供群标题「(N)」显示，不再依赖客户端九宫格成员预览缓存兜底。
    pub member_count: i64,
    /// DM 对端的账号类型(本地 user 实体在场时带出;None=未知)。显示名单点规则
    /// 「userType==系统 → 按 username 查语言包替换」的数据前提,零网络零二次处理。
    pub peer_user_type: Option<i32>,
    /// DM 对端的 username(同上,配合语言包按 username 精确匹配)。
    pub peer_username: Option<String>,
    /// DM 对端头像 URL(本地 user 实体在场时带出;channel.avatar 常为空,
    /// 会话列表/聊天页零网络渲染真实头像的数据前提)。
    pub peer_avatar_url: Option<String>,
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
    pub peer_read_pts: u64,
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
    pub delivered: bool,
    pub delivered_at: u64,
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
    /// AVATAR_CACHE_SPEC P1: 头像本地缓存文件绝对路径；空 = 未缓存。
    #[serde(default)]
    pub avatar_local_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpsertFriendInput {
    pub user_id: u64,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub version: i64,
    pub updated_at: i64,
    /// F-sync.2: 0=pending / 1=accepted / 2=blocked / 3=rejected / 4=recalled / 5=expired.
    /// 与 server FriendshipStatus 对齐。
    pub status: i16,
    /// 仅 status != 1 时有意义：true=我发出的，false=我收到的。accepted 行存 None。
    pub is_outgoing: Option<bool>,
    pub request_message: Option<String>,
    pub request_source: Option<String>,
    pub request_source_id: Option<String>,
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
    /// F-sync.2: 见 UpsertFriendInput::status。
    pub status: i16,
    pub is_outgoing: Option<bool>,
    pub request_message: Option<String>,
    pub request_source: Option<String>,
    pub request_source_id: Option<String>,
    /// AVATAR_CACHE_SPEC P1: 头像本地缓存文件绝对路径（LEFT JOIN user）；空 = 未缓存。
    #[serde(default)]
    pub avatar_local_path: String,
}

/// F-sync.2: friend_request 列表查询方向过滤。
///
/// 现在 friendships 在本地按 viewer 视角投影：所有 (peer_user_id) 行带
/// status + is_outgoing。Sent/Received tab 用此参数选边即可。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FriendRequestDirection {
    /// 我发出的（is_outgoing = true）
    Sent,
    /// 我收到的（is_outgoing = false）
    Received,
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
    /// 群成员数（服务端权威计数，随 group 实体同步下发）。None 时不覆盖已有值。
    pub member_count: Option<i64>,
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

/// Durable canonical mutation waiting for its target message to materialize.
/// `canonical_event` is a FlatBuffers `CanonicalTimelineEvent`, never JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTimelineMutation {
    pub channel_id: u64,
    pub channel_type: i32,
    pub target_server_message_id: u64,
    pub event_id: u64,
    pub pts: u64,
    pub canonical_event: Vec<u8>,
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
    /// 展示名（nickname）。切换账号列表的首选标题。
    pub display_name: Option<String>,
    /// username。展示优先级：display_name > username > uid。
    /// uid 是协议标识，只在前两者都没有时才允许露出来。
    pub username: Option<String>,
    /// 上次使用的登录方式（"BUILTIN" / "PLATFORM"）。
    pub login_mode: Option<String>,
    /// 上次登录填的标识（账密=username，短信=手机号），供重新登录时回填。
    pub login_identifier: Option<String>,
}

#[derive(thiserror::Error, Debug, Clone, Serialize, Deserialize)]
pub enum Error {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("storage error: {0}")]
    Storage(String),
    /// 消息行没有 Snowflake `local_message_id`，因此无法生成服务端认可的幂等
    /// 键。**永久错误**：重试多少次都一样，调用方应隔离而不是重试。
    ///
    /// 单独成一个变体而不是靠错误文案判断——文案一改，数据处理策略就跟着变，
    /// 那是把用户数据押在一句话上。
    #[error("message {message_id} has no local_message_id; cannot mint an idempotency key")]
    MissingLocalMessageId { message_id: u64 },
    #[error("sdk not connected")]
    NotConnected,
    #[error("auth failed: {0}")]
    Auth(String),
    /// 请求发出去了，但在超时窗口内**没有拿到任何应答**。
    ///
    /// 与 [`Error::Transport`] 的区别在于它对调用方是可操作的：这条连接没把请求送到，
    /// 但并不说明业务失败。半开的 socket（对端或中间 CDN 单方面关闭、本端没察觉）
    /// 正是这个形状——`is_connected()` 仍然报 true，请求写进去没人读。
    #[error("no response for {context}")]
    RequestUnanswered { context: String },
    #[error("actor closed")]
    ActorClosed,
    #[error("sdk shutdown")]
    Shutdown,
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("server error: reason_code={code} message={message}")]
    Server { code: u32, message: String },
    /// 附件重试时本地源文件已不存在（被清理 / 该行来自另一台设备）。
    /// 与普通失败区分开：这类重试**永远不可能成功**，UI 必须引导用户重新选择文件，
    /// 而不是继续显示一个点了也没用的「重试」。
    #[error("attachment source missing for message {message_id}")]
    AttachmentSourceMissing { message_id: u64 },
    /// 会话尚未鉴权（连接中 / 重连中）。这是**可重试**的时序状态，不是配置或参数错误：
    /// 上层应等待会话就绪后重试，UI 只能显示本地化的「连接中」提示，绝不能把
    /// `current: New` 这种内部状态名甩给用户。
    #[error("session not ready (current: {state})")]
    SessionNotReady { state: String },
}

/// 认证错误分层语义（spec: TOKEN_REFRESH_SPEC §2.1）。
/// - Terminal → 必须停止一切自动重连，UI 侧强制登出。
/// - Transient → 网络抖动/服务端瞬时拒绝，交给 reconnect 自愈。
/// - Recoverable → access token 已过期但 refresh token 仍有效，应走无感续期流程。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorKind {
    Transient,
    Recoverable,
    Terminal,
}

/// 服务端 AuthorizationResponse.error_code 数字码 → Terminal/Recoverable/Transient 映射。
/// 未知码保守归 Transient，避免误封锁；显式 Terminal 的都是已明确无法自愈的状态。
///
/// 对应 `privchat_protocol::ErrorCode` Authentication 段（10000–10099）：
/// - 10001 InvalidToken / 10003 TokenRevoked / 10004 PermissionDenied /
///   10005 SessionExpired / 10006 SessionNotFound / 10007 UserBanned /
///   10008 IpNotAllowed / 10009 RefreshTokenExpired / 10010 RefreshTokenRevoked
///   → Terminal
/// - 10002 TokenExpired（语义：access token 过期，refresh 可救）→ **Recoverable**
/// - 10000 AuthRequired（RPC 侧信号：缺 / 旧 access token）→ **Recoverable**，
///   调用方在拿到 Recoverable 后应先走 refresh；refresh 若失败再升级为 Terminal。
pub fn classify_auth_error_code(code: u32) -> AuthErrorKind {
    match code {
        10000   // AuthRequired（RPC 侧）：视为 access token 已失效，尝试 refresh
        | 10002 // TokenExpired：access token 过期，尝试 refresh
            => AuthErrorKind::Recoverable,
        10001  // InvalidToken
        | 10003 // TokenRevoked
        | 10004 // PermissionDenied
        | 10005 // SessionExpired
        | 10006 // SessionNotFound
        | 10007 // UserBanned
        | 10008 // IpNotAllowed
        | 10009 // RefreshTokenExpired
        | 10010 // RefreshTokenRevoked
            => AuthErrorKind::Terminal,
        _ => AuthErrorKind::Transient,
    }
}

/// 从 `Error::Auth` message 里解出前缀 `[<code>] ...` 的数字码。
/// SDK 约定所有 auth 失败的 message 都以 `[<code>] msg` 形式编码（code 为十进制 u32），
/// 这样可以保持 `Error::Auth(String)` 结构不变、不破坏 FFI/ABI。
fn parse_auth_error_code(message: &str) -> Option<u32> {
    let rest = message.strip_prefix('[')?;
    let end = rest.find(']')?;
    rest[..end].parse::<u32>().ok()
}

impl Error {
    /// 若本错误是 `Error::Auth`，根据 message 前缀的错误码分层；否则返回 None。
    pub fn auth_kind(&self) -> Option<AuthErrorKind> {
        match self {
            Error::Auth(msg) => Some(
                parse_auth_error_code(msg)
                    .map(classify_auth_error_code)
                    .unwrap_or(AuthErrorKind::Transient),
            ),
            _ => None,
        }
    }

    /// 是否为 Terminal 认证错误（token 过期/撤销等）——调用方用于决定是否停止重连。
    pub fn is_auth_terminal(&self) -> bool {
        matches!(self.auth_kind(), Some(AuthErrorKind::Terminal))
    }

    /// 从 `Error::Auth` message 里提取原始服务端错误码；没有前缀则返回 None。
    pub fn auth_error_code(&self) -> Option<u32> {
        match self {
            Error::Auth(msg) => parse_auth_error_code(msg),
            _ => None,
        }
    }

    /// Whether the failure is transient — queued work should stay in queue
    /// and retry once network/auth recovers, instead of being marked failed.
    ///
    /// 规则：
    /// - 传输层/连接断开 → 可重试（等网络恢复即可）
    /// - 鉴权错误 → 不可重试，交给 reconnect + re-authenticate 通道处理；
    ///   原地重试只会把请求反复丢到未授权会话上
    /// - 服务端业务错误（reason_code != 0）→ 仅白名单里的瞬时码可重试，其余视为永久失败
    pub fn is_retryable(&self) -> bool {
        match self {
            // Auth 错误不可重试：服务端已明确拒绝（token 过期/session 未建立/挤下线），
            // 盲目重试只会把同样的请求反复丢到未授权会话上，造成「一直发送中」。
            // 恢复路径应走 reconnect + re-authenticate，而非让 drain loop 原地重试。
            // 「没拿到应答」必须可重试。它只说明这一次请求没有回音——网络恢复后
            // 原样重发是安全的。漏掉它的代价是发送超时被当成永久失败：outbox 条目
            // 被 reject 并删除，用户的消息就此消失。
            Error::NotConnected | Error::Transport(_) | Error::RequestUnanswered { .. } => true,
            Error::Server { code, .. } => is_retryable_server_code(*code),
            _ => false,
        }
    }

    pub fn sdk_code(&self) -> u32 {
        match self {
            Error::Transport(_) => error_codes::TRANSPORT_FAILURE,
            // 没拿到应答，对宿主而言与传输失败同一类。
            Error::RequestUnanswered { .. } => error_codes::TRANSPORT_FAILURE,
            Error::Serialization(_) => error_codes::SERIALIZATION_FAILURE,
            Error::Storage(_) => error_codes::STORAGE_FAILURE,
            Error::MissingLocalMessageId { .. } => error_codes::STORAGE_FAILURE,
            Error::NotConnected => error_codes::NETWORK_DISCONNECTED,
            Error::Auth(_) => error_codes::AUTH_FAILURE,
            Error::ActorClosed => error_codes::ACTOR_CLOSED,
            Error::Shutdown => error_codes::SHUTDOWN,
            Error::InvalidState(_) => error_codes::INVALID_STATE,
            Error::AttachmentSourceMissing { .. } => error_codes::ATTACHMENT_SOURCE_MISSING,
            Error::SessionNotReady { .. } => error_codes::SESSION_NOT_READY,
            Error::Server { code, .. } => *code,
        }
    }

    pub fn protocol_code(&self) -> u32 {
        match self {
            Error::Transport(_) => ErrorCode::NetworkError as u32,
            Error::RequestUnanswered { .. } => ErrorCode::NetworkError as u32,
            Error::Serialization(_) => ErrorCode::DecodingError as u32,
            Error::Storage(_) => ErrorCode::DatabaseError as u32,
            Error::MissingLocalMessageId { .. } => ErrorCode::DatabaseError as u32,
            Error::NotConnected => ErrorCode::SessionNotFound as u32,
            Error::Auth(_) => ErrorCode::InvalidToken as u32,
            Error::ActorClosed => ErrorCode::SystemBusy as u32,
            Error::Shutdown => ErrorCode::ServiceUnavailable as u32,
            Error::InvalidState(_) => ErrorCode::OperationNotAllowed as u32,
            // 专用码：UI 据此提示「源文件已不存在，请重新选择」，而不是笼统的失败。
            Error::AttachmentSourceMissing { .. } => ErrorCode::AttachmentSourceMissing as u32,
            Error::SessionNotReady { .. } => ErrorCode::SessionNotReady as u32,
            Error::Server { code, .. } => *code,
        }
    }
}

/// 服务端业务错误码是否可重试（仅瞬时类）。
/// 其余业务码都视为永久失败 —— 例如 MessageNotFound/InvalidParams/PermissionDenied
/// 之类的用户态错误，重试只会反复失败，应当出队并标记 failed。
fn is_retryable_server_code(code: u32) -> bool {
    matches!(
        code,
        x if x == ErrorCode::SystemBusy as u32
            || x == ErrorCode::ServiceUnavailable as u32
            || x == ErrorCode::Timeout as u32
            || x == ErrorCode::Maintenance as u32
            || x == ErrorCode::DatabaseError as u32
            || x == ErrorCode::CacheError as u32
            || x == ErrorCode::NetworkError as u32
            || x == ErrorCode::RateLimitExceeded as u32
            || x == ErrorCode::ConcurrentLimitExceeded as u32
    )
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
    /// 会话快照：精确阶段 + 它属于哪个账号的哪一次会话。
    ///
    /// 宿主不能靠「读到状态时再去问一次当前是谁」来防串号——那两次读之间账号可能
    /// 已经换过，且同一个 client 会原地切号，client 身份并不等于账号身份。
    /// 三个字段必须在**同一次**读取里原子取出。
    GetSessionStatus {
        resp: oneshot::Sender<Result<SessionStatus>>,
    },
    #[cfg(test)]
    SetSessionStateForTest {
        session_state: SessionState,
        resp: oneshot::Sender<()>,
    },
    /// 读取最近一次 Terminal 认证错误快照（`None` = 当前没有未清的 ForcedLogout 记录）。
    GetLastTerminalReason {
        resp: oneshot::Sender<Result<Option<TerminalReason>>>,
    },
    /// 读取当前 access_token（权威值；SDK 内部 refresh 后这里拿到的就是新 token）。
    /// 未登录时返回 `Ok(None)`。
    GetCurrentAccessToken {
        resp: oneshot::Sender<Result<Option<String>>>,
    },
    Ping {
        resp: oneshot::Sender<Result<()>>,
    },
    SetNetworkHint {
        hint: NetworkHint,
        resp: oneshot::Sender<Result<()>>,
    },
    InboundFrame {
        /// `State::inbound_epoch` snapshot taken when the inbound task was spawned;
        /// actor drops frames whose epoch doesn't match current epoch.
        epoch: u64,
        biz_type: u8,
        data: Vec<u8>,
    },
    InboundDisconnected {
        epoch: u64,
    },
    SetVideoProcessHook {
        hook: Option<VideoProcessHook>,
        resp: oneshot::Sender<Result<()>>,
    },
    SetLinkPreviewHook {
        hook: Option<LinkPreviewHook>,
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
    Transfer {
        channel_id: u64,
        route: String,
        body: Vec<u8>,
        timeout_ms: u64,
        resp: oneshot::Sender<Result<TransferReply>>,
    },
    RunBootstrapSync {
        resp: oneshot::Sender<Result<()>>,
    },
    EnsureSynced {
        resp: oneshot::Sender<Result<()>>,
    },
    GetSyncState {
        resp: oneshot::Sender<Result<SyncStateSnapshot>>,
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
    EnqueueOutboundAttachment {
        message_id: u64,
        route_key: String,
        resp: oneshot::Sender<Result<u64>>,
    },
    PeekOutboundFiles {
        limit: usize,
        resp: oneshot::Sender<Result<Vec<QueueMessage>>>,
    },
    AckOutboundFiles {
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    KickOutboundDrain,
    CreateLocalMessage {
        input: NewMessage,
        local_message_id: Option<u64>,
        resp: oneshot::Sender<Result<u64>>,
    },
    /// 建消息 + 入队命令，同一 SQLite 事务；提交成功后才发 UI 事件。
    CreateLocalMessageQueued {
        input: NewMessage,
        local_message_id: Option<u64>,
        command_type: String,
        payload: Vec<u8>,
        route_key: Option<String>,
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
    ListMessagesAround {
        channel_id: u64,
        channel_type: i32,
        anchor_server_message_id: u64,
        before_limit: usize,
        after_limit: usize,
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
        message_seq: u32,
        resp: oneshot::Sender<Result<()>>,
    },
    /// SDK-HISTORY-2：回填式拉取频道历史（RPC message/history/get → 本地 upsert 带 pts）。
    /// 与 hydrate 的差别：不更新 channel last_message（向前翻页不能改会话预览）。
    FetchChannelHistory {
        channel_id: u64,
        channel_type: i32,
        before_server_message_id: Option<u64>,
        limit: Option<u32>,
        resp: oneshot::Sender<Result<MessageHistoryResponse>>,
    },
    /// spec §5：jump-to-message 上下文（RPC message/history/around → 全部回填本地）。
    FetchMessagesAround {
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        before_limit: Option<u32>,
        after_limit: Option<u32>,
        resp: oneshot::Sender<Result<MessageHistoryAroundResponse>>,
    },
    RepairMessageProjection {
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
        resp: oneshot::Sender<Result<Option<u64>>>,
    },
    UpdateMessageStatus {
        message_id: u64,
        status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateThumbStatus {
        message_id: u64,
        thumb_status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateMediaDownloaded {
        message_id: u64,
        downloaded: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    FinalizeLocalAttachment {
        message_id: u64,
        content: String,
        thumb_status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    /// 附件定稿 + 入队命令，同一 SQLite 事务；提交成功后才发 UI 事件。
    FinalizeAttachmentAndEnqueue {
        message_id: u64,
        content: String,
        thumb_status: i32,
        route_key: String,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<()>>,
    },
    /// INSERT a local outbound attachment row WITHOUT emitting any event.
    /// Caller must follow up with `FinalizeLocalAttachment` once files are written,
    /// which is the single event the UI observes for this message.
    CreateLocalAttachmentPlaceholder {
        input: NewMessage,
        local_message_id: Option<u64>,
        resp: oneshot::Sender<Result<u64>>,
    },
    SetMessageRevoke {
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteMessageLocal {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessage>>>,
    },
    SetChannelHidden {
        channel_id: u64,
        hidden: bool,
        resp: oneshot::Sender<Result<bool>>,
    },
    DeleteChannelLocal {
        channel_id: u64,
        resp: oneshot::Sender<Result<Vec<StoredMessage>>>,
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
    GetPeerReadPts {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<Option<u64>>>,
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
    UpdateUserAlias {
        user_id: u64,
        alias: Option<String>,
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
    ListFriendRequests {
        outgoing: bool,
        statuses: Vec<i16>,
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
    /// 显式头像 re-cache（CLIENT_GLOBAL_STATE §4.3 P2）：下载 url 到本地并强制落库。
    /// handler 内 spawn 出去（下载可能慢，不阻塞 actor loop），完成回 oneshot。
    /// resp = Ok((local_path, cached_url)) / Err。
    RecacheAvatar {
        user_id: u64,
        url: String,
        resp: oneshot::Sender<Result<(String, String)>>,
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
    SwitchLocalAccount {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    EnsureMessageThumbnail {
        message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    SetLocalAccountDisplayName {
        uid: String,
        display_name: Option<String>,
        username: Option<String>,
        login_mode: Option<String>,
        login_identifier: Option<String>,
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
    /// ForcedLogout 后驻留的终止态：必须由调用方显式 `connect()`/`reset` 才能离开。
    /// 阻止 auto-reconnect（因为 `session_state != SessionState::New`），
    /// 同时让 `connection_state()` 直观反映"被强制登出"状态。
    Terminated,
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
enum Action {
    Connect,
    Login,
    Authenticate,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectPlan {
    AlreadyReady,
    RestorePersistedSession,
    ConnectTransportOnly,
}

fn plan_connect(
    session_state: SessionState,
    has_local_session: bool,
    transport_connected: bool,
) -> Result<ConnectPlan> {
    if session_state == SessionState::Shutdown {
        return Err(Error::Shutdown);
    }
    if session_state == SessionState::Authenticated && transport_connected {
        return Ok(ConnectPlan::AlreadyReady);
    }
    if has_local_session {
        Ok(ConnectPlan::RestorePersistedSession)
    } else {
        Ok(ConnectPlan::ConnectTransportOnly)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthenticateTransportPlan {
    UseCurrent,
    ReconnectTransport,
}

fn plan_authenticate_transport(
    session_state: SessionState,
    transport_connected: bool,
) -> Result<AuthenticateTransportPlan> {
    if session_state == SessionState::Shutdown {
        return Err(Error::Shutdown);
    }
    if !transport_connected {
        return Ok(AuthenticateTransportPlan::ReconnectTransport);
    }
    session_state.can(Action::Authenticate)?;
    Ok(AuthenticateTransportPlan::UseCurrent)
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
            // 用户重新登录时需要从 Terminated 走 Connect 出发（SDK 不会自动做）。
            (SessionState::Terminated, Action::Connect) => Ok(SessionState::Connected),

            (SessionState::Connected, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::LoggedIn, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::Authenticated, Action::Login) => Ok(SessionState::LoggedIn),
            (SessionState::New, Action::Login) => Err(Error::InvalidState(
                "login requires connect first".to_string(),
            )),
            (SessionState::Terminated, Action::Login) => Err(Error::InvalidState(
                "login requires connect after forced logout".to_string(),
            )),

            (SessionState::Connected, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::LoggedIn, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::Authenticated, Action::Authenticate) => Ok(SessionState::Authenticated),
            (SessionState::New, Action::Authenticate) => Err(Error::InvalidState(
                "authenticate requires connect and login first".to_string(),
            )),
            // 放开 Terminated 下的 authenticate：业务层 refresh + re-auth 走这条；
            // 实际 transport 已断开会返回 NotConnected，由调用方决定是否先 connect。
            (SessionState::Terminated, Action::Authenticate) => Ok(SessionState::Authenticated),

            (_, Action::Shutdown) => Ok(SessionState::Shutdown),
        }
    }

    fn as_connection_state(self) -> ConnectionState {
        match self {
            SessionState::New => ConnectionState::New,
            SessionState::Connected => ConnectionState::Connected,
            SessionState::LoggedIn => ConnectionState::LoggedIn,
            SessionState::Authenticated => ConnectionState::Authenticated,
            SessionState::Terminated => ConnectionState::Terminated,
            SessionState::Shutdown => ConnectionState::Shutdown,
        }
    }
}

/// 一页 anti-entropy 扫描的结果。
///
/// `cycle_completed` 表示游标绕回了起点 —— 只有走完完整一圈**且**没有 deferred
/// stale，才能宣布收敛完成。
#[derive(Debug, Clone, Copy, Default)]
struct AntiEntropyPage {
    page_scanned: usize,
    stale_found: usize,
    channels_repaired: usize,
    messages_applied: usize,
    /// 本页发现但因预算未修的 stale 频道数
    deferred: usize,
    /// 本页中服务端没有 PTS 的频道数（异常规模指标，非正常态）
    unknown_channels: usize,
    cycle_completed: bool,
}

impl AntiEntropyPage {
    fn idle() -> Self {
        Self::default()
    }

    /// 可以进入 Converged 吗：走完一圈，且没有留下未修的 stale。
    fn is_converged(&self) -> bool {
        self.cycle_completed && self.deferred == 0
    }
}

#[derive(Debug, Clone, Copy)]
struct AntiEntropyObservation {
    key: (u64, i32),
    local_pts: u64,
    server_pts: Option<u64>,
}

#[derive(Debug, Default)]
struct AntiEntropyPlan {
    repair: Vec<(u64, i32)>,
    consumed: usize,
    last_consumed: Option<(u64, i32)>,
    stale_found: usize,
    deferred: usize,
    /// 批量比对成功但服务端没有 PTS 的频道数。
    ///
    /// 跳过它们是对的（否则游标卡死），但**必须计数**：服务端数据缺失、
    /// 已删除频道、本地脏数据会被同一条路径吞掉，不记数就看不出异常规模。
    unknown_channels: usize,
}

fn plan_anti_entropy_page(
    observations: &[AntiEntropyObservation],
    difference_budget: usize,
) -> AntiEntropyPlan {
    let mut plan = AntiEntropyPlan::default();
    for observation in observations {
        let Some(server_pts) = observation.server_pts else {
            // 批量比对**成功**返回但结果里没有这个频道 —— 服务端不认识它
            // （已删除 / 非消息频道 / 本地多余）。跳过并推进游标。
            //
            // 原来这里 break 且不推进 last_consumed，于是游标永远停在它前面，
            // 每一轮都从同一个频道重新开始：真机实测 429 会话账号出现
            // `scanned=0 deferred=1` 每 80ms 刷一次、永不收敛（deferred != 0
            // 让 is_converged() 恒假）。批量请求本身失败会在调用处 `?` 返回，
            // 走不到这里，所以缺失只可能是「服务端没有」。
            plan.unknown_channels += 1;
            plan.consumed += 1;
            plan.last_consumed = Some(observation.key);
            continue;
        };
        if server_pts <= observation.local_pts {
            plan.consumed += 1;
            plan.last_consumed = Some(observation.key);
            continue;
        }
        plan.stale_found += 1;
        if plan.repair.len() >= difference_budget {
            plan.deferred += 1;
            break;
        }
        plan.repair.push(observation.key);
        plan.consumed += 1;
        plan.last_consumed = Some(observation.key);
    }
    plan
}

struct State {
    config: PrivchatConfig,
    transport: Option<TransportClient>,
    /// 该 transport 的事件流。msgtrans 的客户端事件流是单消费者、只能取一次，
    /// 所以在建连时取走并存在这里，inbound task **借用**而不是夺走它。
    ///
    /// 这点很关键：token 刷新那条重连链会在**不换 transport** 的情况下重挂
    /// inbound task，如果第一次挂载就把流拿走，第二次就再也拿不到 →
    /// 「能发不能收」。`stop_inbound_task` 保证同一时刻只有一个任务持锁。
    transport_events: Arc<tokio::sync::Mutex<Option<msgtrans::ClientEvents>>>,
    session_state: SessionState,
    bootstrap_completed: bool,
    sync_coordinator: SyncCoordinator,
    snowflake: Arc<snowflake_me::Snowflake>,
    storage: StorageHandle,
    skip_inbound_materialization_for_load_testing: bool,
    current_uid: Option<String>,
    /// 会话世代：每次**显式**建立或废弃一个账号会话时自增。
    ///
    /// 存在的理由是宿主的对账需要一个「这份快照属于哪一次会话」的权威标识，而
    /// `current_uid` 单独不够：同一个账号被强制登出后重新登录，uid 一模一样，
    /// 但那是新会话，旧会话的终态（AuthExpired）必须在此刻、也只在此刻被清除。
    ///
    /// 只有 login / register / authenticate / switch_local_account / logout / wipe
    /// 会自增。普通重连**不会**——重连不是新会话，若在那里自增，强制登出的终态
    /// 会被下一次自动重连悄悄抹掉。
    session_epoch: u64,
    should_auto_reconnect: bool,
    reconnect_attempt: u32,
    next_reconnect_at: Option<Instant>,
    /// 一次性闸门：Terminal 级认证失败触发后置 true，防止 reconnect/Command 路径
    /// 重复发 ForcedLogout 或重启 backoff。成功完成 authenticate / login 时重置。
    auth_terminal_fired: bool,
    /// 当前 inbound 会话 epoch。每次 `start_inbound_task` 或 `trigger_forced_logout`
    /// 里显式 bump；actor loop 收到 `InboundFrame` / `InboundDisconnected` 时比对，
    /// 丢弃上一 epoch 遗留在 mpsc 通道里的帧，做到"冻结旧 inbound"。
    inbound_epoch: u64,
    /// 最近一次**成功**完成 resume sync 的 (inbound_epoch, 完成时刻)。
    /// P0-12 单轮化：同一连接世代内短时间的重复触发（冷启动 connect+bootstrap
    /// 双入口、重连多入口）直接跳过，避免 resume sync 全家桶连跑多轮放大重连风暴。
    /// 失败不记录（下一个触发点自然重试）；换代（重连）后必然重新执行。
    last_resume_synced: Option<(u64, Instant)>,
    /// Last bounded anti-entropy scan. Realtime push is the fast path; this
    /// periodic batch-PTS comparison repairs missed pushes without scanning
    /// every channel on every tick.
    last_anti_entropy_at: Instant,
    /// Phase 3 后台收敛：`Some(stats)` = 有一轮在进行中。
    /// 切账号 / 登出 / shutdown 置 `None` —— 这就是「可取消」。
    convergence_run: Option<ResumeRunStats>,
    /// 本轮 resume 的归因 id。
    ///
    /// 没有它就只能拿全量日志的汇总去猜单次启动的构成 —— 不同用户、不同轮次
    /// 混在一起，据此做的性能归因不成立（已发生过：把全用户的 get_difference
    /// 总数当成单账号一次启动的请求量）。
    resume_run_id: u64,
    anti_entropy_jitter: Duration,
    /// P1-05：room 广播按 (channel_id, server_message_id) 去重。订阅后服务端 replay
    /// 历史与实时广播在重叠窗口会重复投递同一条；每 channel 保留最近见过的一批
    /// server_message_id（有界 FIFO），命中即丢弃。server_message_id 缺失（旧帧/
    /// 无 id 的 system push）不去重，原样透传。
    room_seen_msg_ids: HashMap<u64, VecDeque<u64>>,
    /// 最近一次 Terminal 认证失败的原因。`connect()` 成功后清空。
    /// 供 debug / metrics / 冷启动诊断使用，不持久化。
    last_terminal_reason: Option<TerminalReason>,
    network_hint: NetworkHint,
    receive_pipeline: ReceivePipeline,
    last_sync_queued: usize,
    last_sync_dropped_duplicates: usize,
    last_sync_entity_events: Vec<SdkEvent>,
    video_process_hook: Option<VideoProcessHook>,
    link_preview_hook: Option<LinkPreviewHook>,
    last_tmp_cleanup_day: Option<String>,
    pending_events: Vec<SdkEvent>,
    message_cache_policy: MessageCachePolicy,
    channel_message_cache: HashMap<ChannelCacheKey, ChannelMessageCache>,
    channel_cache_generation: HashMap<ChannelCacheKey, u64>,
    /// 「有账号切换在排队」——用计数器表达，不用裸信号。
    ///
    /// actor 在 ensure_synced 里内联 await 整轮同步，期间不处理命令，所以一次很慢的
    /// 同步会把切换一起堵住（用户点了切换，界面十几秒没反应）。
    ///
    /// 为什么是计数器而不是 `Notify::notify_waiters()`：后者只唤醒**此刻已经在等**的
    /// 人，切换请求恰好落在「这一轮同步还没开始 await」的窗口里，那一声就永久丢了，
    /// 慢同步照样把切换堵死。计数器是持久状态：请求数 > 已处理数就是「有切换在排队」，
    /// 什么时候看都算数。Notify 只负责把睡着的 actor 叫醒，不承担记忆。
    switch_requested: Arc<AtomicU64>,
    switch_processed: Arc<AtomicU64>,
    switch_wakeup: Arc<tokio::sync::Notify>,
    /// 测试专用：让一轮同步「跑很久」，用来驱动真实的让出路径。
    ///
    /// 没有它就只能测纯函数——而这里要证的恰恰是 select/放弃/世代守卫在**真的
    /// ensure_synced 里**成立。设成 None 时零开销、行为完全不变。
    #[cfg(test)]
    sync_stall_for_test: Option<Duration>,
    channel_cache_lru: VecDeque<ChannelCacheKey>,
    channel_cache_total_bytes: usize,
    cache_debug_log: bool,
    cache_hit_count: u64,
    cache_miss_count: u64,
    pending_prelogin_inbound_frames: Vec<(u8, Vec<u8>)>,
    /// 活跃订阅注册表（desired subscriptions），key=(channel_id, channel_type)，value=可选 token。
    /// 掉线重连后必须 replay：否则服务端 subscribe_manager 已随旧会话清空，客户端不再收到
    /// 该频道的 presence_changed / typing / room 广播。presence 与订阅严格绑定的客户端侧基础。
    active_subscriptions: HashMap<(u64, u8), Option<String>>,
    presence_cache: Arc<StdMutex<HashMap<u64, PresenceStatus>>>,
    event_tx: Option<broadcast::Sender<SdkEvent>>,
    event_history: Option<Arc<StdMutex<VecDeque<SequencedSdkEvent>>>>,
    event_seq: Option<Arc<AtomicU64>>,
    event_history_limit: usize,
    /// Plan 2 共享作业表：Rust 发起 `MediaJobRequested` 时插入 oneshot sender，
    /// 宿主通过 `PrivchatSdk::submit_media_job_result` 直接（不经 actor cmd）
    /// 取出并触发。
    pending_media_jobs: Arc<StdMutex<HashMap<String, oneshot::Sender<MediaJobResult>>>>,
    /// 投影 repair 队列（MESSAGE_PROJECTION_SPEC §2.4）。
    ///
    /// key = (channel_type, channel_id, server_message_id)：**singleflight**。
    /// 同一条消息在多次读取里被反复发现损坏是常态（打开会话、上滑、切回来），
    /// 每次都发一遍 around 就是拿用户的流量和服务端的配额换同一个答案。
    ///
    /// 不落库，这是有意的：repair 状态没有独立价值，重启后从损坏的投影本身就能
    /// 重新发现。为它建一张表是过度设计。
    repair_queue: VecDeque<(i32, u64, u64)>,
    /// 待补缩略图的消息（SDK 内部背景工作，不阻塞任何命令）。
    ///
    /// 存在的理由：解析下载票据要打一次 `file/get_url`，而它必须在 actor 上跑
    /// （`rpc_call_json` 要 `&mut self` 做 transport 健康对账）。原来的实现在
    /// `ListMessages` 的处理器里对整页消息**串行 await** 这个 RPC——一页几千张图
    /// 就是几千次网络往返锁死 actor，宿主的所有查询排在后面饿死（真机实测：登录后
    /// `loadAllData` 的四个查询 4 分钟一个都没返回，界面永远停在「数据初始化中」）。
    /// 现在只入队，由 tick 限量消费。
    thumbnail_backfill_queue: VecDeque<ThumbnailBackfillItem>,
    thumbnail_backfill_seen: HashSet<u64>,
    /// 已排队/已修过的 key，用于 singleflight 去重。
    repair_seen: HashSet<(i32, u64, u64)>,
    /// 失败后的退避到期时间与次数：离线时不空转，恢复后由下一次读取重新发现。
    repair_backoff: HashMap<(i32, u64, u64), (u32, std::time::Instant)>,
    /// AVATAR_CACHE_SPEC P1: user 头像本地缓存管理器（in-flight/verified 去重）。
    avatar_cache: avatar_cache::AvatarCacheManager,
}

impl State {
    /// AVATAR_CACHE_SPEC P1: upsert_user 落库后触发头像本地缓存。
    ///
    /// 同步快速路径（空 URL / 进程内已验证 / 下载中）直接返回，未命中才 spawn
    /// 后台任务——sync 循环批量灌用户时不会形成 task 洪峰。不阻塞调用方。
    fn ensure_avatar_cached(&self, user_id: u64, avatar_url: &str) {
        let Some(uid) = self.current_uid.as_deref() else {
            return;
        };
        self.avatar_cache.ensure(
            self.storage.clone(),
            avatar_cache::AvatarEventSinks {
                event_tx: self.event_tx.clone(),
                event_history: self.event_history.clone(),
                event_seq: self.event_seq.clone(),
                event_history_limit: self.event_history_limit,
            },
            uid,
            user_id,
            avatar_url,
        );
    }

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

    fn anti_entropy_cursor_key() -> String {
        "__anti_entropy__:channel_cursor:v1".to_string()
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

    async fn load_anti_entropy_cursor(&self) -> (u64, i32) {
        let Some(raw) = self
            .storage
            .kv_get(Self::anti_entropy_cursor_key())
            .await
            .ok()
            .flatten()
        else {
            return (0, -1);
        };
        let Ok(text) = String::from_utf8(raw) else {
            return (0, -1);
        };
        let Some((channel_id, channel_type)) = text.split_once(':') else {
            return (0, -1);
        };
        (
            channel_id.parse().unwrap_or(0),
            channel_type.parse().unwrap_or(-1),
        )
    }

    async fn save_anti_entropy_cursor(&self, channel_id: u64, channel_type: i32) -> Result<()> {
        self.storage
            .kv_put(
                Self::anti_entropy_cursor_key(),
                format!("{channel_id}:{channel_type}").into_bytes(),
            )
            .await
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

    /// A failed request is not proof that the underlying connection died.
    /// Timeouts, protocol responses and local resource/configuration failures are scoped to the
    /// operation; the transport lifecycle event or an explicit probe owns connection teardown.
    fn transport_error_proves_disconnect(error: &TransportError) -> bool {
        matches!(error, TransportError::Connection { .. })
    }

    fn handle_transport_request_error(&mut self, context: &str, error: TransportError) -> Error {
        eprintln!("[SDK.actor] {context} transport error: {error}");
        if Self::transport_error_proves_disconnect(&error) {
            let transition = self.apply_transport_health(false);
            self.push_connection_transition_event(transition);
            self.network_disconnected_error()
        } else if matches!(error, TransportError::Timeout { .. }) {
            // 超时不是「断线」（那要 Connection 才算），但也不是普通业务失败：
            // 它意味着**没有应答**。调用方可以据此决定换一条连接重试。
            Error::RequestUnanswered {
                context: context.to_string(),
            }
        } else {
            Error::Transport(format!("{context}: {error}"))
        }
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
                // Transport went away — if the caller still wants auto-reconnect,
                // arm an immediate retry (backoff will stretch out on repeated failure).
                if self.should_auto_reconnect {
                    self.reconnect_attempt = 0;
                    self.next_reconnect_at = Some(Instant::now());
                }
                Some((from, self.session_state.as_connection_state()))
            }
            _ => None,
        }
    }

    fn reset_reconnect_backoff(&mut self) {
        self.reconnect_attempt = 0;
        self.next_reconnect_at = None;
    }

    /// Compute the next retry deadline using 1s/2s/4s/8s/16s/30s capped backoff
    /// with ±30% jitter, and advance the attempt counter.
    fn schedule_next_reconnect(&mut self) {
        let base_secs: u64 = match self.reconnect_attempt {
            0 => 1,
            1 => 2,
            2 => 4,
            3 => 8,
            4 => 16,
            _ => 30,
        };
        // ±30% jitter：server 重启/发版后全体客户端固定序列会同秒撞门（reconnect
        // storm），随机扰动把重连压力摊开（P0-12）。
        let factor = 0.7 + rand::random::<f64>() * 0.6;
        let mut delay = Duration::from_millis(((base_secs as f64) * 1000.0 * factor) as u64);
        // 离线降频**在设置时**烘进绝对 deadline(不在读取时按 now 现算,否则会被 15s
        // health_tick 反复推迟而饿死)：系统 reachability=Offline 时把探测间隔抬到 ≥60s,
        // 省电;但仍是稳定的绝对时刻,时间一到必触发一次真实 TCP 探测,网络真回来即恢复。
        if !self.network_hint.is_online() {
            delay = delay.max(Duration::from_secs(60));
        }
        self.reconnect_attempt = self.reconnect_attempt.saturating_add(1);
        self.next_reconnect_at = Some(Instant::now() + delay);
        eprintln!(
            "[SDK.actor] auto_reconnect_scheduled attempt={} delay_ms={}",
            self.reconnect_attempt,
            delay.as_millis()
        );
    }

    /// Arm the reconnect driver to fire right now and reset the attempt counter
    /// (used e.g. on network recovery).
    fn mark_reconnect_ready_now(&mut self) {
        self.reconnect_attempt = 0;
        self.next_reconnect_at = Some(Instant::now());
    }

    fn network_disconnected_error(&self) -> Error {
        Error::Transport(NETWORK_DISCONNECTED_MESSAGE.to_string())
    }

    fn classify_resume_error(err: &Error) -> ResumeFailureClass {
        match err {
            Error::Transport(_)
            | Error::RequestUnanswered { .. }
            | Error::NotConnected
            | Error::ActorClosed => ResumeFailureClass::RetryableTemporaryError,
            Error::Serialization(_) => ResumeFailureClass::FatalProtocolError,
            // 本地行缺幂等身份：重试或重连都救不回来，只能重建本地状态。
            Error::Storage(_) | Error::MissingLocalMessageId { .. } => {
                ResumeFailureClass::FullRebuildRequired
            }
            Error::Auth(message) => {
                Self::classify_resume_message(message, ResumeFailureClass::FullRebuildRequired)
            }
            Error::Shutdown => ResumeFailureClass::RetryableTemporaryError,
            // 与 resume 无关的本地附件错误：不该把它当成需要重建/重试的同步失败。
            Error::AttachmentSourceMissing { .. } => ResumeFailureClass::FatalProtocolError,
            Error::SessionNotReady { .. } => ResumeFailureClass::RetryableTemporaryError,
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
            Error::Server { code, .. } => {
                if is_retryable_server_code(*code) {
                    ResumeFailureClass::RetryableTemporaryError
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

    /// 切账号 / 换会话时必须归零的**全部**会话作用域状态。
    ///
    /// 这份清单就是 2026-07-28 那个真机 bug 的教训：当时的「切换」只改了
    /// `current_uid` 和 `bootstrap_completed`，旧 transport、旧 inbound task、旧
    /// 订阅、旧缓存原地不动，于是旧账号的失败被解释成新账号的状态，失败与重连
    /// 互相触发，5 次/秒、CPU 50%、消息发不出去。
    ///
    /// 新增任何会话作用域字段都要在这里补一行；漏一行就是下一次同样的 bug。
    /// 注意顺序：先撤销自动重连意图，再推进 epoch 作废在途帧，最后清数据——
    /// 反过来做的话，清完的结构会被仍在运行的旧任务重新写脏。
    fn reset_session_scoped_state(&mut self, now_ms: i64) {
        self.should_auto_reconnect = false;
        self.reset_reconnect_backoff();
        self.auth_terminal_fired = false;
        self.last_terminal_reason = None;

        // epoch 推进 = 旧 inbound 遗留在 mpsc 里的帧全部作废（丢弃判据见 State.inbound_epoch）。
        self.inbound_epoch = self.inbound_epoch.wrapping_add(1);
        self.last_resume_synced = None;
        self.pending_prelogin_inbound_frames.clear();
        // 缩略图回填是**账号作用域**的：队列里存的是上一个账号的 message_id，
        // 而磁盘上的 active uid 在切号时已经先改成新账号了。不清的话，旧任务会
        // 拿新账号的目录和数据库去更新旧账号的 message_id——跨账号污染。
        self.thumbnail_backfill_queue.clear();
        self.thumbnail_backfill_seen.clear();

        self.sync_coordinator.reset(now_ms);

        self.active_subscriptions.clear();
        self.clear_presence_cache();
        self.room_seen_msg_ids.clear();

        self.channel_message_cache.clear();
        self.channel_cache_generation.clear();
        self.channel_cache_lru.clear();
        self.channel_cache_total_bytes = 0;
        self.cache_hit_count = 0;
        self.cache_miss_count = 0;

        self.repair_queue.clear();
        self.repair_seen.clear();
        self.repair_backoff.clear();

        self.last_sync_queued = 0;
        self.last_sync_dropped_duplicates = 0;
        self.last_sync_entity_events.clear();
        self.pending_events.clear();
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
                        let s = String::from_utf8_lossy(&req.body);
                        if s.len() > 8192 {
                            format!("{}...", &s[..8192])
                        } else {
                            s.into_owned()
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
        let opt = RequestOptions::new().biz_type(biz_type).timeout(timeout);
        match transport.request_with_options(payload, opt).await {
            Ok(raw) => {
                if biz_type == MessageType::RpcRequest as u8 && rpc_logs_enabled() {
                    match decode_message::<RpcResponse>(&raw) {
                        Ok(resp) => {
                            let data_preview = resp
                                .data
                                .as_ref()
                                .map(|v| {
                                    let s = String::from_utf8_lossy(v);
                                    if s.len() > 8192 {
                                        format!("{}...", &s[..8192])
                                    } else {
                                        s.into_owned()
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
            Err(e) => Err(self.handle_transport_request_error(context, e)),
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
            "audio" => ContentMessageType::File,
            "location" => ContentMessageType::Location,
            "contact_card" | "contactcard" | "card" => ContentMessageType::ContactCard,
            "sticker" => ContentMessageType::Sticker,
            "forward" => ContentMessageType::Forward,
            "link" | "url" => ContentMessageType::Link,
            _ => ContentMessageType::Text,
        };
        i32::try_from(normalized.as_u32()).unwrap_or(0)
    }

    fn normalized_message_content_and_extra(
        payload: &serde_json::Value,
    ) -> (String, Option<String>) {
        match payload {
            serde_json::Value::Null => (String::new(), None),
            serde_json::Value::String(text) => {
                if let Some(envelope) = Self::decode_legacy_message_envelope(text) {
                    return Self::normalized_message_content_and_extra(&envelope);
                }
                (text.clone(), None)
            }
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

    /// A bare JSON object containing only `content` may be intentional user
    /// text. Unwrap only legacy protocol envelopes carrying an additional
    /// message marker, matching the TypeScript SDK boundary rule.
    fn decode_legacy_message_envelope(text: &str) -> Option<serde_json::Value> {
        let trimmed = text.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
        let object = value.as_object()?;
        object.get("content")?.as_str()?;
        [
            "metadata",
            "reply_to_message_id",
            "mentioned_user_ids",
            "message_source",
        ]
        .iter()
        .any(|key| object.contains_key(*key))
        .then_some(value)
    }

    fn normalize_new_message(mut input: NewMessage) -> NewMessage {
        let Some(mut envelope) = Self::decode_legacy_message_envelope(&input.content) else {
            return input;
        };
        let display_content = envelope
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if envelope.get("metadata").is_none() {
            if let Ok(metadata) = serde_json::from_str::<serde_json::Value>(&input.extra) {
                if !metadata.is_null() {
                    if let Some(object) = envelope.as_object_mut() {
                        object.insert("metadata".to_string(), metadata);
                    }
                }
            }
        }
        input.content = display_content.clone();
        input.searchable_word = display_content;
        input.extra = envelope.to_string();
        input
    }

    fn payload_bytes_to_message_content_and_extra(payload: &[u8]) -> (String, Option<String>) {
        if payload.is_empty() {
            return (String::new(), None);
        }
        if let Ok(envelope) = decode_message::<privchat_protocol::MessagePayloadEnvelope>(payload) {
            if let Ok(value) = serde_json::to_value(envelope.to_legacy()) {
                return Self::normalized_message_content_and_extra(&value);
            }
        }
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) {
            return Self::normalized_message_content_and_extra(&value);
        }
        // 二进制守卫（生产乱码事故回归；对齐 TS decodePlainTextPayload / server parse_payload）：
        // FB+JSON 都解不了且字节含 C0 控制符（\t\n\r 除外）或非 UTF-8 = 未知二进制（多为版本偏斜下
        // 的 FlatBuffers），绝不能 lossy 渲染成「控制符+长度字节+正文」——回退空 content，由
        // resync/升级收敛。
        let is_binary = match std::str::from_utf8(payload) {
            Err(_) => true,
            Ok(s) => s
                .bytes()
                .any(|b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r'),
        };
        if is_binary {
            tracing::warn!(
                len = payload.len(),
                head = %payload.iter().take(8).map(|b| format!("{b:02x}")).collect::<String>(),
                "payload 既非 envelope/JSON 也非文本，按二进制丢弃 content（版本偏斜?）"
            );
            return (String::new(), None);
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
        let resolution = commit.resolve_canonical_event();
        if resolution.canonical_legacy_mismatch {
            CANONICAL_LEGACY_MISMATCH_COUNT.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                event_id = commit.event_id,
                server_message_id = commit.server_msg_id,
                "canonical timeline event differs from legacy projection"
            );
        }
        if resolution.canonical_decode_error {
            CANONICAL_DECODE_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                event_id = commit.event_id,
                server_message_id = commit.server_msg_id,
                event_schema_version = commit.event_schema_version,
                "canonical timeline event decode failed; using whole legacy event"
            );
        }

        if matches!(
            resolution.source,
            privchat_protocol::rpc::sync::CanonicalEventSource::Canonical
        ) {
            if let Some(event) = resolution.event {
                return Self::sync_item_from_canonical_difference_event(commit, event);
            }
        }

        Self::sync_item_from_legacy_difference_commit(commit)
    }

    fn sync_item_from_canonical_difference_event(
        commit: &privchat_protocol::rpc::sync::ServerCommit,
        event: CanonicalTimelineEvent,
    ) -> (String, SyncEntityItem) {
        #[derive(Serialize)]
        struct RevokeProjection {
            message_id: u64,
            revoke: bool,
            revoked_by: u64,
            revoked_at: i64,
            channel_id: u64,
            channel_type: i32,
        }

        #[derive(Serialize)]
        struct ReactionProjection {
            message_id: u64,
            uid: u64,
            emoji: String,
            deleted: bool,
            channel_id: u64,
            channel_type: i32,
        }

        match event {
            CanonicalTimelineEvent::NewMessage(event) => {
                let legacy = event.payload.to_legacy();
                let payload = serde_json::to_value(legacy).unwrap_or(serde_json::Value::Null);
                let (content, extra) = Self::normalized_message_content_and_extra(&payload);
                (
                    "message".to_string(),
                    Self::build_message_sync_item(
                        commit.server_msg_id,
                        commit.local_message_id.unwrap_or(0),
                        commit.channel_id,
                        i32::from(commit.channel_type),
                        commit.server_timestamp,
                        commit.sender_id,
                        i32::try_from(event.message_type.as_u32()).unwrap_or(0),
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
            CanonicalTimelineEvent::Revoke(event) => {
                let payload = serde_json::to_value(RevokeProjection {
                    message_id: event.target_server_message_id,
                    revoke: true,
                    revoked_by: event.revoked_by,
                    revoked_at: event.revoked_at,
                    channel_id: commit.channel_id,
                    channel_type: i32::from(commit.channel_type),
                })
                .unwrap_or(serde_json::Value::Null);
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
            CanonicalTimelineEvent::ReactionChange(event) => {
                let entity_id = format!(
                    "{}:{}:{}",
                    event.target_server_message_id, event.actor_id, event.emoji
                );
                let payload = serde_json::to_value(ReactionProjection {
                    message_id: event.target_server_message_id,
                    uid: event.actor_id,
                    emoji: event.emoji,
                    deleted: matches!(
                        event.operation,
                        privchat_protocol::ReactionOperation::Remove
                    ),
                    channel_id: commit.channel_id,
                    channel_type: i32::from(commit.channel_type),
                })
                .unwrap_or(serde_json::Value::Null);
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
        }
    }

    fn sync_item_from_legacy_difference_commit(
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
        // push 走 canonical adapter 归一(timestamp 是**秒**——protocol/push.fbs 的
        // `uint`,u32 装不下毫秒纪元),再进 sync item。这样 push 与 sync/history 用的
        // 是同一份归一实现,也是门禁 fixture 调的那一份;此前这里是就地乘 1000,adapter
        // 只有测试在用。
        let canonical = crate::canonical_inbound::CanonicalInboundMessage::from_push(
            push.server_message_id,
            push.local_message_id,
            push.channel_id,
            i32::from(push.channel_type),
            push.from_uid,
            i32::try_from(push.message_type).unwrap_or(0),
            content,
            extra.unwrap_or_default(),
            i64::from(push.message_seq),
            i64::from(push.timestamp),
        );
        let extra = if canonical.extra.is_empty() {
            None
        } else {
            Some(canonical.extra.clone())
        };
        let mut item = Self::build_message_sync_item(
            canonical.server_message_id,
            canonical.local_message_id,
            canonical.channel_id,
            canonical.channel_type,
            canonical.sent_at_ms,
            canonical.from_uid,
            canonical.message_type,
            canonical.content.clone(),
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

    async fn apply_canonical_timeline_push(
        &mut self,
        push: &PushMessageRequest,
    ) -> Result<Option<usize>> {
        if push.topic != CANONICAL_TIMELINE_PUSH_TOPIC_V1 {
            return Ok(None);
        }
        let event = CanonicalTimelineEvent::decode_fb(&push.payload)
            .map_err(|e| Error::Serialization(format!("decode canonical timeline push: {e}")))?;
        let commit = ServerCommit {
            event_id: Some(push.server_message_id),
            pts: u64::from(push.message_seq),
            server_msg_id: push.server_message_id,
            local_message_id: None,
            channel_id: push.channel_id,
            channel_type: push.channel_type,
            message_type: String::new(),
            content: serde_json::Value::Null,
            server_timestamp: i64::from(push.timestamp).saturating_mul(1_000),
            sender_id: push.from_uid,
            sender_info: None,
            event_schema_version: Some(privchat_protocol::CANONICAL_TIMELINE_EVENT_SCHEMA_V1),
            canonical_event: Some(push.payload.clone()),
        };
        if self
            .defer_canonical_mutation_if_target_missing(&commit)
            .await?
        {
            return Ok(Some(0));
        }
        let (entity_type, item) = Self::sync_item_from_canonical_difference_event(&commit, event);
        let materializes_message = entity_type == "message" && !item.deleted;
        let scope = Some(format!("{}:{}", push.channel_type, push.channel_id));
        let mut applied = self
            .enqueue_and_apply_sync_items(entity_type, scope, vec![item], true)
            .await?;
        if materializes_message {
            applied += self
                .replay_pending_timeline_mutations(
                    push.channel_id,
                    i32::from(push.channel_type),
                    push.server_message_id,
                    true,
                )
                .await?;
        }
        Ok(Some(applied))
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

    /// 识别 `friend.request.*` 三类在线 hint topic：
    /// - `friend.request.received`：作为 target 收到新申请；
    /// - `friend.request.sent`：作为 requester 自己其他设备的"我发出了申请" hint；
    /// - `friend.request.status_changed`：accept/reject/recall 等状态变化广播。
    ///
    /// **F-sync.2 变更**：原本只识别 received 这一个 topic，且发出
    /// `SyncEntityChanged{entity_type="friend_request"}` —— 后者触发 Kotlin 侧
    /// 走老的 `friend/pending` RPC 刷新。本轮把 entity_type 统一改成 `"friend"`
    /// （和 entity/sync_entities 入参对齐），SDK 据此走 entity sync 把所有
    /// pending/rejected/recalled/expired 状态拉到本地 friend 表。
    ///
    /// 返回 `Some(peer_user_id)` 表示这是一条 friend.request.* 事件，aggregate_id
    /// 即 envelope 的 aggregate_id（对端 user_id 字符串，由 envelope 提供）。
    /// 返回 `None` 表示不是好友申请事件。
    ///
    /// 与 USER_INBOX_EVENT_ENVELOPE_SPEC §6.1 一致：**不消费 envelope payload**，
    /// 只是把 hint 转成 entity sync 触发点；权威数据来自 entity/sync_entities("friend")。
    fn push_message_to_friend_event(push: &PushMessageRequest) -> Option<u64> {
        match push.topic.as_str() {
            "friend.request.received" | "friend.request.sent" | "friend.request.status_changed" => {
            }
            _ => return None,
        }
        // envelope.aggregate_id 是十进制 user_id 字符串（见 protocol::inbox_event）。
        // 老的 `from_user_id` 字段 fallback 保留兼容历史 server。
        let payload_json: serde_json::Value =
            serde_json::from_slice(&push.payload).unwrap_or(serde_json::Value::Null);
        Self::json_field_u64(&payload_json, &["aggregate_id"])
            .or_else(|| Self::json_field_u64(&payload_json, &["requester_id"]))
            .or_else(|| Self::json_field_u64(&payload_json, &["from_user_id"]))
            .or(Some(push.from_uid))
    }

    fn entity_invalidation_pull_keys(
        push: &PushMessageRequest,
    ) -> Result<Option<Vec<(String, Option<String>)>>> {
        if push.topic != ENTITY_INVALIDATION_PUSH_TOPIC_V1 {
            return Ok(None);
        }
        let batch: EntityInvalidationBatch = match decode_message(&push.payload) {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!(%error, "dropping malformed entity invalidation hint");
                return Ok(Some(Vec::new()));
            }
        };
        if batch.schema_version != privchat_protocol::ENTITY_INVALIDATION_SCHEMA_V1 {
            tracing::warn!(
                schema_version = batch.schema_version,
                "ignoring unsupported entity invalidation schema"
            );
            return Ok(Some(Vec::new()));
        }

        // Multiple item hints for the same entity family collapse into one
        // authoritative delta pull. Entity ids and mutation hints are routing
        // metadata only; local state is never mutated from the push payload.
        let mut pulls: HashMap<(String, Option<String>), ()> = HashMap::new();
        for item in batch.items {
            if !matches!(
                item.entity_type.as_str(),
                "friend" | "user" | "group" | "group_member" | "channel" | "channel_read_cursor"
            ) {
                tracing::warn!(
                    entity_type = item.entity_type,
                    "ignoring unsupported entity invalidation type"
                );
                continue;
            }
            pulls.insert((item.entity_type, item.scope), ());
        }
        Ok(Some(pulls.into_keys().collect()))
    }

    async fn apply_entity_invalidation_push(
        &mut self,
        push: &PushMessageRequest,
    ) -> Result<Option<usize>> {
        let Some(pulls) = Self::entity_invalidation_pull_keys(push)? else {
            return Ok(None);
        };

        let mut total_applied = 0usize;
        for (entity_type, scope) in pulls {
            let applied = self
                .sync_entities(entity_type.clone(), scope.clone())
                .await?;
            total_applied += applied;
            self.pending_events
                .extend(self.last_sync_entity_events.iter().cloned());
            self.pending_events.push(SdkEvent::SyncEntitiesApplied {
                entity_type,
                scope,
                queued: self.last_sync_queued,
                applied,
                dropped_duplicates: self.last_sync_dropped_duplicates,
            });
        }
        Ok(Some(total_applied))
    }

    /// Extract a delivery receipt from a push notification, if applicable.
    fn push_message_to_delivery_receipt(push: &PushMessageRequest) -> Option<(u64, i32, u64, u64)> {
        let payload_json: serde_json::Value = serde_json::from_slice(&push.payload).ok()?;
        let notification_type =
            Self::json_field_string(&payload_json, &["metadata", "notification_type"])?;
        if notification_type != "message_receipt_updated" {
            return None;
        }
        let receipt_type = Self::json_field_string(&payload_json, &["receipt_type"])?;
        if receipt_type != "delivered" {
            return None;
        }
        let channel_id =
            Self::json_field_u64(&payload_json, &["metadata", "channel_id"]).unwrap_or(0);
        let channel_type =
            Self::json_field_u64(&payload_json, &["metadata", "channel_type"]).unwrap_or(1) as i32;
        let server_message_id =
            Self::json_field_u64(&payload_json, &["server_message_id"]).unwrap_or(0);
        let delivered_at = Self::json_field_u64(&payload_json, &["delivered_at"]).unwrap_or(0);
        if server_message_id == 0 {
            return None;
        }
        Some((channel_id, channel_type, server_message_id, delivered_at))
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
            // Channel Transfer wire packets are debug-logged only here. Outbound
            // client→app TransferRequest is matched at the transport layer (see
            // `transfer_channel`); inbound app→user TransferRequest delivery to
            // client is out of scope for v1 (BOT_INTERACTION_SPEC §0).
            MessageType::TransferRequest => {
                if let Ok(v) = decode_message::<TransferRequest>(data) {
                    eprintln!("[SDK.inbound] decoded TransferRequest: {:?}", v);
                }
            }
            MessageType::TransferResponse => {
                if let Ok(v) = decode_message::<TransferResponse>(data) {
                    eprintln!("[SDK.inbound] decoded TransferResponse: {:?}", v);
                }
            }
            MessageType::Unknown => {
                eprintln!("[SDK.inbound] unknown message type, len={}", data.len());
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
                    let friend_meta = friend_sync.friend.clone().unwrap_or_default();
                    let embedded_user = friend_sync.user.clone().unwrap_or_default();
                    if item.deleted {
                        // Server 在 Blocked(2) 时仍发 deleted=true 作为 friends-list
                        // tombstone（spec/05-feature/USER_INBOX_EVENT_ENVELOPE_SPEC §4 &
                        // server friend_service::sync_entities_page 注释）。
                        self.storage.delete_friend(user_id).await?;
                        emitted.push(SdkEvent::SyncEntityChanged {
                            entity_type: "friend".to_string(),
                            entity_id: item.entity_id.clone(),
                            deleted: true,
                        });
                        continue;
                    }
                    // F-sync.2: status 缺省按 1 (accepted) 兜底——兼容尚未升级的老 server
                    // 不发 status 字段的情形（既有行为）。新 server 总会发 0/1/3/4/5。
                    let status = friend_sync.status.unwrap_or(1);
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
                            status,
                            is_outgoing: friend_sync.is_outgoing,
                            request_message: friend_sync.request_message.clone(),
                            request_source: friend_sync.request_source.clone(),
                            request_source_id: friend_sync.request_source_id.clone(),
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
                        if let Some(avatar_url) = embedded_user.avatar.as_deref() {
                            self.ensure_avatar_cached(user_id, avatar_url);
                        }
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
                    let avatar = Self::json_get_string(&payload, &["avatar"]).unwrap_or_default();
                    self.storage
                        .upsert_user(UpsertUserInput {
                            user_id,
                            username: Self::json_get_string(&payload, &["username"]),
                            nickname: Self::json_get_string(&payload, &["nickname", "name"]),
                            alias: Self::json_get_string(&payload, &["alias"]),
                            avatar: avatar.clone(),
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
                    if !item.deleted {
                        self.ensure_avatar_cached(user_id, &avatar);
                    }
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
                            member_count: group_sync
                                .member_count
                                .map(|c| c as i64)
                                .or_else(|| Self::json_get_i64(&payload, &["member_count"])),
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
                            // DM 对端：服务端仅私聊下发；None 时 upsert SQL 用 COALESCE 保留旧值
                            peer_user_id: channel_sync
                                .peer_user_id
                                .or_else(|| Self::json_get_u64(&payload, &["peer_user_id"])),
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
                                avatar: inferred_avatar.clone(),
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
                        self.ensure_avatar_cached(member_uid, &inferred_avatar);
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
                    // 同一条 canonical → 同一条投影。sync 专属的 setting /
                    // order_seq / searchable_word 在下面单独补，它们不属于「这条
                    // 消息本身」，而是本地同步簿记。
                    let canonical =
                        crate::canonical_inbound::CanonicalInboundMessage::from_sync_entity(
                            server_message_id,
                            local_message_id,
                            channel_id,
                            channel_type,
                            from_uid,
                            message_type,
                            content.clone(),
                            extra.clone(),
                            pts,
                            timestamp,
                        );
                    let mime_type =
                        Self::extract_mime_type_from_json(&canonical.content, &canonical.extra);
                    let extra_for_thumb = canonical.extra.clone();
                    let mut input = canonical.to_upsert_input(status, mime_type);
                    input.setting = setting;
                    input.order_seq = order_seq;
                    input.searchable_word = searchable_word;
                    let upserted = self
                        .storage
                        .upsert_remote_message_with_result(input)
                        .await?;
                    let message_id = upserted.message_id;
                    // During bootstrap/periodic sync, do NOT bump unread —
                    // the authoritative count is a local projection from message timeline + read cursor.
                    // For realtime push messages, bump_unread_on_incoming is true.
                    let from_self = current_user_id.map(|v| v == from_uid).unwrap_or(false);
                    // A delayed/replayed realtime message at or before our read cursor is already
                    // read. Do not bump first and rely on a later channel read to self-heal: that
                    // makes the unread badge flash and was the reason stale unread was previously
                    // preserved incorrectly in LocalStore.
                    let is_after_read_cursor = if bump_unread_on_incoming {
                        match self
                            .storage
                            .get_channel_extra(channel_id, channel_type)
                            .await
                        {
                            Ok(Some(extra)) => {
                                u64::try_from(pts).unwrap_or_default() > extra.keep_pts
                            }
                            Ok(None) | Err(_) => true,
                        }
                    } else {
                        false
                    };
                    let should_bump_unread = bump_unread_on_incoming
                        && !from_self
                        && upserted.inserted_new
                        && is_after_read_cursor;
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.unread] message apply: channel_id={} channel_type={} message_id={} server_message_id={} from_uid={} from_self={} inserted_new={} bump_unread_on_incoming={} is_after_read_cursor={} should_bump_unread={}",
                            channel_id,
                            channel_type,
                            message_id,
                            server_message_id,
                            from_uid,
                            from_self,
                            upserted.inserted_new,
                            bump_unread_on_incoming,
                            is_after_read_cursor,
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

                    // Auto-download thumbnail for incoming image/video messages
                    let is_image_or_video = message_type
                        == i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(2)
                        || message_type
                            == i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(3);
                    if is_image_or_video && !from_self {
                        // 同样只入队。入站路径每条消息都要解析一次票据，一轮大同步灌进来
                        // 几百条图片就是几百次串行网络往返压在 actor 上——与列表查询那处
                        // 是同一个病。DB 的 created_at 是毫秒，回填路径要对齐同一单位。
                        let created_at_ms = chrono::Utc::now().timestamp_millis();
                        self.enqueue_thumbnail_backfill(
                            message_id,
                            channel_id,
                            channel_type,
                            created_at_ms,
                            &extra_for_thumb,
                        );
                    }

                    // NewMessage is immutable. Realtime delivery and anti-entropy may overlap,
                    // so replaying an already materialized server message must not trigger a
                    // second entity/timeline refresh or flood the SDK broadcast channel.
                    // Optimistic echo reconciliation is surfaced separately through
                    // MessageSendStatusChanged below.
                    if upserted.inserted_new {
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
                    }
                    // 回显 vs 新消息：以 server_message_id 是否已存在本地（inserted_new）为准，
                    // 不能用 from_self 判定——服务端代发消息（如 RP-12 资金卡片注入，sender=本人
                    // 但本地无乐观原件）必须当新消息上抛；只有命中已有行（本地乐观发送的回显 /
                    // 重复推送）才走 MessageSendStatusChanged 状态更新。from_self 只影响未读/提示，
                    // 不决定消息是否进 timeline。
                    if !upserted.inserted_new && (local_message_id > 0 || from_self) {
                        emitted.push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status,
                            server_message_id: Some(server_message_id),
                        });
                    } else if upserted.inserted_new && from_self {
                        if inbound_logs_enabled() {
                            eprintln!(
                                "[SDK.inbound] server-authored self message surfaced as NEW: channel_id={} message_id={} server_message_id={}",
                                channel_id, message_id, server_message_id
                            );
                        }
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
                    } else if read_pts > 0 {
                        let _ = self
                            .storage
                            .save_peer_read_pts(channel_id, channel_type, read_pts)
                            .await;
                        emitted.push(SdkEvent::PeerReadPtsAdvanced {
                            channel_id,
                            channel_type,
                            reader_id,
                            read_pts,
                        });
                        if inbound_logs_enabled() {
                            eprintln!(
                                "[SDK.read] peer_read_pts_advanced: channel_id={} channel_type={} reader_id={} read_pts={}",
                                channel_id, channel_type, reader_id, read_pts
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

    /// 需要服务端已完成鉴权（ConnAuth 通过）才能执行业务 RPC。
    /// 之前的 `can(Action::Authenticate)` 是状态机的「可转移到 Authenticated 吗」检查，
    /// 它在 Connected/LoggedIn 也返回 Ok，导致请求落到未授权会话上被服务端拒。
    fn require_authenticated(&self) -> Result<()> {
        match self.session_state {
            SessionState::Authenticated => Ok(()),
            SessionState::Shutdown => Err(Error::Shutdown),
            // 结构化 + 可重试：连接/重连过程中的调用不是「非法状态」，
            // 上层据此等待并重试，UI 走本地化提示。
            _ => Err(Error::SessionNotReady {
                state: format!("{:?}", self.session_state),
            }),
        }
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

    /// 出站队列是否可以排空。
    ///
    /// 判据只有三条**可验证的本地事实**：会话已鉴权、有当前用户、transport 还在。
    /// 只有鉴权完成的 session 才能发业务 RPC；Connected/LoggedIn 都可能跑在服务端未授权
    /// 的通道上（例如重连刚握好 TCP、ConnAuth 还没回），那时 drain 只会触发 10000。
    ///
    /// ⚠️ **绝不能把 `network_hint` 放进来**（2026-07-26 生产事故）：系统 reachability 卡在
    /// Offline 后，inbound 推送仍走活着的 transport 正常收消息，而出站队列因这个假闸门永远
    /// 排不空——用户看到「能收到消息，自己发的永远停在『发送中』，既不成功也不失败」。
    /// hint 只允许影响探测/重试的**频率**，不能决定「做不做事」。真断网时 transport 已为
    /// None（或发送直接失败），上面三条判据自然拦住，代价可控。
    fn should_process_outbound_queue(&self) -> bool {
        outbound_queue_ready(
            self.session_state,
            self.current_uid.is_some(),
            self.transport.is_some(),
        )
    }

    async fn cleanup_tmp_dirs_if_needed(&mut self) -> Result<()> {
        if self.current_uid.is_none() {
            return Ok(());
        }
        let today = chrono::Utc::now().format("%Y%m").to_string();
        if self.last_tmp_cleanup_day.as_deref() == Some(today.as_str()) {
            return Ok(());
        }
        let paths = self.storage.get_storage_paths().await?;
        let user_root = PathBuf::from(&paths.user_root);
        let tmp_root = user_root.join("files").join("tmp");
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
                if name.len() == 6 && name.chars().all(|c| c.is_ascii_digit()) && name < today {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
        // Also clean up legacy {user_root}/tmp/ directory
        let legacy_tmp_root = user_root.join("tmp");
        if legacy_tmp_root.exists() {
            let _ = std::fs::remove_dir_all(&legacy_tmp_root);
        }
        self.last_tmp_cleanup_day = Some(today);
        Ok(())
    }

    async fn drain_normal_queue_once(&mut self, limit: usize) -> Result<usize> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let items = self.storage.outbox_peek("message", limit, now_ms).await?;
        if items.is_empty() {
            return Ok(0);
        }
        let mut processed = 0usize;
        for (message_id, _cmd_type, _channel_id, _payload, _route_key, retry_count) in items {
            let msg = match self.storage.get_message_by_id(message_id).await? {
                Some(v) => v,
                None => {
                    let _ = self.storage.outbox_drop(message_id).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "drop_missing".to_string(),
                        message_id: Some(message_id),
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
                    // 更新消息与删除 outbox 行是一个事务（MESSAGE_SPEC §8.3）。
                    // 失败即整体回滚：outbox 行原样还在，下一轮用同一个
                    // local_message_id 重试，服务端按幂等返回同样的结果。没有
                    // 「服务端已收、本地永远发送中」这个窗口可言。
                    if let Err(err) = self
                        .storage
                        .outbox_ack_sent(message_id, resp.server_message_id, resp.message_seq)
                        .await
                    {
                        let next_at = self.outbox_next_attempt_at(retry_count);
                        let _ = self
                            .storage
                            .outbox_bump_retry(message_id, next_at, &err.to_string())
                            .await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: "commit_retry".to_string(),
                            message_id: Some(message_id),
                        });
                        return Err(err);
                    }
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
                        // 补记已送达 + 清命令，同一事务：分两步写的话，崩在
                        // 中间下一轮会把这条已经送达的消息再发一次。
                        //
                        // 事务失败就不发 Sent：命令还在队列里，UI 说「已发送」
                        // 而队列随后又发一次，是最难查的一类重复。
                        if let Err(commit_err) = self
                            .storage
                            .outbox_reconcile_sent(message_id, server_message_id)
                            .await
                        {
                            tracing::warn!(
                                message_id,
                                error = %commit_err,
                                "reconciliation not committed; not publishing Sent"
                            );
                            break;
                        }
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: "dequeue_reconciled".to_string(),
                            message_id: Some(message_id),
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
                    if e.is_retryable() {
                        eprintln!(
                            "[SDK.actor] normal queue send deferred (retryable): message_id={} error={}",
                            message_id, e
                        );
                        // 写退避，否则这条命令下一轮立刻又被取出，变成忙循环打服务端。
                        let next_at = self.outbox_next_attempt_at(retry_count);
                        if let Err(backoff_err) = self
                            .storage
                            .outbox_bump_retry(message_id, next_at, &e.to_string())
                            .await
                        {
                            // 不是终态，不阻断；但必须可见——退避没写进去
                            // 就意味着下一轮会立刻重试。
                            tracing::warn!(
                                message_id,
                                error = %backoff_err,
                                "backoff not persisted; the command may retry immediately"
                            );
                        }
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "normal".to_string(),
                            action: format!("deferred:{}", e),
                            message_id: Some(message_id),
                        });
                        break;
                    }
                    eprintln!(
                        "[SDK.actor] normal queue send failed: message_id={} error={}",
                        message_id, e
                    );
                    // 标记失败 + 删命令，同一事务（见 outbox_reject）。事务没提交
                    // 就什么都别发：命令还在队列里，这条消息并没有「结束」。
                    if let Err(commit_err) = self.storage.outbox_reject(message_id, 3).await {
                        tracing::warn!(
                            message_id,
                            error = %commit_err,
                            "reject not committed; leaving the command queued"
                        );
                        break;
                    }
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "failed".to_string(),
                        message_id: Some(message_id),
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 3,
                            server_message_id: None,
                        });
                    // 命令行已在 outbox_reject 的同一事务里删掉了，这里只报事件。
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "normal".to_string(),
                        action: "failed_drop".to_string(),
                        message_id: Some(message_id),
                    });
                    processed += 1;
                    continue;
                }
            }
        }
        Ok(processed)
    }

    async fn drain_attachment_outbox_once(&mut self, limit: usize) -> Result<usize> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let items = self
            .storage
            .outbox_peek("attachment", limit, now_ms)
            .await?;
        if items.is_empty() {
            return Ok(0);
        }
        eprintln!(
            "[SDK.actor] drain_attachment_outbox_once: items={}",
            items.len()
        );
        let mut processed = 0usize;
        for (message_id, _cmd_type, _channel_id, payload, _route_key, retry_count) in items {
            let msg = match self.storage.get_message_by_id(message_id).await? {
                Some(v) => v,
                None => {
                    let _ = self.storage.outbox_drop(message_id).await;
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "drop_missing".to_string(),
                        message_id: Some(message_id),
                    });
                    processed += 1;
                    continue;
                }
            };
            let local_message_id = self.ensure_local_message_id(message_id).await?;
            eprintln!(
                "[SDK.actor] drain_attachment_outbox_once: processing message_id={} payload_len={} created_at={}",
                message_id,
                payload.len(),
                msg.created_at
            );
            match self
                .process_outbound_file(&msg, local_message_id, payload)
                .await
            {
                Ok(resp) => {
                    // 附件与普通消息共用同一套持久化规则（见 normal 分支注释）：
                    // 一个事务里更新消息并删除 outbox 行，失败就整体回滚。
                    if let Err(err) = self
                        .storage
                        .outbox_ack_sent(message_id, resp.server_message_id, resp.message_seq)
                        .await
                    {
                        let next_at = self.outbox_next_attempt_at(retry_count);
                        let _ = self
                            .storage
                            .outbox_bump_retry(message_id, next_at, &err.to_string())
                            .await;
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "file".to_string(),
                            action: "commit_retry".to_string(),
                            message_id: Some(message_id),
                        });
                        return Err(err);
                    }
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "dequeue".to_string(),
                        message_id: Some(message_id),
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
                        if let Err(commit_err) = self
                            .storage
                            .outbox_reconcile_sent(message_id, server_message_id)
                            .await
                        {
                            tracing::warn!(
                                message_id,
                                error = %commit_err,
                                "reconciliation not committed; not publishing Sent"
                            );
                            break;
                        }
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "file".to_string(),
                            action: "dequeue_reconciled".to_string(),
                            message_id: Some(message_id),
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
                    if e.is_retryable() {
                        // Transient failure (auth/network): keep the item in the queue
                        // and stop draining — next connection / auth / enqueue trigger
                        // will retry. Don't ack, don't mark the message failed.
                        eprintln!(
                            "[SDK.actor] attachment send deferred (retryable): message_id={} error={}",
                            message_id, e
                        );
                        // 同 normal 分支：不写退避就会忙循环。
                        let next_at = self.outbox_next_attempt_at(retry_count);
                        if let Err(backoff_err) = self
                            .storage
                            .outbox_bump_retry(message_id, next_at, &e.to_string())
                            .await
                        {
                            // 不是终态，不阻断；但必须可见——退避没写进去
                            // 就意味着下一轮会立刻重试。
                            tracing::warn!(
                                message_id,
                                error = %backoff_err,
                                "backoff not persisted; the command may retry immediately"
                            );
                        }
                        self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                            kind: "file".to_string(),
                            action: format!("deferred:{}", e),
                            message_id: Some(message_id),
                        });
                        break;
                    }
                    eprintln!(
                        "[SDK.actor] attachment send failed: message_id={} error={}",
                        message_id, e
                    );
                    // 标记失败 + 删命令必须同事务；只更新状态会把命令留在队列，
                    // 下一轮又发一遍一条已经被永久拒绝的附件。
                    if let Err(commit_err) = self.storage.outbox_reject(message_id, 3).await {
                        // 事务没提交：命令还在，状态也没变。这才是真实状态，
                        // 不能发终态事件——发了 UI 就以为结束了。
                        tracing::warn!(
                            message_id,
                            error = %commit_err,
                            "attachment reject not committed; leaving the command queued"
                        );
                        break;
                    }
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: format!("failed:{}", e),
                        message_id: Some(message_id),
                    });
                    self.pending_events
                        .push(SdkEvent::MessageSendStatusChanged {
                            message_id,
                            status: 3,
                            server_message_id: None,
                        });
                    self.pending_events.push(SdkEvent::OutboundQueueUpdated {
                        kind: "file".to_string(),
                        action: "failed_drop".to_string(),
                        message_id: Some(message_id),
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

    /// 重试退避：1s, 2s, 4s, 8s, 16s（MESSAGE_SPEC §8.4）。
    fn outbox_next_attempt_at(&self, retry_count: i64) -> i64 {
        let step = retry_count.clamp(0, 4) as u32;
        let delay_ms = 1000i64 * (1i64 << step);
        chrono::Utc::now().timestamp_millis() + delay_ms
    }

    async fn drain_outbound_queues(&mut self) -> Result<usize> {
        if !self.should_process_outbound_queue() {
            return Ok(0);
        }
        let mut drained = 0usize;
        drained += self
            .drain_normal_queue_once(OUTBOUND_DRAIN_BATCH_SIZE)
            .await?;
        // 一张 outbox 表，扫一次。以前这里按 sled 分片数循环 N 遍，每遍查的却是
        // 同一张 SQLite 表——不会重复发送（actor 串行），但是 N 倍的无效扫描，
        // 还把没有意义的 queue_index 一路暴露到事件和 FFI 上。
        drained += self
            .drain_attachment_outbox_once(OUTBOUND_DRAIN_BATCH_SIZE)
            .await?;
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
                Ok(Ok((c, events))) => {
                    self.transport = Some(c);
                    *self.transport_events.lock().await = Some(events);
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
        if realtime_trace_enabled() {
            eprintln!(
                "[SDK_RECONNECT_BEGIN] old_epoch={} state_before={:?} uid={:?}",
                self.inbound_epoch, self.session_state, self.current_uid
            );
        }
        tracing::debug!(
            state_before = ?self.session_state,
            current_uid_present = self.current_uid.is_some(),
            "auto reconnect started"
        );
        match timeout(self.connect_timeout_total(), self.connect()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "auto reconnect transport failed");
                return Err(e);
            }
            Err(_) => {
                let e = Error::Transport("reconnect timeout".to_string());
                tracing::warn!(error = %e, "auto reconnect timed out");
                return Err(e);
            }
        }
        let uid = match self.current_uid.clone() {
            Some(v) => v,
            None => {
                tracing::debug!("auto reconnect has no persisted user");
                return Ok(SessionState::Connected);
            }
        };
        let snapshot = match self.storage.load_session(uid.clone()).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                tracing::debug!(user_id = %uid, "auto reconnect has no session snapshot");
                return Ok(SessionState::Connected);
            }
            Err(e) => {
                tracing::warn!(user_id = %uid, error = %e, "auto reconnect session load failed");
                return Ok(SessionState::Connected);
            }
        };

        tracing::debug!(user_id = %snapshot.user_id, "restoring persisted session");
        let user_id = snapshot.user_id;
        let device_id = snapshot.device_id.clone();
        let bootstrap_completed = snapshot.bootstrap_completed;
        let has_access_token = !snapshot.token.is_empty();
        tracing::debug!(
            user_id = %user_id,
            device_id = %device_id,
            has_access_token,
            state_before = ?self.session_state,
            "authenticating restored session"
        );
        let first_attempt = timeout(
            Duration::from_secs(20),
            self.authenticate(user_id, snapshot.token, device_id.clone()),
        )
        .await;
        let auth_result = match first_attempt {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Transport("reconnect auth timeout".to_string())),
        };
        match auth_result {
            Ok(()) => {
                self.bootstrap_completed = bootstrap_completed;
                tracing::debug!(user_id = %user_id, "persisted session restored");
                Ok(SessionState::Authenticated)
            }
            Err(e) => {
                // 新 transport 还留着服务端会视为「未认证会话」，后续 RPC 会被 10000 拒绝。
                // 主动释放，让监控循环走 backoff 再来一次完整握手。
                eprintln!("[SDK.actor] monitor: restore auth failed: {e} (dropping transport)");
                let _ = self.disconnect().await;
                Err(e)
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
        let options = RequestOptions::new()
            .biz_type(MessageType::PingRequest as u8)
            .timeout(Duration::from_secs(2));
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

    async fn connect_one(
        &self,
        ep: &ServerEndpoint,
    ) -> Result<(TransportClient, msgtrans::ClientEvents)> {
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] connect_one: begin");
        }
        let timeout = self.timeout();
        let target = Self::resolve_target(&ep.host, ep.port).await?;
        let mut client = match ep.protocol {
            TransportProtocol::Quic => {
                let mut cfg = QuicClientConfig::new(&target)
                    .map_err(|e| Error::Transport(format!("quic config: {e}")))?
                    .connect_timeout(timeout)
                    .server_name(ep.host.clone());
                if quic_accept_self_signed_for_testing_enabled() {
                    warn_quic_insecure_verification_disabled_once();
                    cfg = cfg.danger_skip_verification();
                }
                TransportClientBuilder::new()
                    .protocol(cfg)
                    .build()
                    .await
                    .map_err(|e| Error::Transport(format!("quic build: {e}")))?
            }
            TransportProtocol::Tcp => {
                let cfg = TcpClientConfig::new(&target)
                    .map_err(|e| Error::Transport(format!("tcp config: {e}")))?
                    .connect_timeout(timeout);
                TransportClientBuilder::new()
                    .protocol(cfg)
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
                    .connect_timeout(timeout)
                    // msgtrans 2.0: TLS 行为改为 ClientTls 枚举(旧 verify_tls
                    // 是从未接线的假开关);use_tls=false 的端点走 ws:// 本就
                    // 不触发 TLS,这里映射为 Insecure 仅保语义完整。
                    .tls(if ep.use_tls {
                        msgtrans::ClientTls::SystemRoots
                    } else {
                        msgtrans::ClientTls::Insecure
                    });
                TransportClientBuilder::new()
                    .protocol(cfg)
                    .build()
                    .await
                    .map_err(|e| Error::Transport(format!("ws build: {e}")))?
            }
        };
        // 先取事件流再 connect：事件流是有界队列，连接期间的事件会排队而不是丢失。
        let events = client
            .events()
            .await
            .map_err(|e| Error::Transport(format!("events: {e}")))?;
        client
            .connect()
            .await
            .map_err(|e| Error::Transport(format!("connect: {e}")))?;
        if actor_logs_enabled() {
            eprintln!("[SDK.actor] connect_one: connected");
        }
        Ok((client, events))
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
            body: serde_json::to_vec(&AuthLoginRequest {
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
        let auth: AuthResponse = serde_json::from_slice(&body)
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
        self.session_epoch += 1;
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
            body: serde_json::to_vec(&UserRegisterRequest {
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
        let auth: AuthResponse = serde_json::from_slice(&body)
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
        self.session_epoch += 1;
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
            // 把服务端 error_code（u32）塞进 message 前缀 `[<code>] ...`，供上层 auth_kind() 解出。
            // 0 表示未携带码，归 Transient。
            let code = auth_resp.error_code.unwrap_or(0);
            let message = auth_resp
                .error_message
                .unwrap_or_else(|| "authorization failed".to_string());
            return Err(Error::Auth(format!("[{}] {}", code, message)));
        }
        let uid = user_id.to_string();
        // authenticate() 的职责只是用当前 access_token 握手；不应覆盖 login/register 写入的
        // refresh_token 或过期时间。若该用户已有会话，只原子刷新 access_token；否则才走
        // save_login（用于外部认证首次 handshake，无 refresh_token）。
        let existing_session = self.storage.load_session(uid.clone()).await.ok().flatten();
        if let Some(existing) = existing_session.as_ref() {
            // 冷启动 restore：从持久化 snapshot 恢复 bootstrap_completed，与自动重连路径
            // （restore_persisted_session）保持一致。否则每次冷启动 bootstrap_completed 停留在
            // false → ① 本地优先读（get_channels 等）被 current_uid_required gate 拒绝，UI 只能
            // 等网络 sync 才出会话；② 后续 run_bootstrap_sync 被迫走全量而非增量 resume。
            // full_rebuild_required() 仍是安全网：本地 store 需重建时即便标记为 true 也会走全量。
            self.bootstrap_completed = existing.bootstrap_completed;
        }
        if existing_session.is_some() {
            self.storage
                .update_access_token(uid.clone(), token_for_persist, None)
                .await?;
            // update_access_token 不写 K_CUR_UID；与 save_login 分支保持一致，确保磁盘 current_uid
            // 与本次 authenticate 的 uid 对齐，避免后续 with_uid! 命令读到 None。
            self.storage.save_current_uid(uid.clone()).await?;
        } else {
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
        }
        self.storage.flush_user(uid.clone()).await?;
        self.current_uid = Some(uid);
        self.session_epoch += 1;
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
                body: serde_json::to_vec(&req)
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
                        let s = String::from_utf8_lossy(v);
                        if s.len() > 512 {
                            format!("{}...", &s[..512])
                        } else {
                            s.into_owned()
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
                serde_json::from_slice(&body).map_err(|e| {
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
            body: serde_json::to_vec(&ready_req)
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
            serde_json::from_slice(&body)
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
            body: serde_json::to_vec(&req)
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
        serde_json::from_slice(&body)
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
            body: serde_json::to_vec(&req)
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
        let resp: GetChannelPtsResponse = serde_json::from_slice(&body)
            .map_err(|e| Error::Serialization(format!("decode get_channel_pts response: {e}")))?;
        Ok(resp.current_pts)
    }

    async fn batch_get_channel_pts(
        &mut self,
        channels: Vec<(u64, i32)>,
    ) -> Result<BatchGetChannelPtsResponse> {
        self.rpc_call_typed(
            routes::sync::BATCH_GET_CHANNEL_PTS,
            &BatchGetChannelPtsRequest {
                channels: channels
                    .into_iter()
                    .map(|(channel_id, channel_type)| ChannelIdentifier {
                        channel_id,
                        channel_type: u8::try_from(channel_type).unwrap_or(1),
                    })
                    .collect(),
            },
        )
        .await
    }

    /// wire message_type 字符串 → 本地 i32（history/around/hydrate 共用，防三处漂移）。
    fn wire_message_type_to_i32(t: &str) -> i32 {
        let ct = match t {
            "image" => ContentMessageType::Image,
            "voice" => ContentMessageType::Voice,
            "video" => ContentMessageType::Video,
            // 注：普通音频文件（mp3/wav/...）作为 File 消息发送，不再有独立 Audio 类型
            "file" | "audio" => ContentMessageType::File,
            // RP-12：资金卡片（服务端注入）历史/冷同步必须保留类型，否则落成 Text→渲染原始 JSON
            "red_packet" => ContentMessageType::RedPacket,
            "money_transfer" => ContentMessageType::MoneyTransfer,
            "system" => ContentMessageType::System,
            _ => ContentMessageType::Text,
        };
        i32::try_from(ct.as_u32()).unwrap_or(0)
    }

    /// 单条历史消息落库（SDK-HISTORY-2 核心）：status=2 + 真实 pts(=message_seq)。
    /// get/around 回填共用；upsert 的乱序回退 guard 由 order_seq 保证幂等。
    /// history / around 响应的一条消息 → 落库入参。
    ///
    /// 一处定义,三条回填路径共用(store_history_item / channel hydrate /
    /// bootstrap 内联 hydrate)。此前是三份手抄,`extra` 在其中两份里被写成空串,
    /// 于是同一条图片消息「翻页翻到」和「hydrate 拉到」的行内容不一样——这种分叉
    /// 只要还存在三份,就一定会再次发生。
    fn history_item_to_upsert_input(
        item: &MessageHistoryItem,
        channel_type: i32,
    ) -> UpsertRemoteMessageInput {
        let canonical = crate::canonical_inbound::CanonicalInboundMessage::from_history_item(
            item,
            if channel_type == 0 { 1 } else { channel_type },
            Self::wire_message_type_to_i32,
        );
        let mime_type = Self::extract_mime_type_from_json(&canonical.content, &canonical.extra);
        // status=2：history 返回的都是服务端已确认的消息。
        canonical.to_upsert_input(2, mime_type)
    }

    async fn store_history_item(
        &mut self,
        item: &MessageHistoryItem,
        channel_type: i32,
    ) -> Result<u64> {
        Ok(self
            .storage
            .upsert_remote_message_with_result(Self::history_item_to_upsert_input(
                item,
                channel_type,
            ))
            .await?
            .message_id)
    }

    /// 回填式拉取频道历史：RPC → 逐条落库 → 返回响应（spec §6：get 结果必须回填，
    /// UI 随后从本地库重查渲染）。不更新 last_message（向前翻页不改会话预览）。
    async fn fetch_and_store_channel_history(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        before_server_message_id: Option<u64>,
        limit: Option<u32>,
    ) -> Result<MessageHistoryResponse> {
        let req = MessageHistoryGetRequest {
            user_id: 0,
            channel_id,
            before_server_message_id,
            limit,
        };
        let resp: MessageHistoryResponse = self
            .rpc_call_typed(routes::message_history::GET, &req)
            .await?;
        let normalized_channel_type = if channel_type == 0 { 1 } else { channel_type };
        for item in &resp.messages {
            self.store_history_item(item, normalized_channel_type)
                .await?;
        }
        Ok(resp)
    }

    /// jump-to-message 上下文：RPC around → before/anchor/after 全部回填 → 返回响应。
    /// search 命中（snippet 投影）不落库；点击后走本方法拿完整消息（spec §4/§5/§6 边界）。
    async fn fetch_and_store_messages_around(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        before_limit: Option<u32>,
        after_limit: Option<u32>,
    ) -> Result<MessageHistoryAroundResponse> {
        let req = MessageHistoryAroundRequest {
            channel_id,
            message_id,
            before_limit,
            after_limit,
        };
        let resp: MessageHistoryAroundResponse = self
            .rpc_call_typed(routes::message_history::AROUND, &req)
            .await?;
        let normalized_channel_type = if channel_type == 0 { 1 } else { channel_type };
        for item in resp
            .before_messages
            .iter()
            .chain(std::iter::once(&resp.anchor_message))
            .chain(resp.after_messages.iter())
        {
            self.store_history_item(item, normalized_channel_type)
                .await?;
        }
        Ok(resp)
    }

    /// 把一条损坏的投影排进 repair 队列。**立即返回，不发网络。**
    ///
    /// 调用它的是读路径（打开会话、上滑翻页）——那里绝不能等一串 around 请求：
    /// 坏数据还没修好之前会话就先卡住，用户付出的代价比问题本身大。真正的修复由
    /// actor 的 repair tick 取队列执行。
    ///
    /// 三道闸门，少一道就会变成「每次读都打一遍服务端」：
    /// - **singleflight**：`repair_seen` 保证同一条消息只排一次。反复读到同一条坏行
    ///   是常态（打开、上滑、切回来），每次都发请求是拿用户流量换同一个答案。
    /// - **有界**：队列超过上限就丢弃，留给下一次读取重新发现——堆积没有意义，
    ///   因为损坏是从数据本身发现的，不会丢。
    /// - **退避**：失败多半是离线或服务端不稳，退避期内直接跳过。不需要额外的
    ///   「网络恢复唤醒」：读路径本身就是唤醒信号。
    fn enqueue_projection_repair(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) {
        let key = (channel_type, channel_id, server_message_id);
        if let Some((_, until)) = self.repair_backoff.get(&key) {
            if std::time::Instant::now() < *until {
                return;
            }
        }
        if !self.repair_seen.insert(key) {
            return;
        }
        if self.repair_queue.len() >= REPAIR_QUEUE_LIMIT {
            self.repair_seen.remove(&key);
            return;
        }
        self.repair_queue.push_back(key);
    }

    /// 登记一条待补缩略图的消息。**纯内存操作，不做任何 await。**
    ///
    /// 这是本修复的要害：调用方（消息列表查询、入站消息）只负责说「这条需要补」，
    /// 网络往返一律留给 [`drain_thumbnail_backfill`] 在 tick 里限量做。
    fn enqueue_thumbnail_backfill(
        &mut self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        created_at_ms: i64,
        extra: &str,
    ) {
        if !self.thumbnail_backfill_seen.insert(message_id) {
            return;
        }
        if self.thumbnail_backfill_queue.len() >= THUMBNAIL_BACKFILL_QUEUE_LIMIT {
            self.thumbnail_backfill_seen.remove(&message_id);
            return;
        }
        self.thumbnail_backfill_queue.push_back(ThumbnailBackfillItem {
            session_epoch: self.session_epoch,
            message_id,
            channel_id,
            channel_type,
            created_at_ms,
            extra: extra.to_string(),
        });
    }

    /// 补一小批缩略图。由 actor tick 调用，每次最多
    /// [`THUMBNAIL_BACKFILL_BATCH_LIMIT`] 条——**下载仍然是 spawn 的，这里只做票据解析**。
    /// [`commands_idle`] 由 actor 传入，用来在**每一条之间**重新判断队列里有没有
    /// 待处理命令——后台工作让位给宿主请求，是每一步都要成立的，不是开头一次。
    async fn drain_thumbnail_backfill(&mut self, commands_idle: impl Fn() -> bool) {
        // 没有当前账号就没有归属可言，整批不做。
        let Some(owner_uid) = self.current_uid.clone() else {
            return;
        };
        let user_root = match self.storage.get_storage_paths().await {
            Ok(paths) => PathBuf::from(&paths.user_root),
            Err(_) => return,
        };
        for _ in 0..THUMBNAIL_BACKFILL_BATCH_LIMIT {
            // 每条之间都重新让位。只在开头查一次是不够的：一条 `file/get_url`
            // 在坏网络下可以耗到整个请求超时，三条串起来就是几十秒，期间新到的
            // 宿主命令只能干等——那样「后台工作永不压过宿主请求」就是句空话。
            if !commands_idle() {
                return;
            }
            let Some(item) = self.thumbnail_backfill_queue.pop_front() else {
                return;
            };
            self.thumbnail_backfill_seen.remove(&item.message_id);
            // 世代对不上 = 这条属于上一个账号，丢弃（见 ThumbnailBackfillItem）。
            if item.session_epoch != self.session_epoch {
                continue;
            }
            let ticket = match Self::extract_thumbnail_file_id(&item.extra) {
                Some(tid) => self.resolve_thumbnail_ticket(tid).await,
                None => None,
            };
            Self::spawn_auto_download_thumbnail(
                owner_uid.clone(),
                &item.extra,
                ticket,
                &user_root,
                item.message_id,
                item.created_at_ms,
                item.channel_id,
                item.channel_type,
                self.storage.clone(),
                self.event_tx.clone(),
                self.event_history.clone(),
                self.event_seq.clone(),
                self.event_history_limit,
            );
        }
    }

    /// 处理一批排队的 repair。由 actor tick 调用，每次最多 [`REPAIR_BATCH_LIMIT`] 条。
    ///
    /// 修好后**只发一次** TimelineUpdated：投影是原地更新的，message.id 不变、
    /// 未读不动、cursor 不动 —— repair 不是「收到新消息」。
    async fn drain_projection_repairs(&mut self) {
        for _ in 0..REPAIR_BATCH_LIMIT {
            let Some(key) = self.repair_queue.pop_front() else {
                return;
            };
            let (channel_type, channel_id, server_message_id) = key;
            let outcome = tokio::time::timeout(
                Duration::from_millis(REPAIR_TIMEOUT_MS),
                self.repair_message_projection(channel_id, channel_type, server_message_id),
            )
            .await;
            self.repair_seen.remove(&key);

            match outcome {
                Ok(Ok(Some(message_id))) => {
                    self.repair_backoff.remove(&key);
                    self.invalidate_channel_cache_with_reason(
                        channel_id,
                        channel_type,
                        "repair_message_projection",
                    );
                    if let (Some(tx), Some(history), Some(seq)) = (
                        self.event_tx.as_ref(),
                        self.event_history.as_ref(),
                        self.event_seq.as_ref(),
                    ) {
                        emit_sequenced_event(
                            tx,
                            history,
                            seq,
                            self.event_history_limit,
                            SdkEvent::TimelineUpdated {
                                channel_id,
                                channel_type,
                                message_id,
                                reason: "message_projection_repaired".to_string(),
                            },
                        );
                    }
                }
                // 服务端也没有这条：不是可重试失败，别再排它。
                Ok(Ok(None)) => {
                    self.repair_backoff.remove(&key);
                }
                Ok(Err(e)) => self.note_repair_failure(key, &e.to_string()),
                Err(_) => self.note_repair_failure(key, "repair timed out"),
            }
        }
    }

    fn note_repair_failure(&mut self, key: (i32, u64, u64), reason: &str) {
        let attempts = self.repair_backoff.get(&key).map(|(n, _)| *n).unwrap_or(0) + 1;
        let delay =
            REPAIR_BACKOFF_BASE_MS.saturating_mul(1u64 << attempts.min(REPAIR_BACKOFF_MAX_SHIFT));
        self.repair_backoff.insert(
            key,
            (
                attempts,
                std::time::Instant::now() + Duration::from_millis(delay),
            ),
        );
        tracing::warn!(
            server_message_id = key.2,
            channel_id = key.1,
            attempts,
            delay_ms = delay,
            reason,
            "投影 repair 失败,退避后由下一次读取重新发现"
        );
    }

    /// 按 `server_message_id` 定向修复一条投影损坏的消息。
    ///
    /// 「投影损坏」指本地这一行是某条旧代码或半截写入留下的:metadata 丢了(图片
    /// 永远加载不出来)、时间戳单位错了、pts 缺失。这类行不该靠删掉整段会话历史
    /// 再全量重拉来救——那会连带丢掉本地已下载的媒体、已读位置和 gap 水位,代价
    /// 远大于问题本身,而且用户会看到会话「闪空」。
    ///
    /// 修复路径就是一条:按 server_message_id 走 around 拿回权威消息 → 重新跑
    /// canonical projection → 原地 upsert(`server_message_id` 唯一,天然幂等)。
    ///
    /// 返回本地 message id;消息在服务端也不存在时返回 None。
    pub async fn repair_message_projection(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) -> Result<Option<u64>> {
        let normalized_channel_type = if channel_type == 0 { 1 } else { channel_type };
        let resp = self
            .fetch_and_store_messages_around(
                channel_id,
                normalized_channel_type,
                server_message_id,
                Some(0),
                Some(0),
            )
            .await?;
        if resp.anchor_message.message_id != server_message_id {
            tracing::warn!(
                server_message_id,
                channel_id,
                "repair: 服务端没有返回这条 anchor,跳过"
            );
            return Ok(None);
        }
        // fetch_and_store_messages_around 内部已按 canonical projection 落库,
        // 这里只把本地 id 查回来给调用方。
        Ok(self
            .storage
            .get_message_id_by_server_message_id(
                channel_id,
                normalized_channel_type,
                server_message_id,
            )
            .await?)
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
            let message_id = self
                .storage
                .upsert_remote_message_with_result(Self::history_item_to_upsert_input(
                    &item,
                    normalized_channel_type,
                ))
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

    async fn defer_canonical_mutation_if_target_missing(
        &self,
        commit: &ServerCommit,
    ) -> Result<bool> {
        let resolution = commit.resolve_canonical_event();
        let Some(event) = resolution.event else {
            return Ok(false);
        };
        let target_server_message_id = match &event {
            CanonicalTimelineEvent::Revoke(event) => event.target_server_message_id,
            CanonicalTimelineEvent::ReactionChange(event) => event.target_server_message_id,
            CanonicalTimelineEvent::NewMessage(_) => return Ok(false),
        };
        if self
            .storage
            .get_message_id_by_server_message_id(
                commit.channel_id,
                i32::from(commit.channel_type),
                target_server_message_id,
            )
            .await?
            .is_some()
        {
            return Ok(false);
        }
        let Some(event_id) = commit.event_id else {
            tracing::warn!(
                channel_id = commit.channel_id,
                target_server_message_id,
                "cannot persist out-of-order legacy mutation without event_id"
            );
            return Ok(false);
        };
        self.storage
            .put_pending_timeline_mutation(PendingTimelineMutation {
                channel_id: commit.channel_id,
                channel_type: i32::from(commit.channel_type),
                target_server_message_id,
                event_id,
                pts: commit.pts,
                canonical_event: event.encode_fb().map_err(|e| {
                    Error::Serialization(format!("encode pending canonical mutation: {e}"))
                })?,
            })
            .await?;
        Ok(true)
    }

    async fn replay_pending_timeline_mutations(
        &mut self,
        channel_id: u64,
        channel_type: i32,
        target_server_message_id: u64,
        bump_unread_on_incoming: bool,
    ) -> Result<usize> {
        let pending = self
            .storage
            .list_pending_timeline_mutations(channel_id, channel_type, target_server_message_id)
            .await?;
        let mut applied = 0usize;
        for mutation in pending {
            let event = CanonicalTimelineEvent::decode_fb(&mutation.canonical_event)
                .map_err(|e| Error::Serialization(format!("decode pending mutation: {e}")))?;
            let sender_id = match &event {
                CanonicalTimelineEvent::Revoke(event) => event.revoked_by,
                CanonicalTimelineEvent::ReactionChange(event) => event.actor_id,
                CanonicalTimelineEvent::NewMessage(_) => 0,
            };
            let commit = ServerCommit {
                event_id: Some(mutation.event_id),
                pts: mutation.pts,
                server_msg_id: mutation.event_id,
                local_message_id: None,
                channel_id,
                channel_type: u8::try_from(channel_type).unwrap_or(1),
                message_type: String::new(),
                content: serde_json::Value::Null,
                server_timestamp: 0,
                sender_id,
                sender_info: None,
                event_schema_version: Some(privchat_protocol::CANONICAL_TIMELINE_EVENT_SCHEMA_V1),
                canonical_event: Some(mutation.canonical_event.clone()),
            };
            let (entity_type, item) = Self::sync_item_from_difference_commit(&commit);
            let scope = Some(format!("{channel_type}:{channel_id}"));
            let count = self
                .enqueue_and_apply_sync_items(
                    entity_type.clone(),
                    scope.clone(),
                    vec![item],
                    bump_unread_on_incoming,
                )
                .await?;
            self.storage
                .delete_pending_timeline_mutation(mutation)
                .await?;
            self.queue_last_sync_events(entity_type, scope, count);
            applied += count;
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
                if self
                    .defer_canonical_mutation_if_target_missing(commit)
                    .await?
                {
                    continue;
                }
                let (entity_type, item) = Self::sync_item_from_difference_commit(commit);
                let materializes_message = entity_type == "message" && !item.deleted;
                let applied = self
                    .enqueue_and_apply_sync_items(
                        entity_type.clone(),
                        scope.clone(),
                        vec![item],
                        false,
                    )
                    .await?;
                total_applied += applied;
                self.queue_last_sync_events(entity_type, scope.clone(), applied);
                if materializes_message {
                    total_applied += self
                        .replay_pending_timeline_mutations(
                            commit.channel_id,
                            i32::from(commit.channel_type),
                            commit.server_msg_id,
                            false,
                        )
                        .await?;
                }
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
                peer_user_id: channel.peer_user_id,
            })
            .await?;
        Ok(())
    }

    /// Bounded repair for pushes lost while the transport still looked healthy.
    /// 一页 anti-entropy 的结果。
    ///
    /// 只返回「修了几条」是不够的：0 既可能是「这一页没东西修」，也可能是
    /// 「整个账号都收敛了」，两者语义天差地别。据此判定 Converged 会在扫到
    /// 第一页干净数据时就过早宣布收敛完成。
    async fn run_anti_entropy_once(&mut self) -> Result<AntiEntropyPage> {
        const PAGE_SIZE: usize = 100;
        const WIFI_DIFFERENCE_BUDGET: usize = 8;
        const CELLULAR_DIFFERENCE_BUDGET: usize = 4;

        if self.session_state != SessionState::Authenticated || !self.bootstrap_completed {
            return Ok(AntiEntropyPage::idle());
        }
        let (after_channel_id, after_channel_type) = self.load_anti_entropy_cursor().await;
        // 从头开始扫的这一轮，是否已经走完一整圈
        let mut cycle_completed = false;
        let mut channels = self
            .storage
            .list_channel_identifiers_after(after_channel_id, after_channel_type, PAGE_SIZE)
            .await?;
        if channels.is_empty() && after_channel_id != 0 {
            // 游标走到末尾又绕回开头 = 完整扫过一圈
            cycle_completed = true;
            self.save_anti_entropy_cursor(0, -1).await?;
            channels = self
                .storage
                .list_channel_identifiers_after(0, -1, PAGE_SIZE)
                .await?;
        }
        if channels.is_empty() {
            // 一个频道都没有：也算走完一圈
            return Ok(AntiEntropyPage {
                page_scanned: 0,
                stale_found: 0,
                channels_repaired: 0,
                messages_applied: 0,
                deferred: 0,
                unknown_channels: 0,
                cycle_completed: true,
            });
        }

        let remote = self.batch_get_channel_pts(channels.clone()).await?;
        let remote_pts: HashMap<(u64, i32), u64> = remote
            .channel_pts_map
            .into_iter()
            .map(|row| {
                (
                    (row.channel_id, i32::from(row.channel_type)),
                    row.current_pts,
                )
            })
            .collect();
        let budget = if self.network_hint == NetworkHint::Cellular {
            CELLULAR_DIFFERENCE_BUDGET
        } else {
            WIFI_DIFFERENCE_BUDGET
        };
        let mut observations = Vec::with_capacity(channels.len());
        for (channel_id, channel_type) in channels.iter().copied() {
            let materialized_pts = self
                .storage
                .max_message_pts(channel_id, channel_type)
                .await?;
            let local_pts = self
                .load_resume_channel_pts(channel_id, channel_type)
                .await
                .unwrap_or(0)
                .max(materialized_pts);
            observations.push(AntiEntropyObservation {
                key: (channel_id, channel_type),
                local_pts,
                server_pts: remote_pts.get(&(channel_id, channel_type)).copied(),
            });
        }
        let plan = plan_anti_entropy_page(&observations, budget);
        let mut messages_applied = 0usize;
        for (channel_id, channel_type) in plan.repair.iter().copied() {
            messages_applied += self
                .resume_channel_difference(channel_id, channel_type)
                .await?;
        }
        let channels_repaired = plan.repair.len();

        if plan.deferred > 0 {
            if let Some((channel_id, channel_type)) = plan.last_consumed {
                self.save_anti_entropy_cursor(channel_id, channel_type)
                    .await?;
            }
        } else if channels.len() < PAGE_SIZE {
            // 已消费末页，当前周期完整结束。
            cycle_completed = true;
            self.save_anti_entropy_cursor(0, -1).await?;
        } else if let Some((channel_id, channel_type)) = channels.last().copied() {
            self.save_anti_entropy_cursor(channel_id, channel_type)
                .await?;
        }
        tracing::debug!(
            scanned = plan.consumed,
            difference_calls = channels_repaired,
            channels_repaired,
            messages_applied,
            stale_found = plan.stale_found,
            unknown_channels = plan.unknown_channels,
            deferred = plan.deferred,
            cycle_completed,
            "anti-entropy channel page completed"
        );
        Ok(AntiEntropyPage {
            page_scanned: plan.consumed,
            stale_found: plan.stale_found,
            unknown_channels: plan.unknown_channels,
            channels_repaired,
            messages_applied,
            deferred: plan.deferred,
            cycle_completed,
        })
    }

    async fn execute_resume_sync(&mut self) -> Result<()> {
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
        // P0-12 单轮化：同一连接世代内一轮成功后，30s 内的重复触发直接跳过。
        // 冷启动（connect + bootstrap 双入口）和重连（retry driver / connect /
        // cmd_authenticate 三入口）都会连续触发多轮全家桶；连接期间的增量由
        // push 实时覆盖，重复轮次只放大服务端压力。debounce 而非严格单次：
        // 前台回归等超过窗口的触发仍作为安全网执行。
        if let Some((epoch, at)) = self.last_resume_synced {
            if epoch == self.inbound_epoch && at.elapsed() < Duration::from_secs(30) {
                eprintln!(
                    "[SDK.actor] resume sync skipped: epoch {} already synced {}ms ago",
                    epoch,
                    at.elapsed().as_millis()
                );
                return Ok(());
            }
        }
        let mut stats = ResumeRunStats::default();
        self.resume_run_id = self.resume_run_id.wrapping_add(1);
        let run_id = self.resume_run_id;
        println!("[SDK.resume] run={run_id} phase=2 begin");
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

        // ── Phase 2 到此为止（spec SDK_SYNC_RESUME_SPEC §Startup Phases）──
        //
        // 上面几个实体族是**自包含列表投影**：会话/好友/群列表带着渲染该行所需的
        // 名称、头像、类型，请求数固定为实体族个数，与账号大小无关。到这里主界面
        // 就能正确显示了，立刻宣布 CriticalReady 让用户可用。
        //
        // 原先排在这之后的两段被移出关键路径：
        //   1. 逐群 sync_entities("group_member", group_id) —— 请求数 O(群数)
        //   2. 逐频道 resume_channel_difference        —— 请求数 O(频道数 × 分页)
        // 它们让上线时延随账号大小线性增长（实测百来个会话就要挂几分钟），
        // 现在交给 Phase 3 后台收敛：先 batch_get_channel_pts 批量比对，只修 stale。
        // Phase 2 完成的信号就是 readiness → Ready：ensure_synced 收尾时
        // `sync_coordinator.complete()` 会置位并发 SyncStateChanged，宿主据此撤横幅。
        // 不另造 CriticalReady 事件——uniffi 绑定是手工维护的，多一个变体就要同步
        // 三个平台的绑定文件，而 readiness 已经把这件事说清楚了。
        //
        // 旧 `ResumeSyncCompleted` 保持原义：在 Phase 3 收敛完成时才发（见 repair_tick）。
        //
        // 开启 Phase 3。这里只置标志，实际收敛由 actor loop 的 repair_tick 驱动，
        // 一轮修一小批（有界预算），不阻塞命令处理，也不霸占 actor。
        // println! 而非 eprintln!：Android 的 logcat 收不到 stderr，
        // 而这些打点的用途正是生产排查。
        println!(
            "[SDK.resume] run={run_id} phase=2 done entity_types={} (critical path complete)",
            stats.entity_types_synced
        );
        // stats 交给 Phase 3：ResumeSyncCompleted 只在真正全量收敛后发一次。
        // 在这里发过一次是漏删——那会让宿主以为整轮结束，而收敛才刚开始。
        self.convergence_run = Some(stats);
        self.sync_coordinator.set_convergence(
            crate::sync_coordinator::Convergence::Scanning,
            chrono::Utc::now().timestamp_millis(),
        );
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

    /// [bump_unread_on_incoming]:仅真正的实时推送传 true。resume/difference 回填的
    /// 历史消息必须传 false——它们已包含在 channel 冷启动同步的服务端未读基线里,
    /// 再按实时消息 bump 会把未读恰好双算(2026-07-24 事故:系统消息 11→22、DM 1→2)。
    async fn enqueue_and_apply_sync_items(
        &mut self,
        entity_type: String,
        scope: Option<String>,
        items: Vec<SyncEntityItem>,
        bump_unread_on_incoming: bool,
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
                    bump_unread_on_incoming,
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

    /// P1-05：room 广播去重。返回 true 表示这条 (channel_id, server_message_id) 最近
    /// 已见过（应丢弃）。None id 一律放行（无法去重）。每 channel 有界 FIFO（256）。
    fn room_message_is_duplicate(
        &mut self,
        channel_id: u64,
        server_message_id: Option<u64>,
    ) -> bool {
        const ROOM_DEDUP_WINDOW: usize = 256;
        let Some(id) = server_message_id else {
            return false;
        };
        let seen = self.room_seen_msg_ids.entry(channel_id).or_default();
        if seen.contains(&id) {
            return true;
        }
        seen.push_back(id);
        if seen.len() > ROOM_DEDUP_WINDOW {
            seen.pop_front();
        }
        false
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
                    if realtime_trace_enabled() {
                        eprintln!(
                        "[SDK_INBOUND_PRELOGIN_BUFFER] biz_type={} current_uid=None queue_len={} (push 帧卡在登录前缓冲；若不 replay 即收不到)",
                        biz_type,
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
        let mut direct_applied = 0usize;
        // (channel_id, channel_type, server_message_id, delivered_at)
        let mut delivery_receipts: Vec<(u64, i32, u64, u64)> = Vec::new();
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
                if self.skip_inbound_materialization_for_load_testing {
                    return Ok(0);
                }
                if realtime_trace_enabled() {
                    eprintln!(
                    "[SDK_MESSAGE_EVENT_DECODED] biz=Push channel_id={} server_message_id={} from_uid={} msg_type={} payload_len={}",
                    req.channel_id, req.server_message_id, req.from_uid, req.message_type, req.payload.len()
                );
                }
                if let Some(count) = self.apply_entity_invalidation_push(&req).await? {
                    direct_applied += count;
                } else if let Some(count) = self.apply_canonical_timeline_push(&req).await? {
                    direct_applied += count;
                } else if let Some(peer_uid) = Self::push_message_to_friend_event(&req) {
                    // F-sync.2: 转 entity_type="friend"，让 SDK 走 entity sync
                    // 把 pending/rejected/recalled 状态从 server 拉到本地 friend 表。
                    self.pending_events.push(SdkEvent::SyncEntityChanged {
                        entity_type: "friend".to_string(),
                        entity_id: peer_uid.to_string(),
                        deleted: false,
                    });
                } else if let Some(status_item) = Self::push_message_to_status_sync_item(&req) {
                    read_cursor_items.push(status_item);
                } else if let Some(receipt) = Self::push_message_to_delivery_receipt(&req) {
                    delivery_receipts.push(receipt);
                } else {
                    // deleted=true 时 push_message_to_sync_item 会透传给 SyncEntityItem.deleted，
                    // 进而触发 "message" 实体处理里的 set_message_revoke 路径
                    message_items.push(Self::push_message_to_sync_item(req));
                }
            }
            MessageType::PushBatchRequest => {
                let req: PushBatchRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode push batch: {e}")))?;
                if self.skip_inbound_materialization_for_load_testing {
                    return Ok(0);
                }
                for push in req.messages {
                    if let Some(count) = self.apply_entity_invalidation_push(&push).await? {
                        direct_applied += count;
                    } else if let Some(count) = self.apply_canonical_timeline_push(&push).await? {
                        direct_applied += count;
                    } else if let Some(peer_uid) = Self::push_message_to_friend_event(&push) {
                        self.pending_events.push(SdkEvent::SyncEntityChanged {
                            entity_type: "friend".to_string(),
                            entity_id: peer_uid.to_string(),
                            deleted: false,
                        });
                    } else if let Some(status_item) = Self::push_message_to_status_sync_item(&push)
                    {
                        read_cursor_items.push(status_item);
                    } else if let Some(receipt) = Self::push_message_to_delivery_receipt(&push) {
                        delivery_receipts.push(receipt);
                    } else {
                        message_items.push(Self::push_message_to_sync_item(push));
                    }
                }
            }
            MessageType::PublishRequest => {
                let req: PublishRequest = decode_message(&data)
                    .map_err(|e| Error::Serialization(format!("decode publish request: {e}")))?;
                if let Ok(push) = decode_message::<PushMessageRequest>(&req.payload) {
                    if self.skip_inbound_materialization_for_load_testing {
                        return Ok(0);
                    }
                    // IM 场景：payload 是 PushMessageRequest（私聊/群聊消息走 sync pipeline）
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] publish payload decoded as PushMessageRequest channel_id={}",
                            push.channel_id
                        );
                    }
                    if let Some(count) = self.apply_entity_invalidation_push(&push).await? {
                        direct_applied += count;
                    } else if let Some(count) = self.apply_canonical_timeline_push(&push).await? {
                        direct_applied += count;
                    } else if let Some(peer_uid) = Self::push_message_to_friend_event(&push) {
                        self.pending_events.push(SdkEvent::SyncEntityChanged {
                            entity_type: "friend".to_string(),
                            entity_id: peer_uid.to_string(),
                            deleted: false,
                        });
                    } else if let Some(status_item) = Self::push_message_to_status_sync_item(&push)
                    {
                        read_cursor_items.push(status_item);
                    } else if let Some(receipt) = Self::push_message_to_delivery_receipt(&push) {
                        delivery_receipts.push(receipt);
                    } else {
                        message_items.push(Self::push_message_to_sync_item(push));
                    }
                } else if let Ok(batch) = decode_message::<PushBatchRequest>(&req.payload) {
                    if self.skip_inbound_materialization_for_load_testing {
                        return Ok(0);
                    }
                    if inbound_logs_enabled() {
                        eprintln!(
                            "[SDK.inbound] publish payload decoded as PushBatchRequest count={}",
                            batch.messages.len()
                        );
                    }
                    for push in batch.messages {
                        if let Some(count) = self.apply_entity_invalidation_push(&push).await? {
                            direct_applied += count;
                        } else if let Some(count) =
                            self.apply_canonical_timeline_push(&push).await?
                        {
                            direct_applied += count;
                        } else if let Some(peer_uid) = Self::push_message_to_friend_event(&push) {
                            self.pending_events.push(SdkEvent::SyncEntityChanged {
                                entity_type: "friend".to_string(),
                                entity_id: peer_uid.to_string(),
                                deleted: false,
                            });
                        } else if let Some(status_item) =
                            Self::push_message_to_status_sync_item(&push)
                        {
                            read_cursor_items.push(status_item);
                        } else if let Some(receipt) = Self::push_message_to_delivery_receipt(&push)
                        {
                            delivery_receipts.push(receipt);
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
                    // P1-05：去重 replay/live 重叠窗口的重复帧。presence_changed 是无 id
                    // 的状态帧，不参与去重（每次都应用最新态）。
                    if req.topic.as_deref() == Some("presence_changed") {
                        self.apply_presence_changed_payload(&req.payload);
                    } else if self.room_message_is_duplicate(req.channel_id, req.server_message_id)
                    {
                        if inbound_logs_enabled() {
                            eprintln!(
                                "[SDK.inbound] drop duplicate room publish channel_id={} server_message_id={:?}",
                                req.channel_id, req.server_message_id
                            );
                        }
                        return Ok(0);
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

        let mut applied = direct_applied;
        if !message_items.is_empty() {
            let n = message_items.len();
            let before = applied;
            applied += self
                .enqueue_and_apply_sync_items("message".to_string(), None, message_items, true)
                .await?;
            if realtime_trace_enabled() {
                eprintln!(
                    "[SDK_LOCAL_STORE_APPLY] message_items={} applied={} uid={:?}",
                    n,
                    applied - before,
                    self.current_uid
                );
            }
        }
        if !read_cursor_items.is_empty() {
            applied += self
                .enqueue_and_apply_sync_items(
                    "channel_read_cursor".to_string(),
                    None,
                    read_cursor_items,
                    true,
                )
                .await?;
        }
        // Process delivery receipts: persist + emit events
        for (channel_id, channel_type, server_message_id, delivered_at) in delivery_receipts {
            let local_message_id = self
                .storage
                .mark_message_delivered(server_message_id, delivered_at)
                .await
                .unwrap_or(None);
            if let Some(message_id) = local_message_id {
                self.pending_events.push(SdkEvent::MessageDelivered {
                    channel_id,
                    channel_type,
                    message_id,
                    server_message_id,
                    delivered_at,
                });
                if inbound_logs_enabled() {
                    eprintln!(
                        "[SDK.delivered] message_delivered: channel_id={} message_id={} server_message_id={} delivered_at={}",
                        channel_id, message_id, server_message_id, delivered_at
                    );
                }
            }
        }
        Ok(applied)
    }

    async fn replay_prelogin_inbound_frames(&mut self) -> Result<usize> {
        if self.current_uid.is_none() || self.pending_prelogin_inbound_frames.is_empty() {
            return Ok(0);
        }
        let pending = std::mem::take(&mut self.pending_prelogin_inbound_frames);
        let count = pending.len();
        let mut applied = 0usize;
        for (biz_type, data) in pending {
            applied += self.handle_inbound_frame(biz_type, data).await?;
        }
        if realtime_trace_enabled() {
            eprintln!(
                "[SDK_INBOUND_PRELOGIN_REPLAY] count={} applied={} current_uid={:?}",
                count, applied, self.current_uid
            );
        }
        Ok(applied)
    }

    async fn execute_bootstrap_sync(&mut self) -> Result<()> {
        if self.session_state != SessionState::Authenticated {
            return Err(Error::InvalidState(
                "run_bootstrap_sync requires authenticated state".to_string(),
            ));
        }

        if self.bootstrap_completed && !self.full_rebuild_required().await {
            match self.execute_resume_sync().await {
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

    /// 有没有「已请求但还没被 actor 处理」的账号切换。
    fn switch_is_pending(&self) -> bool {
        self.switch_requested.load(Ordering::SeqCst) > self.switch_processed.load(Ordering::SeqCst)
    }

    /// 等到有账号切换排队为止。
    ///
    /// 先注册唤醒、再读计数器：顺序反了就会漏掉「读完之后、注册之前」那一瞬间到达的
    /// 请求——正是裸 `notify_waiters()` 丢信号的那个窗口。注册在前，那一声必然落在
    /// 已注册的等待者身上；而即使它仍然落空，循环回来重读计数器也能看到事实。
    async fn wait_for_switch_request(
        requested: Arc<AtomicU64>,
        processed: Arc<AtomicU64>,
        wakeup: Arc<tokio::sync::Notify>,
    ) {
        loop {
            let notified = wakeup.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if requested.load(Ordering::SeqCst) > processed.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    /// 跑一轮同步（如果闸门允许）。
    ///
    /// 返回值是**这一轮到底跑没跑**。以前它返回 `Result<()>`，被闸门挡掉时也是 Ok——
    /// 调用方无从分辨「同步完成了」和「什么都没发生」，于是 `run_bootstrap_sync` 会
    /// 在 bootstrap 根本没跑的情况下报成功，接着每一个 local-first 操作都失败。
    ///
    /// `explicit` = 宿主主动要求（不是自动触发）。它能解除退避窗口：退避压的是自动
    /// 重试的空转，不该把用户点下的那一次也一起吞掉。
    async fn ensure_synced_inner<F>(&mut self, explicit: bool, mut emit: F) -> Result<bool>
    where
        F: FnMut(SdkEvent),
    {
        if self.session_state != SessionState::Authenticated {
            return Err(Error::InvalidState(
                "ensure_synced requires authenticated state".to_string(),
            ));
        }

        // 已经有切换在排队就根本别开这一轮：开了也是马上让出，白白把闸门开关一遍。
        if self.switch_is_pending() {
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] ensure_synced skipped: an account switch is pending");
            }
            return Ok(false);
        }

        let kind = if self.bootstrap_completed && !self.full_rebuild_required().await {
            SyncRunKind::Resume
        } else {
            SyncRunKind::Bootstrap
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        if explicit {
            // 宿主主动要的这一次不该被上一次失败排下的退避窗口吞掉。
            self.sync_coordinator.note_explicit_request();
        }
        // 退避/合并/终态都在协调器里判定。sync 有多个触发源（重连成功、connect
        // 成功、token 刷新后重认证、显式命令），它们只有共同经过这一道闸门才收
        // 敛得住——在任何单个调用点 sleep 都会被下一个触发源绕开。
        if let Err(rejection) = self.sync_coordinator.begin(kind, now_ms) {
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] ensure_synced skipped: {rejection:?}");
            }
            return Ok(false);
        }
        // 这一轮属于哪个账号/会话世代。切账号会 bump 它。
        let generation = self.sync_coordinator.generation();
        emit(SdkEvent::SyncStateChanged {
            state: self.sync_coordinator.snapshot(),
        });

        // 同步跑起来，同时盯着「有账号切换在排队」这个铃。actor 在这里内联 await，
        // 期间不处理命令——不盯着的话，一次卡住的同步会把切换一起堵死，用户点了
        // 切换要等十几秒才有反应。铃响就让出：这一轮的结果已经不属于任何人了。
        let result = tokio::select! {
            biased;
            _ = Self::wait_for_switch_request(
                self.switch_requested.clone(),
                self.switch_processed.clone(),
                self.switch_wakeup.clone(),
            ) => None,
            r = async {
                #[cfg(test)]
                if let Some(stall) = self.sync_stall_for_test {
                    tokio::time::sleep(stall).await;
                }
                match kind {
                    SyncRunKind::Bootstrap => self.execute_bootstrap_sync().await,
                    SyncRunKind::Resume => self.execute_resume_sync().await,
                }
            } => Some(r),
        };

        let Some(result) = result else {
            // 主动放弃，不是失败：不能记 attempt/退避（那会让新账号背上上一个账号的
            // 债），但必须把闸门撤下来，否则 begin() 会永远挡住后面每一个触发源。
            self.sync_coordinator
                .abandon(chrono::Utc::now().timestamp_millis());
            // 后台收敛也必须停：那一轮属于正被切走的账号，继续跑会把上一个账号的
            // 频道差异写进新账号的会话里。
            self.convergence_run = None;
            if actor_logs_enabled() {
                eprintln!("[SDK.actor] ensure_synced yielded to a pending account switch");
            }
            return Ok(false);
        };

        // 跑的过程中账号被切走了：这一轮的结果属于**上一个** owner，既不能写进
        // 协调器（会污染新账号的 attempt/退避/终态），也不能发事件（UI 会拿旧账号
        // 的同步结果去更新新账号）。静默丢弃，新账号自己的那一轮会照常跑。
        //
        // 正常情况下上面的 select 已经先一步让出（切换 API 投命令前会按铃），所以这里
        // 兜的是漏网的那一格：铃响时这一轮恰好还没开始 await（notify_waiters 只唤醒
        // **已经在等**的人，早响的一声会丢），同步照跑完，回来时世代已经变了。
        // 那时这份结果属于上一个 owner，写进协调器会污染新账号的 attempt/退避，
        // 发事件会让 UI 拿旧账号的同步结果去更新新账号。丢弃即可，新账号自己会跑。
        if self.sync_coordinator.generation() != generation {
            if actor_logs_enabled() {
                eprintln!(
                    "[SDK.actor] ensure_synced result dropped: generation {generation} -> {} (account switched mid-flight)",
                    self.sync_coordinator.generation()
                );
            }
            return Ok(false);
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        match &result {
            Ok(()) => self.sync_coordinator.complete(kind, now_ms),
            Err(error) => self.sync_coordinator.fail(
                kind,
                error.is_auth_terminal(),
                Some(error.protocol_code()),
                error.to_string(),
                now_ms,
            ),
        }
        emit(SdkEvent::SyncStateChanged {
            state: self.sync_coordinator.snapshot(),
        });
        result.map(|()| true)
    }

    /// 自动触发的同步（重连成功 / connect / token 刷新后重认证）。
    async fn ensure_synced<F>(&mut self, emit: F) -> Result<()>
    where
        F: FnMut(SdkEvent),
    {
        self.ensure_synced_inner(false, emit).await.map(|_| ())
    }

    /// 宿主显式要求的同步。返回「是否真的跑了」——调用方要靠它区分
    /// 「同步完成」和「被闸门挡住、什么都没发生」。
    async fn ensure_synced_explicit<F>(&mut self, emit: F) -> Result<bool>
    where
        F: FnMut(SdkEvent),
    {
        self.ensure_synced_inner(true, emit).await
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
                let message_id = self
                    .storage
                    .upsert_remote_message_with_result(Self::history_item_to_upsert_input(
                        &item,
                        normalized_channel_type,
                    ))
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
            body: serde_json::to_vec(&req).map_err(|e| {
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
        let response: PresenceBatchStatusResponse = serde_json::from_slice(&body).map_err(|e| {
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
            body: serde_json::to_vec(&req)
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
            param: token.clone().unwrap_or_default(),
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
        // 记入 desired-subscription 注册表，供重连后 replay（presence/typing 恢复的基础）。
        self.active_subscriptions
            .insert((channel_id, channel_type), token);
        Ok(())
    }

    /// Channel Transfer client→app RPC. See `02-server/CHANNEL_TRANSFER_SPEC.md`.
    /// `request_id` is generated locally (snowflake → string) and matched at the
    /// transport layer; the wire `TransferResponse` is decoded into `TransferReply`.
    async fn transfer_channel(
        &mut self,
        channel_id: u64,
        route: String,
        body: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<TransferReply> {
        let request_id = self
            .snowflake
            .next_id()
            .map_err(|e| Error::Storage(format!("generate transfer request_id failed: {e:?}")))?
            .to_string();
        let req = TransferRequest {
            request_id,
            channel_id,
            route: route.clone(),
            body,
        };
        let payload = encode_message(&req)
            .map_err(|e| Error::Serialization(format!("encode transfer_channel: {e}")))?;
        let timeout = Duration::from_millis(timeout_ms.max(1));
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::TransferRequest as u8,
                timeout,
                "transfer_channel",
            )
            .await?;
        let resp: TransferResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode transfer_channel resp: {e}")))?;
        Ok(TransferReply {
            request_id: resp.request_id,
            channel_id: resp.channel_id,
            code: resp.code,
            message: resp.message,
            data: resp.data.unwrap_or_default(),
        })
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
        // 从 desired-subscription 注册表移除：重连后不再 replay 该频道。
        self.active_subscriptions
            .remove(&(channel_id, channel_type));
        Ok(())
    }

    /// 重连后重放所有活跃订阅（desired subscriptions）。服务端 subscribe_manager 随旧会话清空，
    /// 不重放则 presence_changed / typing / room 广播全断。best-effort：单个失败不影响其它。
    /// 注意直接发 SubscribeRequest，不复用 subscribe_channel（避免借用 active_subscriptions 同时改它）。
    async fn replay_subscriptions(&mut self) {
        if self.active_subscriptions.is_empty() {
            return;
        }
        let subs: Vec<((u64, u8), Option<String>)> = self
            .active_subscriptions
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        if realtime_trace_enabled() {
            eprintln!(
                "[SDK_RESUBSCRIBE] replaying {} active subscriptions after reconnect",
                subs.len()
            );
        }
        let timeout = self.timeout();
        for ((channel_id, channel_type), token) in subs {
            let req = SubscribeRequest {
                setting: 0,
                local_message_id: 0,
                channel_id,
                channel_type,
                action: 1, // SUBSCRIBE
                param: token.unwrap_or_default(),
            };
            let Ok(payload) = encode_message(&req) else {
                continue;
            };
            if let Err(e) = self
                .request_bytes(
                    Bytes::from(payload),
                    MessageType::SubscribeRequest as u8,
                    timeout,
                    "replay_subscribe",
                )
                .await
            {
                eprintln!(
                    "[SDK_RESUBSCRIBE] replay failed channel_id={} type={} err={}",
                    channel_id, channel_type, e
                );
            }
        }
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
        // Build envelope in legacy (Value-based) shape to keep the existing
        // flexible content-detection logic intact, then bridge to the typed
        // wire envelope at the FlatBuffers encode boundary below.
        let extra_envelope = Self::decode_legacy_message_envelope(&message.extra)
            .and_then(|value| serde_json::from_value::<LocalMessagePayloadEnvelope>(value).ok());
        let mut envelope = extra_envelope.unwrap_or_else(|| LocalMessagePayloadEnvelope {
            content: content.clone(),
            metadata: serde_json::from_str::<serde_json::Value>(&message.extra)
                .ok()
                .and_then(|value| if value.is_null() { None } else { Some(value) }),
            reply_to_message_id: None,
            mentioned_user_ids: None,
            message_source: None,
        });

        if let Some(content_json) = Self::decode_legacy_message_envelope(&content) {
            if let Ok(parsed_envelope) =
                serde_json::from_value::<LocalMessagePayloadEnvelope>(content_json)
            {
                envelope = parsed_envelope;
            }
        } else if let Ok(content_json) = serde_json::from_str::<serde_json::Value>(&content) {
            if content_json
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
                envelope.content =
                    Self::attachment_placeholder_text(message.message_type, file_type).to_string();
                envelope.metadata = Some(content_json);
            }
        }

        // Wire boundary: bridge the legacy Value-based envelope to the typed
        // FlatBuffers envelope and encode as binary.
        let content_type =
            ContentMessageType::from_u32(message_type).unwrap_or(ContentMessageType::Text);
        let typed_envelope = MessagePayloadEnvelope::from_legacy(&envelope, content_type);
        let payload = privchat_protocol::encode_message(&typed_envelope)
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
        let opt = RequestOptions::new()
            .biz_type(MessageType::SendMessageRequest as u8)
            .timeout(timeout);
        let raw = transport
            .request_with_options(Bytes::from(request_data), opt)
            .await
            .map_err(|e| self.handle_transport_request_error("direct send message", e))?;
        let resp: SendMessageResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode send response: {e}")))?;
        if resp.reason_code != 0 {
            return Err(Error::Server {
                code: resp.reason_code,
                message: format!("send message failed: reason_code={}", resp.reason_code),
            });
        }
        if resp.server_message_id == 0 {
            return Err(Error::Serialization(
                "send message response missing server_message_id".to_string(),
            ));
        }
        Ok(resp)
    }

    /// Extract mime_type from content or extra JSON.
    /// Tries content first, then extra. Looks for "mime_type" key in metadata or flat format.
    fn extract_mime_type_from_json(content: &str, extra: &str) -> Option<String> {
        for src in [content, extra] {
            if src.is_empty() {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(src) {
                // Envelope: {"metadata":{"mime_type":"image/jpeg"}}
                if let Some(mime) = json
                    .get("metadata")
                    .and_then(|m| m.get("mime_type"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    return Some(mime.to_string());
                }
                // Flat: {"mime_type":"image/jpeg"}
                if let Some(mime) = json
                    .get("mime_type")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    return Some(mime.to_string());
                }
            }
        }
        None
    }

    /// Extract thumbnail_url from message content JSON (supports envelope and flat formats).
    fn extract_thumbnail_url(content: &str) -> Option<String> {
        let json: serde_json::Value = serde_json::from_str(content).ok()?;
        // Envelope format: {"content":"...","metadata":{"thumbnail_url":"..."}}
        if let Some(url) = json
            .get("metadata")
            .and_then(|m| m.get("thumbnail_url"))
            .and_then(|v| v.as_str())
        {
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
        // Flat format: {"thumbnail_url":"..."}
        if let Some(url) = json.get("thumbnail_url").and_then(|v| v.as_str()) {
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
        None
    }

    /// 从消息内容 JSON 解析缩略图的协议权威 `thumbnail_file_id`（envelope `metadata` 或 flat）。
    /// Scheme B：接收端按此 file_id 走 `file/get_url` 拿 signed_url + cek 下载解密。
    fn extract_thumbnail_file_id(content: &str) -> Option<u64> {
        let json: serde_json::Value = serde_json::from_str(content).ok()?;
        let read = |scope: &serde_json::Value| -> Option<u64> {
            scope.get("thumbnail_file_id").and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
        };
        read(json.get("metadata").unwrap_or(&json))
            .or_else(|| read(&json))
            .filter(|id| *id > 0)
    }

    /// 解析缩略图下载票据：State 内（actor 上下文）按 `thumbnail_file_id` 调 `file/get_url`，
    /// 拿 signed_url + encryption_version + cek。失败返回 None（缩略图静默不下载，不阻塞）。
    /// CEK 只来自此处，绝不取自消息 metadata。
    async fn resolve_thumbnail_ticket(
        &mut self,
        thumbnail_file_id: u64,
    ) -> Option<ResolvedFileDownload> {
        let req = FileGetUrlRequest {
            file_id: thumbnail_file_id,
            user_id: 0,
        };
        let resp: FileGetUrlResponse = self
            .rpc_call_typed(routes::file::GET_URL, &req)
            .await
            .ok()?;
        if resp.file_url.trim().is_empty() {
            return None;
        }
        Some(ResolvedFileDownload {
            url: resp.file_url,
            encryption_version: resp.encryption_version,
            cek: resp.cek,
        })
    }

    /// Spawn a background task to download a thumbnail for an incoming message.
    ///
    /// Scheme B：缩略图也是独立 file。`ticket` 是 caller 用 `thumbnail_file_id` 经
    /// `file/get_url` 解析出的下载票据（v1 加密：url + encryption_version + cek）。
    /// 没有 `thumbnail_file_id` 时 `ticket=None`，退回消息里的 legacy 明文 `thumbnail_url`（v0）。
    /// **CEK 只来自 get_url ticket，绝不取自消息 metadata。**
    #[allow(clippy::too_many_arguments)]
    /// `owner_uid` = 这个下载属于哪个账号。下载是异步的，完成时账号可能已经切走——
    /// 那时写库和发事件都必须整条放弃，否则会拿旧账号的 message_id 写进新账号的库、
    /// 把旧账号的 timeline 事件发给新账号的界面。
    fn spawn_auto_download_thumbnail(
        owner_uid: String,
        content: &str,
        ticket: Option<ResolvedFileDownload>,
        user_root: &Path,
        message_id: u64,
        created_at_ms: i64,
        channel_id: u64,
        channel_type: i32,
        storage: crate::storage_actor::StorageHandle,
        event_tx: Option<broadcast::Sender<SdkEvent>>,
        event_history: Option<Arc<StdMutex<VecDeque<SequencedSdkEvent>>>>,
        event_seq: Option<Arc<AtomicU64>>,
        event_history_limit: usize,
    ) {
        let (thumb_url, thumb_enc_version, thumb_cek) = match ticket {
            // v1：get_url 解析的票据，密文 blob，用票据里的 cek 解密。
            Some(t) if t.url.starts_with("http") => (t.url, t.encryption_version, t.cek),
            // legacy：消息里只有明文 thumbnail_url（旧 v0 附件）。
            _ => match Self::extract_thumbnail_url(content) {
                Some(url) if url.starts_with("http") => (url, 0, None),
                _ => {
                    // thumb_status=3 是**终态**：写下去之后永不重试，UI 从此渲染
                    // 静态占位符。所以它只能表示「已经看清楚了,这条消息确实没有
                    // 缩略图」,不能表示「我没看到缩略图字段」。
                    //
                    // 这两者的区别就是上一次事故:history 回填丢了 metadata,extra
                    // 是空串,于是这里判定「没有缩略图」并永久标记——整段历史的图片
                    // 从此是灰块,重开 App 也不会好。
                    //
                    // 现在的规则:metadata 解析不出来时停在 0(未知/待重试),留给下
                    // 一次投影或定向 repair 去补;只有确实解析出了 metadata 而其中
                    // 没有缩略图字段,才允许进终态。
                    // 许可条件是「服务端明确说了没有缩略图」,不是「metadata 能解析」。
                    // 后者会漏掉最要命的一种:metadata 里有 thumbnail_file_id,只是这次
                    // file/get_url 因网络/token/服务端抖动没拿到票据——那是可重试失败,
                    // 写成终态就等于一次抖动永久毁掉一张图。
                    let explicit_absence =
                        crate::canonical_inbound::CanonicalInboundMessage::from_sync_entity(
                            0,
                            0,
                            channel_id,
                            channel_type,
                            0,
                            0,
                            String::new(),
                            content.to_string(),
                            0,
                            0,
                        )
                        .server_says_no_thumbnail();
                    if explicit_absence {
                        tokio::spawn(async move {
                            let _ = storage.update_thumb_status(message_id, 3).await;
                        });
                    } else {
                        tracing::warn!(
                            message_id,
                            channel_id,
                            "拿不到缩略图票据,但服务端未明确表示没有缩略图:保持待重试,不写终态"
                        );
                    }
                    return;
                }
            },
        };
        let dir = media_store::get_message_dir(user_root, message_id as i64, created_at_ms);
        let thumb_path = dir.join(Self::thumb_filename_for_url(&thumb_url));
        let webp_path = dir.join(media_store::THUMB_FILENAME);
        let png_path = dir.join(media_store::THUMB_PNG_FILENAME);
        if thumb_path.exists() || webp_path.exists() || png_path.exists() {
            let _ = tokio::spawn(async move {
                let _ = storage
                    .update_thumb_status_scoped(owner_uid.clone(), message_id, 1)
                    .await;
            });
            return;
        }
        tokio::spawn(async move {
            match Self::do_download_thumbnail(
                &thumb_url,
                &dir,
                &thumb_path,
                thumb_enc_version,
                thumb_cek.as_deref(),
            )
            .await
            {
                Ok(()) => {
                    // 账号已切走 → 不写库、也不发事件。
                    match storage
                        .update_thumb_status_scoped(owner_uid.clone(), message_id, 1)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => return,
                        Err(e) => {
                            eprintln!("[SDK.thumb] update thumb_status=1 failed: {e}");
                        }
                    }
                    // Notify clients that the thumbnail is ready so they can refresh
                    let event = SdkEvent::TimelineUpdated {
                        channel_id,
                        channel_type,
                        message_id,
                        reason: "thumbnail_ready".to_string(),
                    };
                    if let (Some(tx), Some(history), Some(seq)) =
                        (&event_tx, &event_history, &event_seq)
                    {
                        emit_sequenced_event(tx, history, seq, event_history_limit, event);
                    } else if let Some(tx) = &event_tx {
                        let _ = tx.send(event);
                    }
                }
                Err(e) => {
                    eprintln!(
                        "[SDK.thumb] auto-download failed message_id={} url={}: {}",
                        message_id, thumb_url, e
                    );
                    if let Err(e2) = storage
                        .update_thumb_status_scoped(owner_uid.clone(), message_id, 2)
                        .await
                    {
                        eprintln!("[SDK.thumb] update thumb_status=2 failed: {e2}");
                    }
                }
            }
        });
    }

    /// Pick canonical thumbnail filename based on the remote URL extension,
    /// so the file name on disk matches the actual bytes (webp vs png).
    fn thumb_filename_for_url(url: &str) -> &'static str {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        let ext = path
            .rsplit('/')
            .next()
            .and_then(|name| name.rsplit('.').next())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        match ext.as_str() {
            "png" => media_store::THUMB_PNG_FILENAME,
            _ => media_store::THUMB_FILENAME,
        }
    }

    async fn do_download_thumbnail(
        url: &str,
        dir: &Path,
        thumb_path: &Path,
        encryption_version: i32,
        cek: Option<&str>,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let resp = reqwest::Client::new().get(url).send().await?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()).into());
        }
        let bytes = resp.bytes().await?;
        // 附件加密 v1：缩略图 blob 同样是 nonce||ct||tag，用 file/get_url 票据里的 cek
        // 本地解密后再落盘；v0（legacy 明文）原样写入。解密失败直接报错，绝不写密文当图片。
        let plaintext = crate::attachment_crypto::decrypt_downloaded_attachment_bytes(
            encryption_version,
            cek,
            &bytes,
        )?;
        std::fs::create_dir_all(dir)?;
        std::fs::write(thumb_path, &plaintext)?;
        eprintln!(
            "[SDK.thumb] auto-download ok: {} ({} bytes, enc_v={})",
            thumb_path.display(),
            plaintext.len(),
            encryption_version
        );
        Ok(())
    }

    /// 存储层 / 上传层使用的文件分类字符串，严格对齐服务端 `FileType`：
    /// `"image"` / `"video"` / `"voice"` / `"file"`。
    ///
    /// 规则：发送入口（`message_type`）决定分类，MIME 不反推消息类型。
    /// Voice 消息直接返回 `"voice"`，服务端据此走独立的尺寸限额与 /voices/ 存储目录。
    fn guess_file_type(message_type: i32, filename: &str, mime: &str) -> &'static str {
        let image_type = privchat_protocol::message::ContentMessageType::Image as i32;
        let video_type = privchat_protocol::message::ContentMessageType::Video as i32;
        let voice_type = privchat_protocol::message::ContentMessageType::Voice as i32;
        if message_type == image_type {
            return "image";
        }
        if message_type == video_type {
            return "video";
        }
        if message_type == voice_type {
            return "voice";
        }
        if mime.starts_with("image/") {
            return "image";
        }
        if mime.starts_with("video/") {
            return "video";
        }
        let ext = filename
            .rsplit('.')
            .next()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "heic" => "image",
            "mp4" | "mov" | "mkv" | "avi" | "webm" => "video",
            _ => "file",
        }
    }

    /// 附件消息的会话预览文案。
    ///
    /// 分层：
    /// 1. 第一层看 `message_type`（Voice / Image / Video 各自独立类型）——
    ///    协议级类型是真理，一旦匹配就返回固定文案，不受 file_type 影响。
    /// 2. 第二层（File 消息）才按存储层 `file_type`（"image"/"video"/"file"）细分。
    ///
    /// 与「发送入口决定消息类型，MIME 不反推」原则一致：只有 File 消息才允许
    /// 靠 MIME 推回 `[图片]`/`[视频]` 这种预览文案。
    fn attachment_placeholder_text(message_type: i32, file_type: &str) -> &'static str {
        use privchat_protocol::message::ContentMessageType;
        if message_type == ContentMessageType::Voice as i32 {
            return "[语音]";
        }
        if message_type == ContentMessageType::Image as i32 {
            return "[图片]";
        }
        if message_type == ContentMessageType::Video as i32 {
            return "[视频]";
        }
        match file_type {
            "image" => "[图片]",
            "video" => "[视频]",
            _ => "[文件]",
        }
    }

    fn default_mime_for_message_type(message_type: i32) -> &'static str {
        use privchat_protocol::message::ContentMessageType;
        let image_type = ContentMessageType::Image as i32;
        let video_type = ContentMessageType::Video as i32;
        let voice_type = ContentMessageType::Voice as i32;
        if message_type == image_type {
            "image/jpeg"
        } else if message_type == video_type {
            "video/mp4"
        } else if message_type == voice_type {
            "audio/mp4"
        } else {
            "application/octet-stream"
        }
    }

    /// 从 extra JSON 中按优先级读 u64：先找顶层 `key`，再找 `metadata.key`。
    fn pick_u64_from_extra(extra: &str, key: &str) -> Option<u64> {
        let value = serde_json::from_str::<serde_json::Value>(extra).ok()?;
        value
            .get(key)
            .or_else(|| value.get("metadata").and_then(|m| m.get(key)))
            .and_then(|v| v.as_u64())
    }

    /// 语音消息：协议要求 `VoiceMetadata.duration` 必填。
    ///
    /// 上传完成后 SDK 会用 attachment_content 重写 message.content，如果不把发送侧
    /// 采样的时长搬进去，接收端 VoiceMetadata 解析就会拿到 duration=0。录制不足 1 秒
    /// 时兜底为 1，与客户端约定保持一致。
    fn merge_voice_metadata(extra: &str, attachment_content: &mut serde_json::Value) {
        let duration_secs = Self::pick_u64_from_extra(extra, "duration")
            .map(|v| v.min(u32::MAX as u64) as u32)
            .unwrap_or(1);
        if let Some(obj) = attachment_content.as_object_mut() {
            obj.insert(
                "duration".to_string(),
                serde_json::Value::from(duration_secs),
            );
        }
    }

    /// 视频消息：协议 `VideoMetadata` 要求 duration / width / height 必填，缩略图三件套可选。
    ///
    /// 与语音不同，视频的 UI（气泡比例、首帧渲染）依赖完整的尺寸信息，因此独立成函数处理，
    /// 避免和语音的单字段逻辑混用。未抽帧的视频允许缺省 thumbnail_*，由客户端播放器自行处理。
    fn merge_video_metadata(extra: &str, attachment_content: &mut serde_json::Value) {
        let Some(obj) = attachment_content.as_object_mut() else {
            return;
        };
        let insert_u32 =
            |obj: &mut serde_json::Map<String, serde_json::Value>, key: &str, v: u64| {
                obj.insert(
                    key.to_string(),
                    serde_json::Value::from(v.min(u32::MAX as u64) as u32),
                );
            };
        if let Some(duration) = Self::pick_u64_from_extra(extra, "duration") {
            insert_u32(obj, "duration", duration);
        }
        if let Some(width) = Self::pick_u64_from_extra(extra, "width") {
            insert_u32(obj, "width", width);
        }
        if let Some(height) = Self::pick_u64_from_extra(extra, "height") {
            insert_u32(obj, "height", height);
        }
        if let Some(tw) = Self::pick_u64_from_extra(extra, "thumbnail_width") {
            insert_u32(obj, "thumbnail_width", tw);
        }
        if let Some(th) = Self::pick_u64_from_extra(extra, "thumbnail_height") {
            insert_u32(obj, "thumbnail_height", th);
        }
    }

    /// 补全 channel 的 last_message_* 元数据字段。
    ///
    /// **重要**：本函数**不再改写 `last_msg_content`**（参见架构归正）。
    /// content 字段保持为消息的原始体（TEXT = 纯文本 / 其它 = 结构化 JSON），
    /// 预览文案由 UI 层基于 `last_message_type` + `last_message_is_revoked` + i18n 渲染。
    ///
    /// 行为：通过 `last_local_message_id` 查本地消息表，取出 `message_type` 与
    /// 撤回标记后写到 channel 的相应字段。查不到时保持 None / false。
    async fn materialize_channel_preview(&self, mut channel: StoredChannel) -> StoredChannel {
        if channel.last_local_message_id > 0 {
            if let Ok(Some(message)) = self
                .storage
                .get_message_by_id(channel.last_local_message_id)
                .await
            {
                if message.channel_id == channel.channel_id
                    && message.channel_type == channel.channel_type
                {
                    channel.last_message_type = Some(message.message_type);
                    // StoredMessage 暂无显式 revoked 字段；客户端通过 message_type==System
                    // + content/extra 中的 topic / metadata 推断（spec/SYSTEM_MESSAGE_SPEC §3）。
                    // 这里保守置 false，未来如需精确从 StoredMessageExtra.revoke 派生再补。
                    channel.last_message_is_revoked = false;
                }
            }
        }
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

    /// 解码图片并应用 EXIF orientation，返回的尺寸/像素均为「显示方向」。
    ///
    /// `image` crate 默认不会消费 EXIF orientation：`img.width()/height()` 给的是
    /// 传感器原始像素方向。真机横拍照片常带 orientation=6/8（旋转 90°），不处理会让
    /// metadata 宽高反置（web 按 metadata 定气泡比例 → 比例错）且缩略图转向错。
    /// 这里读出 orientation 并 apply 到解码结果，保证所有下游消费者拿到的是显示方向。
    fn decode_image_oriented(source_path: &std::path::Path) -> Result<image::DynamicImage> {
        let reader = image::ImageReader::open(source_path)
            .map_err(|e| Error::Storage(format!("open image failed: {e}")))?;
        let mut decoder = reader
            .into_decoder()
            .map_err(|e| Error::Storage(format!("decode image failed: {e}")))?;
        let orientation = image::ImageDecoder::orientation(&mut decoder)
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let mut img = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| Error::Storage(format!("decode image failed: {e}")))?;
        img.apply_orientation(orientation);
        Ok(img)
    }

    fn generate_image_thumbnail_sync(
        source_path: &std::path::Path,
        output_path: &std::path::Path,
        max_edge: u32,
        quality: u8,
    ) -> Result<(u32, u32, u64)> {
        let img = Self::decode_image_oriented(source_path)?;
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
        // WebP supports RGBA, so preserve alpha channel for PNG/WebP sources.
        let rgba = thumb.to_rgba8();
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::Storage(format!("create thumb dir failed: {e}")))?;
        }
        let file = std::fs::File::create(output_path)
            .map_err(|e| Error::Storage(format!("create thumb failed: {e}")))?;
        let mut writer = std::io::BufWriter::new(file);
        let _ = quality; // lossless WebP; quality unused but kept for API compat
        let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut writer);
        encoder
            .encode(
                rgba.as_raw(),
                thumb.width(),
                thumb.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|e| Error::Storage(format!("encode thumb webp failed: {e}")))?;
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
        // 附件加密 v1（ATTACHMENT_ENCRYPTION_SPEC）：所有聊天附件（图片/视频/文件/语音/缩略图，
        // 均经此统一上传点）整文件 AES-256-GCM 加密；上传密文 blob = nonce||ct||tag，
        // multipart 带 encryption_version=1 + cek(base64url)。对象存储只存密文。CEK 不进日志。
        let (blob, cek_b64) = crate::attachment_crypto::encrypt_attachment(&data)
            .map_err(|e| Error::Serialization(format!("attachment encrypt failed: {e}")))?;
        let part = reqwest::multipart::Part::bytes(blob)
            .file_name(filename.to_string())
            .mime_str(mime_type)
            .map_err(|e| Error::Serialization(format!("invalid mime_type for upload part: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("encryption_version", "1")
            .text("cek", cek_b64);
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
        let envelope = response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| Error::Serialization(format!("decode upload response: {e}")))?;
        // server `SERVICE_RESPONSE_ENVELOPE_SPEC` v1.1：所有 HTTP 接口统一 `{code, message, data}`。
        // upload 响应也走信封；业务字段在 `data` 子对象里。
        let code = envelope.get("code").and_then(|v| v.as_u64()).unwrap_or(0);
        if code != 0 {
            let message = envelope
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("upload failed");
            return Err(Error::Transport(format!(
                "upload failed: code={code} message={message}"
            )));
        }
        let value = envelope.get("data").cloned().unwrap_or(envelope);
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
        // 命令里不带文件字节。附件由 SDK 托管在本地，`message.content` 就是它的
        // 路径——把几十上百 MB 复制进 SQLite 只是同一份数据存两遍，还要跟着事务
        // 一起写。空 payload 因此是**正常情况**：从托管路径读。
        //
        // 旧 sled 队列里的项内嵌了字节，搬过来后仍然带着，按原样发。
        let payload = if payload.is_empty() {
            let path = message
                .content
                .strip_prefix("file://")
                .unwrap_or(&message.content);
            // 异步读：单 actor 跑着收消息、同步和事件发布，几十上百 MB 的
            // 附件用同步读会把这些一起卡住。
            tokio::fs::read(path).await.map_err(|e| {
                Error::InvalidState(format!("attachment source unreadable at {path}: {e}"))
            })?
        } else {
            payload
        };
        if payload.is_empty() {
            return Err(Error::InvalidState(
                "attachment payload is empty".to_string(),
            ));
        }
        // Extract original filename from content (before server overwrites it)
        let original_filename = std::path::Path::new(&message.content)
            .file_name()
            .and_then(|v| v.to_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty() && !s.starts_with('['));

        // Determine MIME from original filename (best effort)
        let mime_type = original_filename
            .as_deref()
            .map(|n| Self::guess_mime_type(n).to_string())
            .unwrap_or_else(|| {
                Self::default_mime_for_message_type(message.message_type).to_string()
            });

        // Canonical payload filename: payload.{ext} (Spec §7.5 v2)
        let filename =
            media_store::payload_filename_with_fallback(&mime_type, original_filename.as_deref());

        let file_type =
            Self::guess_file_type(message.message_type, &filename, &mime_type).to_string();

        let storage_paths = self.storage.get_storage_paths().await?;
        let user_root = PathBuf::from(&storage_paths.user_root);
        let files_dir =
            media_store::get_message_dir(&user_root, message.message_id as i64, message.created_at);
        std::fs::create_dir_all(&files_dir)
            .map_err(|e| Error::Storage(format!("create files dir failed: {e}")))?;

        let body_path = files_dir.join(&filename);
        let meta_path = files_dir.join(media_store::META_FILENAME);
        std::fs::write(&body_path, &payload)
            .map_err(|e| Error::Storage(format!("write body file failed: {e}")))?;
        let mut upload_payload = payload;
        let mut upload_filename = filename.clone();
        let mut body_size = upload_payload.len() as u64;

        let mut source_width = None;
        let mut source_height = None;
        let mut thumb_upload: Option<(PathBuf, String, String)> = None;
        if file_type == "image" {
            if let Ok(img) = Self::decode_image_oriented(&body_path) {
                source_width = Some(img.width());
                source_height = Some(img.height());
            }
            let canonical_thumb = files_dir.join(media_store::THUMB_FILENAME);
            let (w, h, size) =
                Self::generate_image_thumbnail_sync(&body_path, &canonical_thumb, 320, 85)?;
            let _ = self
                .storage
                .update_thumb_status(message.message_id, 1)
                .await;
            thumb_upload = Some((
                canonical_thumb,
                "image/webp".to_string(),
                media_store::THUMB_FILENAME.to_string(),
            ));
            let meta = MediaMeta {
                source: MediaSourceMeta {
                    original_filename: original_filename.clone().unwrap_or_default(),
                    mime: mime_type.clone(),
                    width: source_width,
                    height: source_height,
                    file_size: Some(body_size),
                },
                thumbnail: Some(MediaThumbnailMeta {
                    width: Some(w),
                    height: Some(h),
                    file_size: Some(size),
                    mime: Some("image/webp".to_string()),
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
                // Hook writes compressed output in place to `payload.{ext}`.
                // On failure the implementation must leave the file untouched.
                let compressed_ok =
                    hook(MediaProcessOp::Compress, &body_path, &meta_path, &body_path)
                        .map_err(|e| Error::Storage(format!("video compress hook failed: {e}")))?;
                if compressed_ok {
                    upload_payload = std::fs::read(&body_path).map_err(|e| {
                        Error::Storage(format!("read compressed video failed: {e}"))
                    })?;
                    body_size = upload_payload.len() as u64;
                    upload_filename = filename.clone();
                }
            }

            let canonical_thumb = files_dir.join(media_store::THUMB_FILENAME);
            let thumb_scratch = files_dir.join("thumb.src.jpg");
            let mut hook_used = false;
            // Host (Kotlin/iOS) may have pre-generated thumb.webp during the send-prep
            // loading modal. If present, trust it: set thumb_status=1 and skip both the
            // sync hook and Plan 2 async path.
            if canonical_thumb.exists()
                && std::fs::metadata(&canonical_thumb)
                    .map(|m| m.len() > 0)
                    .unwrap_or(false)
            {
                hook_used = true;
                let _ = self
                    .storage
                    .update_thumb_status(message.message_id, 1)
                    .await;
                thumb_upload = Some((
                    canonical_thumb.clone(),
                    "image/webp".to_string(),
                    media_store::THUMB_FILENAME.to_string(),
                ));
            }
            if !hook_used {
                if let Some(hook) = self.video_process_hook.as_ref() {
                    // Hook outputs JPEG; Rust re-encodes to canonical WebP (Spec §FILE_STORAGE).
                    let ok = hook(
                        MediaProcessOp::Thumbnail,
                        &body_path,
                        &meta_path,
                        &thumb_scratch,
                    )
                    .map_err(|e| Error::Storage(format!("video thumbnail hook failed: {e}")))?;
                    if ok && thumb_scratch.exists() {
                        match Self::generate_image_thumbnail_sync(
                            &thumb_scratch,
                            &canonical_thumb,
                            320,
                            85,
                        ) {
                            Ok(_) => {
                                hook_used = true;
                                let _ = self
                                    .storage
                                    .update_thumb_status(message.message_id, 1)
                                    .await;
                                thumb_upload = Some((
                                    canonical_thumb.clone(),
                                    "image/webp".to_string(),
                                    media_store::THUMB_FILENAME.to_string(),
                                ));
                            }
                            Err(e) => {
                                eprintln!("[SDK.video] re-encode thumbnail to webp failed: {e}");
                            }
                        }
                        let _ = std::fs::remove_file(&thumb_scratch);
                    }
                }
            }
            // Plan 2: no sync hook registered → issue an async media job to the host
            // (Kotlin/iOS) and block on a oneshot. `submit_media_job_result` bypasses
            // the actor command channel because the actor is blocked here.
            if !hook_used && self.event_tx.is_some() {
                const VIDEO_THUMBNAIL_TIMEOUT_MS: u64 = 8_000;
                let job_id = self
                    .snowflake
                    .next_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|_| {
                        chrono::Utc::now()
                            .timestamp_nanos_opt()
                            .unwrap_or(0)
                            .to_string()
                    });
                let (job_tx, job_rx) = oneshot::channel::<MediaJobResult>();
                {
                    let mut locked = self
                        .pending_media_jobs
                        .lock()
                        .expect("pending_media_jobs poisoned");
                    locked.insert(job_id.clone(), job_tx);
                }
                let event = SdkEvent::MediaJobRequested {
                    job_id: job_id.clone(),
                    job_kind: "video_thumbnail".to_string(),
                    source_path: body_path.display().to_string(),
                    output_path: thumb_scratch.display().to_string(),
                    mime_type: mime_type.clone(),
                    message_id: message.message_id,
                    timeout_ms: VIDEO_THUMBNAIL_TIMEOUT_MS,
                };
                if let (Some(tx), Some(history), Some(seq)) = (
                    self.event_tx.as_ref(),
                    self.event_history.as_ref(),
                    self.event_seq.as_ref(),
                ) {
                    emit_sequenced_event(tx, history, seq, self.event_history_limit, event);
                }
                let wait =
                    tokio::time::timeout(Duration::from_millis(VIDEO_THUMBNAIL_TIMEOUT_MS), job_rx)
                        .await;
                // Clear the entry on any exit path — host may submit after timeout.
                if let Ok(mut locked) = self.pending_media_jobs.lock() {
                    locked.remove(&job_id);
                }
                match wait {
                    Ok(Ok(result)) if result.ok => {
                        let out = result
                            .output_path
                            .as_deref()
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|| thumb_scratch.clone());
                        if out.exists() {
                            match Self::generate_image_thumbnail_sync(
                                &out,
                                &canonical_thumb,
                                320,
                                85,
                            ) {
                                Ok(_) => {
                                    hook_used = true;
                                    let _ = self
                                        .storage
                                        .update_thumb_status(message.message_id, 1)
                                        .await;
                                    thumb_upload = Some((
                                        canonical_thumb.clone(),
                                        "image/webp".to_string(),
                                        media_store::THUMB_FILENAME.to_string(),
                                    ));
                                }
                                Err(e) => {
                                    eprintln!(
                                        "[SDK.video] plan2 re-encode thumbnail to webp failed: {e}"
                                    );
                                }
                            }
                        } else {
                            eprintln!(
                                "[SDK.video] plan2 host reported ok but {} missing",
                                out.display()
                            );
                        }
                        let _ = std::fs::remove_file(&thumb_scratch);
                    }
                    Ok(Ok(result)) => {
                        eprintln!(
                            "[SDK.video] plan2 host failed job {job_id}: {}",
                            result.error.unwrap_or_else(|| "unknown".to_string())
                        );
                    }
                    Ok(Err(_)) => {
                        eprintln!("[SDK.video] plan2 job {job_id} sender dropped");
                    }
                    Err(_) => {
                        eprintln!(
                            "[SDK.video] plan2 job {job_id} timed out after {}ms",
                            VIDEO_THUMBNAIL_TIMEOUT_MS
                        );
                    }
                }
            }
            if !hook_used {
                let _ = self
                    .storage
                    .update_thumb_status(message.message_id, 3)
                    .await;
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
                    original_filename: original_filename.clone().unwrap_or_default(),
                    mime: mime_type.clone(),
                    width: None,
                    height: None,
                    file_size: Some(body_size),
                },
                thumbnail: if thumb_upload.is_some() {
                    Some(MediaThumbnailMeta {
                        width: thumb_w,
                        height: thumb_h,
                        file_size: thumb_size,
                        mime: thumb_mime,
                    })
                } else {
                    None
                },
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
                    original_filename: original_filename.clone().unwrap_or_default(),
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

        eprintln!(
            "[SDK.actor] process_outbound_file: message_id={} file_type={} mime={} body_size={} has_thumb={}",
            message.message_id, file_type, mime_type, body_size, thumb_upload.is_some()
        );

        let uploaded_thumbnail = if let Some((thumb_path, thumb_mime, thumb_name)) = thumb_upload {
            let thumb_size = std::fs::metadata(&thumb_path)
                .map(|m| m.len() as i64)
                .map_err(|e| Error::Storage(format!("stat thumb file failed: {e}")))?;
            eprintln!(
                "[SDK.actor] process_outbound_file: uploading thumbnail size={}",
                thumb_size
            );
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

        eprintln!(
            "[SDK.actor] process_outbound_file: requesting upload token for main file size={}",
            upload_payload.len()
        );
        let token = self
            .request_upload_token(
                message.from_uid,
                upload_filename.clone(),
                upload_payload.len() as i64,
                mime_type.clone(),
                file_type.clone(),
            )
            .await?;
        eprintln!(
            "[SDK.actor] process_outbound_file: uploading main file to {}",
            token.upload_url
        );
        let uploaded = self
            .upload_file_bytes(
                &token.upload_url,
                &token.token,
                &upload_filename,
                &mime_type,
                upload_payload,
            )
            .await?;
        eprintln!("[SDK.actor] process_outbound_file: upload callback");
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

        // 图片消息协议必须带缩略图引用(server 校验拒绝缺失)。缩略图未产出时把
        // 原图 file 引用为缩略图(接收端按缩略图链路下载原图渲染),与 TS SDK 一致;
        // 视频/文件不强制。
        let (thumbnail_file_id_u64, fallback_thumb_url) = match thumbnail_file_id_u64 {
            Some(id) => (Some(id), None),
            None if file_type == "image" => {
                (Some(uploaded_file_id), Some(uploaded.file_url.clone()))
            }
            None => (None, None),
        };

        let mut attachment_content = if let Some(thumb_file_id) = thumbnail_file_id_u64 {
            let thumbnail_url = uploaded_thumbnail
                .as_ref()
                .map(|v| v.file_url.clone())
                .or(fallback_thumb_url)
                .unwrap_or_default();
            // Scheme B：缩略图也是独立 file，接收端走 thumbnail_file_id -> file/get_url -> cek
            // 统一下载解密。**CEK 永不进消息 metadata**。thumbnail_url 仅作 legacy 明文 fallback。
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
                // 原图尺寸（客户端加密前解码得到）。加密后服务端拿不到密文图片尺寸，
                // 必须客户端带上，否则接收端 UI 没法按原比例渲染气泡（只能正方形兜底）。
                "width": source_width.unwrap_or(0),
                "height": source_height.unwrap_or(0),
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
                "width": source_width.unwrap_or(0),
                "height": source_height.unwrap_or(0),
            })
        };

        // 按消息类型独立合并 metadata：Voice 与 Video 的协议形态不同，不共用逻辑。
        let msg_type = message.message_type;
        if msg_type == (privchat_protocol::message::ContentMessageType::Voice as i32) {
            Self::merge_voice_metadata(&message.extra, &mut attachment_content);
        } else if msg_type == (privchat_protocol::message::ContentMessageType::Video as i32) {
            Self::merge_video_metadata(&message.extra, &mut attachment_content);
        }
        let content = serde_json::to_string(&attachment_content)
            .map_err(|e| Error::Serialization(format!("encode attachment content: {e}")))?;
        let req = self.build_send_message_request_with_content(
            message,
            local_message_id,
            content.clone(),
        )?;
        let resp = self.direct_send_message(req).await?;
        // 把带 width/height/file_id/thumbnail 的最终 content 回写发送端本地行。
        // 否则本地行停在入队时的初始 content（无尺寸），发送端自己的气泡读不到宽高、
        // 退化竖向默认 150×200（接收端拿的是 wire content，所以一直正常）。best-effort：
        // 回写失败不影响已发出的消息，重进会话时下次读到的仍是这份 content。
        if let Err(err) = self
            .storage
            .update_message_content(message.message_id, &content)
            .await
        {
            eprintln!(
                "[SDK.actor] update_message_content after send failed: message_id={} err={err}",
                message.message_id
            );
        }
        Ok(resp)
    }

    async fn rpc_call_json(
        &mut self,
        route: String,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let timeout = self.timeout();
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| Error::Serialization(format!("encode rpc body: {e}")))?;
        let request_context = format!("rpc call route={route}");
        let request = RpcRequest {
            route,
            body: body_bytes,
        };
        let payload = encode_message(&request)
            .map_err(|e| Error::Serialization(format!("encode rpc request: {e}")))?;
        let raw = self
            .request_bytes(
                Bytes::from(payload),
                MessageType::RpcRequest as u8,
                timeout,
                &request_context,
            )
            .await?;
        let rpc_resp: RpcResponse = decode_message(&raw)
            .map_err(|e| Error::Serialization(format!("decode rpc response: {e}")))?;
        if rpc_resp.code != 0 {
            // 认证段 (10000-10099) 单独归为 Error::Auth（非 retryable），
            // 其它 code 交给 is_retryable_server_code 按段判定。
            let code_u32 = rpc_resp.code as u32;
            if (10000..10100).contains(&code_u32) {
                return Err(Error::Auth(format!(
                    "[{}] {}",
                    rpc_resp.code, rpc_resp.message
                )));
            }
            return Err(Error::Server {
                code: code_u32,
                message: rpc_resp.message,
            });
        }
        match rpc_resp.data {
            Some(bytes) if !bytes.is_empty() => serde_json::from_slice(&bytes)
                .map_err(|e| Error::Serialization(format!("decode rpc data: {e}"))),
            _ => Ok(serde_json::Value::Null),
        }
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
        let prev_last_local_message_id = existing
            .as_ref()
            .map(|c| c.last_local_message_id)
            .unwrap_or(0);
        let prev_last_msg_content = existing
            .as_ref()
            .map(|c| c.last_msg_content.clone())
            .unwrap_or_default();
        let prev_last_msg_timestamp = existing.as_ref().map(|c| c.last_msg_timestamp).unwrap_or(0);
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
                // channel_type 客户端约定：1=DM，2=群，其它=房间。
                let inferred_name = if channel_type == 1 {
                    // 系统会话不特判(uid 只是部署事实):名字统一走 user 实体解析链,
                    // 这里仅留 uid 文本兜底,由上层按 username/user_type 本地化。
                    match from_uid {
                        Some(uid) if uid > 0 && uid != current_uid => uid.to_string(),
                        _ => String::new(),
                    }
                } else {
                    // 群/房间的名字来自 group 实体（entity sync），这里绝不能拿 channel_id
                    // 当名字：它会被写进 channel.channel_name，而频道列表查询对群会优先取
                    // channel_name，于是真正的群名（即便随后同步到）被永久盖住，标题卡在裸 id。
                    // 留空，交给查询回落到 group.name / 成员名。
                    String::new()
                };
                (
                    inferred_name,
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
        // 预览反乱序守卫（MESSAGE_HISTORY spec §2.5）：预览选择必须与显示排序同构
        // （pending 最新端；已确认按 pts）。到达顺序 ≠ pts 顺序（例：登录通知 pts=2 先经
        // push 到达，欢迎消息 pts=1 后经 sync 补齐——此前预览被"到达序"覆盖成旧消息）。
        // 仅当新行不older于当前预览行时才覆盖预览字段；unread 计数不受影响照常更新。
        let preview_should_update =
            if prev_last_local_message_id == 0 || prev_last_local_message_id == message_id {
                true
            } else {
                let new_row = self
                    .storage
                    .get_message_by_id(message_id)
                    .await
                    .ok()
                    .flatten();
                match new_row {
                    None => true, // 新行尚未落库（调用时序差异）：保守覆盖，下次调用自愈
                    Some(nr) => {
                        if nr.server_message_id.unwrap_or(0) == 0 {
                            true // pending 自发消息 = 时间线最新端
                        } else {
                            match self
                                .storage
                                .get_message_by_id(prev_last_local_message_id)
                                .await
                                .ok()
                                .flatten()
                            {
                                None => true, // 旧预览行已不存在（被清理），覆盖
                                Some(or) => {
                                    if or.server_message_id.unwrap_or(0) == 0 {
                                        // 旧预览是 pending：收到的 server 消息按时间更新，允许覆盖
                                        // （pending ack 回填 pts 后由后续更新自然归位）
                                        true
                                    } else {
                                        nr.pts.unwrap_or(0) >= or.pts.unwrap_or(0)
                                    }
                                }
                            }
                        }
                    }
                }
            };

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
                last_msg_timestamp: if preview_should_update {
                    timestamp_ms
                } else {
                    prev_last_msg_timestamp
                },
                last_local_message_id: if preview_should_update {
                    message_id
                } else {
                    prev_last_local_message_id
                },
                last_msg_content: if preview_should_update {
                    content.to_string()
                } else {
                    prev_last_msg_content.clone()
                },
                // Timeline preview updates must not mint a synthetic entity version.
                // Keep the existing channel entity version so later sync_entities(channel)
                // payloads can still apply top/mute/name changes from the server.
                version: existing_version,
                // 非同步路径不触碰对端：upsert SQL 用 COALESCE 保留已存 peer_user_id。
                peer_user_id: None,
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
    /// 见 [State::switch_requested]：切换账号前先记一笔并叫醒 actor，让正在跑的同步让出。
    switch_requested: Arc<AtomicU64>,
    switch_wakeup: Arc<tokio::sync::Notify>,
    startup_error: Arc<StdMutex<Option<Error>>>,
    snowflake: Arc<snowflake_me::Snowflake>,
    presence_cache: Arc<StdMutex<HashMap<u64, PresenceStatus>>>,
    typing_throttle: Arc<StdMutex<HashMap<(u64, bool, u8), std::time::Instant>>>,
    data_dir: Arc<String>,
    /// file queue 的路由键（构造期由 endpoint 固化）。附件首发与重试必须用同一个键，
    /// 才会落到同一条有序队列。
    file_route_key: Arc<Option<String>>,
    download_manager: media_download::DownloadManager,
    pending_media_jobs: Arc<StdMutex<HashMap<String, oneshot::Sender<MediaJobResult>>>>,
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
                | SdkEvent::ForcedLogout { .. }
        )
    }

    pub fn new(config: PrivchatConfig) -> Self {
        Self::with_runtime(config, RuntimeProvider::new_owned())
    }

    pub fn with_runtime(config: PrivchatConfig, runtime_provider: RuntimeProvider) -> Self {
        let configured_data_dir = config.data_dir.clone();
        let data_dir_for_self = configured_data_dir.clone();
        // 附件 file queue 的路由键在构造期固化：首发与重试必须落到同一条有序队列。
        let file_route_key = config.endpoints.first().map(Self::endpoint_route_key);
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
        let pending_media_jobs: Arc<StdMutex<HashMap<String, oneshot::Sender<MediaJobResult>>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let actor_pending_media_jobs = pending_media_jobs.clone();
        // CODEX-8：worker 位取自持久化 installation id（稳定设备身份），替代 pid/启动毫秒的
        // 临时派生 —— 重启后 worker 位不漂移；配合服务端 (sender, device, local_message_id)
        // 幂等命名空间，雪花碰撞面收敛到单设备内（由毫秒+序列保证唯一）。
        let (machine_id, data_center_id) = stable_snowflake_worker_bits(&config.data_dir);
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
        // 账号切换的「让出」通道：计数器记事实，Notify 负责叫醒。见 State::switch_requested。
        let switch_requested_sdk = Arc::new(AtomicU64::new(0));
        let switch_processed_sdk = Arc::new(AtomicU64::new(0));
        let switch_wakeup_sdk = Arc::new(tokio::sync::Notify::new());
        let switch_requested_actor = switch_requested_sdk.clone();
        let switch_processed_actor = switch_processed_sdk.clone();
        let switch_wakeup_actor = switch_wakeup_sdk.clone();
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
                transport_events: Arc::new(tokio::sync::Mutex::new(None)),
                session_state: SessionState::New,
                bootstrap_completed: saved
                    .as_ref()
                    .map(|s| s.bootstrap_completed)
                    .unwrap_or(false),
                sync_coordinator: SyncCoordinator::new(),
                snowflake,
                storage: storage.clone(),
                skip_inbound_materialization_for_load_testing:
                    SKIP_INBOUND_MATERIALIZATION_FOR_LOAD_TESTING.load(Ordering::SeqCst),
                current_uid,
                session_epoch: 0,
                should_auto_reconnect: false,
                reconnect_attempt: 0,
                next_reconnect_at: None,
                auth_terminal_fired: false,
                inbound_epoch: 0,
                last_resume_synced: None,
                last_anti_entropy_at: Instant::now(),
                convergence_run: None,
                resume_run_id: 0,
                anti_entropy_jitter: Duration::from_secs(rand::random::<u64>() % 16),
                room_seen_msg_ids: HashMap::new(),
                last_terminal_reason: None,
                network_hint: NetworkHint::Unknown,
                receive_pipeline: ReceivePipeline::default(),
                last_sync_queued: 0,
                last_sync_dropped_duplicates: 0,
                last_sync_entity_events: Vec::new(),
                video_process_hook: None,
                link_preview_hook: None,
                last_tmp_cleanup_day: None,
                pending_events: Vec::new(),
                message_cache_policy: MessageCachePolicy::default(),
                channel_message_cache: HashMap::new(),
                channel_cache_generation: HashMap::new(),
                switch_requested: switch_requested_actor,
                switch_processed: switch_processed_actor,
                switch_wakeup: switch_wakeup_actor,
                #[cfg(test)]
                sync_stall_for_test: None,
                channel_cache_lru: VecDeque::new(),
                channel_cache_total_bytes: 0,
                cache_debug_log: std::env::var("PRIVCHAT_CACHE_LOG").ok().as_deref() == Some("1"),
                cache_hit_count: 0,
                cache_miss_count: 0,
                pending_prelogin_inbound_frames: Vec::new(),
                active_subscriptions: HashMap::new(),
                presence_cache: actor_presence_cache,
                event_tx: Some(actor_event_tx.clone()),
                event_history: Some(actor_event_history.clone()),
                event_seq: Some(actor_event_seq.clone()),
                event_history_limit: event_history_limit,
                pending_media_jobs: actor_pending_media_jobs,
                repair_queue: VecDeque::new(),
                repair_seen: HashSet::new(),
                thumbnail_backfill_queue: VecDeque::new(),
                thumbnail_backfill_seen: HashSet::new(),
                repair_backoff: HashMap::new(),
                avatar_cache: avatar_cache::AvatarCacheManager::default(),
            };
            let mut inbound_task: Option<tokio::task::JoinHandle<()>> = None;
            let mut health_tick = interval(Duration::from_secs(15));
            #[allow(unused_mut)]
            let mut health_tick_enabled = true;
            // repair tick 比 health tick 快:损坏的图片/顺序应当尽快自愈,但仍然是
            // **后台**节奏——读路径只入队,一次 tick 最多处理 REPAIR_BATCH_LIMIT 条。
            let mut repair_tick = interval(Duration::from_secs(2));
            // Burst（默认）会把错过的 tick 补齐：后台收敛一轮跑几秒（429 会话账号每页
            // 要 200 次本地读 + 一次批量 RPC + 最多 8 个 difference），错过的 tick 于是
            // 连续就绪，actor 再也回不到命令处理 —— 宿主的 ensure_synced 永远收不到应答，
            // 界面卡在「同步中」。Skip：迟到的 tick 直接丢，只保留下一次。
            repair_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            health_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                // Backoff driver: fires when `next_reconnect_at` is due. When no retry
                // is armed we pin a `pending()` so this arm is inert but keeps its type
                // inside `select!`.
                // New = 传输断开;Connected + 有本地账号 = token-refresh handoff 期间的
                // 未认证保活态(见 AccessTokenRefreshNeeded 发射点)——两者都允许重试驱动。
                let awaiting_reauth = state.session_state == SessionState::Connected
                    && state.current_uid.is_some();
                // 系统 reachability(network_hint)只是加速信号,不是永久闸门:模拟器/
                // 挂起恢复后系统常给出 Offline 假信号且再无恢复回调,若据此一票否决,
                // 重连永不发起(2026-07-24 实测:sim 常亮整夜「网络已断开」,实际网络
                // 通畅)。Offline 期间降频到 ≥60s 做真实连接探测,连上即翻回 Online。
                // ⚠️ 用 next_reconnect_at 的**绝对时刻**,绝不在此按 `now` 现算 deadline。
                // 历史 bug(2026-07-24 生产三修):OFFLINE 分支曾写 `at.max(now + 60s)`,而 15s
                // 的 health_tick 每次醒来都重进 select! 用新的 `now` 重算这个 max → 60s 倒计时
                // 被永久往后推(15s<60s,retry_sleep 永远等不到截止点就被下轮重置)→ 断网后
                // **重试永不触发**,横幅永久卡「网络已断开」。离线降频改到 schedule_next_reconnect
                // 的**设置时**烘进 next_reconnect_at(稳定绝对值),这里只做纯读取。
                let retry_deadline = if state.should_auto_reconnect
                    && (state.session_state == SessionState::New || awaiting_reauth)
                {
                    state.next_reconnect_at
                } else {
                    None
                };
                let retry_sleep = async move {
                    match retry_deadline {
                        Some(at) => {
                            let now = Instant::now();
                            let dur = at.saturating_duration_since(now);
                            sleep(dur).await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::pin!(retry_sleep);

                // sync 退避到期的唤醒。
                //
                // 只有退避门禁是不够的：门禁挡住了热循环，但如果连接一直是好的、
                // 没有新的外部触发（重连成功 / connect / token 刷新）再来敲门，
                // 一次临时 sync 失败就会永远停在 Retrying——退避到期了也没人来跑。
                // 谁持有 deadline 谁就得负责叫醒，这里把它接进 actor 的定时器。
                //
                // 与上面 next_reconnect_at 的区别：那个存的是 Instant，这里存的是
                // **绝对 epoch ms**，所以每轮按 now 重算剩余时长是安全的——目标时刻
                // 本身不会移动，不存在被 health_tick 反复推迟的那类饿死。
                let sync_retry_deadline = if state.session_state == SessionState::Authenticated {
                    state.sync_coordinator.next_retry_at_ms()
                } else {
                    None
                };
                let sync_retry_sleep = async move {
                    match sync_retry_deadline {
                        Some(at_ms) => {
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            let remaining = (at_ms - now_ms).max(0) as u64;
                            sleep(Duration::from_millis(remaining)).await;
                        }
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::pin!(sync_retry_sleep);

                tokio::select! {
                    _ = &mut sync_retry_sleep => {
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] sync retry deadline reached");
                        }
                        let _ = state.ensure_synced(|event| {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                event,
                            );
                        }).await;
                    }
                    _ = repair_tick.tick() => {
                        if state.session_state != SessionState::Shutdown
                            && !state.repair_queue.is_empty()
                        {
                            state.drain_projection_repairs().await;
                        }
                        // 缩略图回填：同样是后台工作，命令优先。队列里有待处理命令时
                        // 直接跳过——让宿主等应答（进而卡住界面）是本末倒置。
                        if rx.is_empty()
                            && state.session_state == SessionState::Authenticated
                            && !state.thumbnail_backfill_queue.is_empty()
                        {
                            state.drain_thumbnail_backfill(|| rx.is_empty()).await;
                        }
                        // Phase 3 后台收敛：一次一小批 stale 频道（run_anti_entropy_once
                        // 内部用 batch_get_channel_pts 批量比对 + WiFi/蜂窝预算）。
                        // 失败只退避重试，**不动 readiness** —— 用户此刻能正常收发。
                        //
                        // 命令优先：队列里有待处理命令时不开新一轮。收敛是后台工作，
                        // 让宿主等应答（进而卡住界面）是本末倒置。
                        if !rx.is_empty()
                            && state.session_state == SessionState::Authenticated
                            && state.convergence_run.is_some()
                        {
                            continue;
                        }
                        if state.session_state == SessionState::Authenticated
                            && state.convergence_run.is_some()
                            && state.sync_coordinator.convergence_retry_ready(
                                chrono::Utc::now().timestamp_millis(),
                            )
                        {
                            match state.run_anti_entropy_once().await {
                                // 只有「完整扫过一圈 + 没有留下未修的 stale」才算收敛。
                                // 用「本页修了 0 条」判定会在扫到第一页干净数据时就
                                // 过早宣布完成，后面的频道再也不会被检查。
                                Ok(page) if page.is_converged() => {
                                    if let Some(stats) = state.convergence_run.take() {
                                        println!(
                                            "[SDK.resume] run={} phase=3 converged scanned={} channels_repaired={} messages_applied={}",
                                            state.resume_run_id,
                                            page.page_scanned,
                                            page.channels_repaired,
                                            page.messages_applied
                                        );
                                        state.sync_coordinator.set_convergence(
                                            crate::sync_coordinator::Convergence::Converged,
                                            chrono::Utc::now().timestamp_millis(),
                                        );
                                        // 全量收敛完成才发 ResumeSyncCompleted —— 保持它
                                        // 「这一轮全做完了」的原始语义。
                                        state.queue_resume_completed(stats);
                                    }
                                }
                                Ok(page) => {
                                    // unknown 不为零也要出声：那是异常规模指标
                                    if page.stale_found > 0
                                        || page.deferred > 0
                                        || page.unknown_channels > 0
                                    {
                                        println!(
                                            "[SDK.resume] run={} phase=3 page scanned={} stale={} channels_repaired={} messages_applied={} deferred={} unknown={}",
                                            state.resume_run_id,
                                            page.page_scanned,
                                            page.stale_found,
                                            page.channels_repaired,
                                            page.messages_applied,
                                            page.deferred,
                                            page.unknown_channels
                                        );
                                    }
                                    state.sync_coordinator.set_convergence(
                                        if page.stale_found > 0 {
                                            crate::sync_coordinator::Convergence::Repairing
                                        } else {
                                            crate::sync_coordinator::Convergence::Scanning
                                        },
                                        chrono::Utc::now().timestamp_millis(),
                                    );
                                }
                                Err(err) => {
                                    let now_ms = chrono::Utc::now().timestamp_millis();
                                    let (attempt, next_retry_at_ms) =
                                        state.sync_coordinator.backoff_convergence(now_ms);
                                    tracing::warn!(
                                        error = %err,
                                        attempt,
                                        next_retry_at_ms,
                                        "convergence pass failed; backing off"
                                    );
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
                            // 让调度器有机会把排队的命令喂进来
                            tokio::task::yield_now().await;
                        }
                        continue;
                    }
                    _ = health_tick.tick(), if health_tick_enabled => {
                        if state.session_state == SessionState::Shutdown {
                            continue;
                        }

                        // ── 活性看门狗（2026-07-24 生产事故三修根治）──────────────────
                        // 断网→联网后横幅永久卡「网络已断开」的架构根因:重试驱动可能走进
                        // inert 死态——retry 守卫兜底(session 短暂非 New 时)会把
                        // next_reconnect_at 置 None,而唯一复活入口(SetNetworkHint
                        // offline→online)依赖 iOS reachability 的恢复回调,该回调在挂起/
                        // 长断网后常常**根本不投递** → 驱动永不再武装 → 永久卡死。
                        //
                        // 修:15s health_tick 无条件充当看门狗——只要有「想在线」的会话
                        // (should_auto_reconnect + 传输断开 New / 待重认证 Connected+uid)
                        // 而驱动已 inert(next_reconnect_at==None),立即重新武装。这样
                        // 恢复不再依赖任何系统回调:OFFLINE 期驱动仍每≤60s 做真实 TCP 探测,
                        // 网络一旦真回来,下一次 try_auto_reconnect 成功即把 hint 复位并认证。
                        // 活性看门狗:想在线(should_auto_reconnect)+ 传输断开(New)/待重认证,
                        // 而重连驱动 inert(next_reconnect_at 空)→ 重新武装。honor should_auto_reconnect
                        // 是必须的:手动/后台 Disconnect 也把 session 设成 New 且保留 uid,唯一区分
                        // 「想重连」vs「显式离线」的信号就是这个标志,不能绕过。
                        let wants_online = state.should_auto_reconnect
                            && (matches!(state.session_state, SessionState::New)
                                || (state.session_state == SessionState::Connected
                                    && state.current_uid.is_some()));
                        if wants_online && state.next_reconnect_at.is_none() {
                            eprintln!(
                                "[SDK.actor] liveness watchdog: retry driver inert (state={:?}, hint={:?}); re-arming reconnect",
                                state.session_state, state.network_hint
                            );
                            state.reconnect_attempt = 0;
                            state.next_reconnect_at = Some(Instant::now());
                        }

                        // ⚠️ 这里曾是 `if !network_hint.is_online() { continue; }` —— 一个
                        // 假 Offline 就把整个 tick 尾部(连接探测 / anti-entropy / **出站队列
                        // 排空**)全部跳过。后果(2026-07-26 生产实测):inbound 推送走的是活着的
                        // transport、不看 hint,所以**用户能正常收消息**,但自己发的消息因为
                        // outbox 永远排不空而卡在「发送中」,既不成功也不失败。
                        // 原则同前:reachability 只是提示,绝不做硬闸门——各操作自身的前置条件
                        // (session_state / bootstrap_completed / 队列非空)才是真正的门槛,真断网
                        // 时它们各自失败即可,代价只是一次廉价的探测。

                        // Probe live connections for silent drops (NAT/proxy timeouts).
                        // apply_transport_health will arm the reconnect driver on loss.
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

                        let _ = state.cleanup_tmp_dirs_if_needed().await;

                        if state.session_state == SessionState::Authenticated
                            && state.bootstrap_completed
                            && state.last_anti_entropy_at.elapsed()
                                >= Duration::from_secs(60) + state.anti_entropy_jitter
                        {
                            // Advance before RPC so a transient failure cannot turn the
                            // 15s health tick into an unbounded retry loop.
                            state.last_anti_entropy_at = Instant::now();
                            if let Err(err) = state.run_anti_entropy_once().await {
                                tracing::warn!(error = %err, "anti-entropy pass failed");
                            }
                        }

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
                    _ = &mut retry_sleep => {
                        // Retry driver: the deadline fired. Guard again because state
                        // may have changed between pin and wake.
                        let awaiting_reauth = state.session_state == SessionState::Connected
                            && state.current_uid.is_some();
                        if (state.session_state != SessionState::New && !awaiting_reauth)
                            || !state.should_auto_reconnect
                        {
                            state.next_reconnect_at = None;
                            continue;
                        }
                        let attempt_n = state.reconnect_attempt.saturating_add(1);
                        eprintln!("[SDK.actor] auto_reconnect_attempt #{attempt_n}");
                        let from = state.session_state.as_connection_state();
                        match state.try_auto_reconnect().await {
                            Ok(next) => {
                                state.session_state = next;
                                state.reset_reconnect_backoff();
                                state.sync_coordinator.note_network_available();
                                // 握手 + 认证全通了，解除 Terminal 闸门，后续如果再过期还能再触发一次。
                                state.auth_terminal_fired = false;
                                // 真实连接成功是网络在线的最强证据:覆盖系统 reachability 的
                                // Offline 假信号(否则 hint 卡 Offline 会继续压制其它路径)。
                                if !state.network_hint.is_online() {
                                    eprintln!("[SDK.actor] probe connected while hint=Offline; reset hint to Unknown");
                                    state.network_hint = NetworkHint::Unknown;
                                }
                                eprintln!("[SDK.actor] auto_reconnect_result ok attempt=#{attempt_n}");
                                // [硬化] 重连成功后必须：① 重启 inbound task（订阅新 transport + bump epoch）
                                start_inbound_task(&mut state, actor_cmd_tx.clone(), &mut inbound_task)
                                    .await;
                                if realtime_trace_enabled() { eprintln!(
                                    "[SDK_RECONNECT_OK] new_epoch={} state={:?} uid={:?}",
                                    state.inbound_epoch, state.session_state, state.current_uid
                                ); }
                                // ② 防御性 replay：若有 push 帧在登录前缓冲（current_uid 恢复后），立即重放，
                                //    避免 message/presence/typing 卡在 prelogin 队列里收不到。
                                if let Err(err) = state.replay_prelogin_inbound_frames().await {
                                    eprintln!("[SDK.actor] prelogin replay failed after reconnect: {err}");
                                }
                                // 重连后重放活跃订阅：恢复 presence_changed / typing / room 广播。
                                state.replay_subscriptions().await;
                                if state.session_state == SessionState::Authenticated
                                    && state.bootstrap_completed
                                {
                                    // ③ resume sync 把重连/断网期间漏掉的增量补回来
                                    if let Err(err) = state.ensure_synced(|event| {
                                        emit_sequenced_event(
                                            &actor_event_tx,
                                            &actor_event_history,
                                            &actor_event_seq,
                                            event_history_limit,
                                            event,
                                        );
                                    }).await {
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
                            Err(e) => {
                                eprintln!("[SDK.actor] auto_reconnect_result fail attempt=#{attempt_n}");
                                if e.is_auth_terminal() {
                                    // Terminal 认证错误（token 撤销/设备不匹配等）由
                                    // trigger_forced_logout 统一收口：停 inbound、断 transport、
                                    // 清状态、发 ForcedLogout。继续自动重连只会把过期 token 反复
                                    // 投给服务端打满日志。
                                    trigger_forced_logout(
                                        &mut state,
                                        &mut inbound_task,
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        &e,
                                        ForcedLogoutSource::ConnectAuth,
                                    )
                                    .await;
                                } else if matches!(
                                    e.auth_kind(),
                                    Some(AuthErrorKind::Recoverable)
                                ) {
                                    // Recoverable（10002 AccessTokenExpired 等）：
                                    // 停 auto-reconnect，避免拿过期 token 反复打服务端。
                                    //
                                    // 关键：try_auto_reconnect 失败时已 disconnect 释放 transport，
                                    // state 仍是 New。如果直接 emit 事件让 host refresh + authenticate，
                                    // 但 SessionState=New 下 Action::Authenticate 会被状态机拒绝
                                    // （spec TOKEN_REFRESH_SPEC §7：New/Shutdown 报错）。
                                    //
                                    // 所以在 emit 事件**之前**主动 connect 一次重建 transport，把 state
                                    // 转回 Connected。host 收到事件后调 sdk.authenticate(uid, newToken,
                                    // deviceId) 直接走 Connected→Authenticated 即可，不需要先 connect
                                    // （否则 host 的 connect 又触发 try_auto_reconnect 用旧 token 撞
                                    // 10002 死循环）。
                                    //
                                    // 若 connect 也失败（网络真断了）则保持 New + emit 事件，host recover
                                    // 会因 sdk.authenticate 报 InvalidState 失败；下次 retry / 网络恢复
                                    // 还会再 emit，最终自愈。
                                    let code = e.auth_error_code().unwrap_or(0);
                                    let message = e.to_string();
                                    eprintln!(
                                        "[SDK.actor] auto_reconnect: recoverable auth error code={code}; reconnect transport then emit AccessTokenRefreshNeeded"
                                    );
                                    let pre_state = state.session_state.as_connection_state();
                                    if let Ok(()) = timeout(state.connect_timeout_total(), state.connect()).await
                                        .unwrap_or(Err(Error::Transport("reconnect timeout".to_string())))
                                    {
                                        if let Ok(next_state) = state.session_state.can(Action::Connect) {
                                            state.session_state = next_state;
                                            emit_sequenced_event(
                                                &actor_event_tx,
                                                &actor_event_history,
                                                &actor_event_seq,
                                                event_history_limit,
                                                SdkEvent::ConnectionStateChanged {
                                                    from: pre_state,
                                                    to: state.session_state.as_connection_state(),
                                                },
                                            );
                                        }
                                    } else {
                                        eprintln!(
                                            "[SDK.actor] auto_reconnect recoverable: post-fail reconnect failed; emit event but host authenticate likely InvalidState"
                                        );
                                    }
                                    // 活性兜底(2026-07-24 生产事故):这里曾 should_auto_reconnect=false
                                    // 永久关闭重连,把恢复责任完全交给宿主的一次性 refresh —— 宿主那一次
                                    // 失败(如撞上服务端发版窗口)就死锁在「网络已断开」,且唯一救活入口
                                    // 是前台/网络回调,常亮设备永远等不到。改为降频保活:60s 一轮继续
                                    // 探测,每轮撞 10002 会再 emit 一次事件(宿主侧 mutex 去重),无论
                                    // 宿主死活 SDK 自身永远有心跳式重试;宿主 refresh 成功后 authenticate
                                    // 会 reset backoff 自然接管。
                                    state.reconnect_attempt = state.reconnect_attempt.saturating_add(1);
                                    state.next_reconnect_at =
                                        Some(Instant::now() + Duration::from_secs(60));
                                    eprintln!(
                                        "[SDK.actor] auto_reconnect: token-refresh handoff to host; keep-alive retry in 60s"
                                    );
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::AccessTokenRefreshNeeded { code, message },
                                    );
                                } else {
                                    state.schedule_next_reconnect();
                                }
                            }
                        }
                    }
                    cmd = rx.recv() => {
                        let Some(cmd) = cmd else { break; };
                        match cmd {
                    Command::Connect { resp } => {
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd connect");
                        }
                        {
                            // ⚠️ 这里**不再**用 network_hint 做硬闸门(2026-07-25 生产事故):
                            // 系统 reachability 断线/挂起后常卡 Offline 且不再投递恢复回调,
                            // 据此一票否决会让宿主的「前台/网络恢复/用户点重连」全部空转。
                            // 显式 Connect 本身就是「我要上线」的强意图信号:直接尝试,真断网
                            // 自然失败并由重试驱动接管;成功则下方把 hint 复位(真实连接=真源)。
                            if !state.network_hint.is_online() {
                                eprintln!(
                                    "[SDK.actor] connect requested while hint=Offline; attempting anyway (hint is advisory)"
                                );
                            }
                            // A user-driven Connect expresses the intent to stay online —
                            // enable auto-reconnect up-front so retry fires even if this
                            // first attempt fails (e.g. server temporarily down).
                            state.should_auto_reconnect = true;
                            let from_state = state.session_state.as_connection_state();
                            let had_local_session = state.current_uid.is_some();
                            let transport_connected = state.is_connected().await;
                            let plan = plan_connect(
                                state.session_state,
                                had_local_session,
                                transport_connected,
                            );
                            let already_ready =
                                matches!(&plan, Ok(ConnectPlan::AlreadyReady));
                            let mut transition_from = from_state;

                            // Queries are observers. Explicit connect is the intent boundary
                            // that reconciles a stale logical session with transport reality.
                            if !transport_connected {
                                if let Some((from, to)) = state.apply_transport_health(false) {
                                    stop_inbound_task(&mut inbound_task).await;
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged { from, to },
                                    );
                                    transition_from = to;
                                }
                            }

                            // 关键不变量：宿主在已登录场景下点 connect()（前台 fast-track / 网络
                            // 恢复 / 后台 disconnect 后回前台），final state 必须**仍是** Authenticated，
                            // 否则任何要 authenticated session 的 RPC（markRead / sendMessage /
                            // search 等）会立刻挂"current: Connected"。
                            //
                            // 触发场景：
                            //  1) SYNC_READY 透明 fast-track：state=Authenticated，transport 还没掉
                            //  2) 后台超时 disconnect 后回前台：state=New，transport=None，但 storage
                            //     里的 session snapshot 还在
                            //  3) 网络抖动：state=New（health probe 降级过），transport=None
                            //
                            // 三种场景都用 try_auto_reconnect 单步法解决：它 atomic 地 connect +
                            // load session + re-authenticate，自然收敛到 Authenticated（有 snapshot）
                            // 或 Connected（无 snapshot：冷启动 / 已 logged out / forced）。
                            //
                            // 之前用过"先转 Connected 再尝试 auto-restore"两步法，transport 偶尔在
                            // `state.connect()` 内部短路没真正重建 → auto-restore race 到失败 → 卡 Connected。
                            let result: Result<()> = match plan {
                                Err(e) => Err(e),
                                Ok(ConnectPlan::AlreadyReady) => Ok(()),
                                Ok(ConnectPlan::RestorePersistedSession) => {
                                    match state.try_auto_reconnect().await {
                                        Ok(next) => {
                                            state.session_state = next;
                                            Ok(())
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Ok(ConnectPlan::ConnectTransportOnly) => {
                                    // 冷启动且无本地 session（用户没登录过）/ logged out / forced：
                                    // 走简单 connect+transition；不 authenticate（没 session 可用）。
                                    match state.session_state.can(Action::Connect) {
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
                                    }
                                }
                            };

                            if result.is_ok() {
                                state.reset_reconnect_backoff();
                                if !already_ready {
                                    state.sync_coordinator.note_network_available();
                                }
                                // 连上了就是网络可达的铁证:复位系统 reachability 的 Offline
                                // 假信号,否则 hint 会继续压制退避节奏与其它路径。
                                if !state.network_hint.is_online() {
                                    eprintln!("[SDK.actor] connect ok while hint=Offline; reset hint to Unknown");
                                    state.network_hint = NetworkHint::Unknown;
                                }
                                // 用户主动 Connect 成功 = 新的登录回合。清 Terminal 闸门
                                // 和上一轮 terminal reason，让后续 Authenticate 若再遇到
                                // Terminal 错可以再次触发 ForcedLogout。
                                state.auth_terminal_fired = false;
                                state.last_terminal_reason = None;
                                if !already_ready {
                                    start_inbound_task(
                                        &mut state,
                                        actor_cmd_tx.clone(),
                                        &mut inbound_task,
                                    )
                                    .await;
                                    if realtime_trace_enabled() { eprintln!(
                                        "[SDK_RECONNECT_OK] (connect-path) new_epoch={} state={:?} uid={:?}",
                                        state.inbound_epoch, state.session_state, state.current_uid
                                    ); }
                                    // [硬化] 防御性 replay：push 帧可能在登录前缓冲，current_uid 恢复后立即重放。
                                    if let Err(err) = state.replay_prelogin_inbound_frames().await {
                                        eprintln!("[SDK.actor] connect: prelogin replay failed: {err}");
                                    }
                                    // 重连后重放活跃订阅：恢复 presence_changed / typing / room 广播。
                                    state.replay_subscriptions().await;
                                }

                                // 若 try_auto_reconnect 把 session 拉回 Authenticated 且
                                // bootstrap 早已完成，触发一次 resume_sync 把背景期间漏掉的
                                // 增量同步上来（与 retry driver 的成功分支一致）。
                                // 入口可能是 Authenticated（fast-track）或 New（后台 disconnect 后回前台），
                                // 只要本地有 session 且当前已 Authenticated，就跑 resume_sync。
                                // 先把「连接已就绪」告诉宿主，再跑 resume sync。
                                //
                                // 顺序反过来会让状态事件被整轮同步挡住：会话多的账号 ensure_synced
                                // 要跑很久，期间 UI 收不到 Authenticated，状态条一直停在「服务器
                                // 连接中」——而连接其实早就建好、数据也正在回来。连接态是既成事实，
                                // 不该等数据层动作完成才通知；同步进度本来就有 resume_sync_* 事件
                                // 单独播报。
                                let to_state = state.session_state.as_connection_state();
                                if transition_from != to_state {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged {
                                            from: transition_from,
                                            to: to_state,
                                        },
                                    );
                                }

                                // 连接已就绪，先答复宿主再跑 resume sync。
                                //
                                // 把 resp.send 留到同步之后，宿主的 connect() 就要挂到整轮
                                // 同步结束才返回：会话多的账号能挂几十秒，而宿主往往在 connect()
                                // 前后切状态条文案（「连接中」→「同步中」→清空），于是状态条
                                // 卡死在「服务器连接中」——连接其实早就建好了。
                                // connect 的语义是「连接建立成功」，不该捎带数据层的耗时。
                                let _ = resp.send(Ok(()));

                                if had_local_session
                                    && state.session_state == SessionState::Authenticated
                                    && state.bootstrap_completed
                                {
                                    if let Err(err) = state.ensure_synced(|event| {
                                        emit_sequenced_event(
                                            &actor_event_tx,
                                            &actor_event_history,
                                            &actor_event_seq,
                                            event_history_limit,
                                            event,
                                        );
                                    }).await {
                                        eprintln!(
                                            "[SDK.actor] connect: resume sync after auto-restore failed: {err}"
                                        );
                                    }
                                }
                            } else {
                                // First attempt failed — schedule a backoff retry so we
                                // don't leave the client silently unconnected.
                                state.schedule_next_reconnect();
                                let _ = resp.send(result);
                            }
                        }
                    }
                    Command::Disconnect { resp } => {
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd disconnect");
                        }
                        state.should_auto_reconnect = false;
                        state.reset_reconnect_backoff();
                        stop_inbound_task(&mut inbound_task).await;
                        let from_state = state.session_state.as_connection_state();
                        let result = state.disconnect().await;
                        if result.is_ok() && state.session_state != SessionState::Shutdown {
                            state.clear_presence_cache();
                            // 显式 disconnect（should_auto_reconnect=false，终态）→ 清订阅注册表，
                            // 不再 replay（重连/重登录会重新订阅）。
                            state.active_subscriptions.clear();
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
                    // **只读，不得有副作用。**
                    //
                    // 连接健康由真实 transport 事件、health tick 和显式 connect/authenticate
                    // 意图对账；观察者不得改变 session 或停止 inbound task。
                    Command::IsConnected { resp } => {
                        let is_connected = state.is_connected().await;
                        let _ = resp.send(Ok(is_connected));
                    }
                    // 同 IsConnected：只读，不裁决健康。
                    Command::GetConnectionState { resp } => {
                        let _ = resp.send(Ok(state.session_state.as_connection_state()));
                    }
                    // 同上，只读。三个字段一次取出，保证它们互相自洽。
                    Command::GetSessionStatus { resp } => {
                        let _ = resp.send(Ok(SessionStatus {
                            state: state.session_state.as_connection_state(),
                            account_uid: state.current_uid.clone(),
                            session_epoch: state.session_epoch,
                        }));
                    }
                    #[cfg(test)]
                    Command::SetSessionStateForTest {
                        session_state,
                        resp,
                    } => {
                        health_tick_enabled = false;
                        state.session_state = session_state;
                        let _ = resp.send(());
                    }
                    Command::GetCurrentAccessToken { resp } => {
                        let token = match state.current_uid.clone() {
                            Some(uid) => state.storage.load_access_token(uid).await,
                            None => Ok(None),
                        };
                        let _ = resp.send(token);
                    }
                    Command::GetLastTerminalReason { resp } => {
                        let _ = resp.send(Ok(state.last_terminal_reason.clone()));
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
                            // Network just came back — reset backoff and let the retry
                            // driver fire immediately. No inline retry here; the select!
                            // arm will wake up on the zero-delay deadline.
                            eprintln!("[SDK.actor] network_hint offline->online: reset backoff, arming immediate retry");
                            state.mark_reconnect_ready_now();
                            // sync 的退避同理：它等的是「有希望重试的时刻」，网络刚
                            // 回来正是那个时刻，再让用户干等剩余退避是把节流用错地方。
                            state.sync_coordinator.note_network_available();
                        }
                        let _ = resp.send(Ok(()));
                    }
                    Command::InboundFrame { epoch, biz_type, data } => {
                        // 丢弃旧 inbound 任务遗留的帧（例如 ForcedLogout 发生时已 stop，
                        // 但 mpsc 通道里可能还有 pending 帧）。
                        if epoch != state.inbound_epoch {
                            // [DIAG] 重连 epoch 竞态高度可疑：若 push 帧因 epoch 不匹配被丢，
                            // 这里会持续打印 → 根因即重连 epoch 管理。
                            if realtime_trace_enabled() { eprintln!(
                                "[SDK_INBOUND_FRAME_DROPPED_EPOCH] frame_epoch={} current_epoch={} biz_type={} payload_len={} uid={:?}",
                                epoch, state.inbound_epoch, biz_type, data.len(), state.current_uid
                            ); }
                            continue;
                        }
                        if realtime_trace_enabled() { eprintln!(
                            "[SDK_PUSH_FRAME_RECEIVED] frame_epoch={} current_epoch={} biz_type={} payload_len={} uid={:?}",
                            epoch, state.inbound_epoch, biz_type, data.len(), state.current_uid
                        ); }
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
                    Command::InboundDisconnected { epoch } => {
                        if epoch != state.inbound_epoch {
                            continue;
                        }
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
                    Command::SetLinkPreviewHook { hook, resp } => {
                        state.link_preview_hook = hook;
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
                        let has_access_token = !token.is_empty();
                        tracing::debug!(
                            user_id = %user_id,
                            device_id = %device_id,
                            has_access_token,
                            state_before = ?state.session_state,
                            "authenticate command started"
                        );
                        let transport_connected = state.is_connected().await;
                        let preflight = plan_authenticate_transport(
                            state.session_state,
                            transport_connected,
                        );
                        // 认证要么用当前连接、要么先重建——**两条路共用同一段拆除流程**
                        // （停 inbound / 掀 transport 健康 / 发状态事件 / 重连），
                        // 免得重试路径少做几步，留下「逻辑上已认证、实际没有 transport」。
                        let mut need_reconnect = match preflight {
                            Err(e) => {
                                let _ = resp.send(Err(e));
                                continue;
                            }
                            Ok(AuthenticateTransportPlan::UseCurrent) => false,
                            Ok(AuthenticateTransportPlan::ReconnectTransport) => true,
                        };
                        let mut result: Result<()> = Err(Error::ActorClosed);
                        for round in 0..2u8 {
                            if need_reconnect {
                                let reconnected = async {
                                // `connect()` and credential exchange are separated by host HTTP
                                // work. The transport may die in that gap. Reconnect only the
                                // transport here and authenticate with the supplied fresh token;
                                // do not run try_auto_reconnect with the persisted old token.
                                stop_inbound_task(&mut inbound_task).await;
                                if let Some((from, to)) =
                                    state.apply_transport_health(false)
                                {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        SdkEvent::ConnectionStateChanged { from, to },
                                    );
                                }
                                match timeout(
                                    state.connect_timeout_total(),
                                    state.connect(),
                                )
                                .await
                                {
                                    Ok(Ok(())) => {
                                        let connected_from =
                                            state.session_state.as_connection_state();
                                        state.session_state = SessionState::Connected;
                                        if connected_from
                                            != state.session_state.as_connection_state()
                                        {
                                            emit_sequenced_event(
                                                &actor_event_tx,
                                                &actor_event_history,
                                                &actor_event_seq,
                                                event_history_limit,
                                                SdkEvent::ConnectionStateChanged {
                                                    from: connected_from,
                                                    to: state
                                                        .session_state
                                                        .as_connection_state(),
                                                },
                                            );
                                        }
                                        Ok(())
                                    }
                                    Ok(Err(e)) => Err(e),
                                    Err(_) => Err(Error::Transport(
                                        "authenticate transport reconnect timeout".to_string(),
                                    )),
                                }
                                }
                                .await;
                                if let Err(e) = reconnected {
                                    result = Err(e);
                                    break;
                                }
                            }
                            let next_state = match state.session_state.can(Action::Authenticate) {
                                Ok(v) => v,
                                Err(e) => {
                                    result = Err(e);
                                    break;
                                }
                            };
                            result = state
                                .authenticate(user_id, token.clone(), device_id.clone())
                                .await;
                            if result.is_ok() {
                                state.session_state = next_state;
                                tracing::debug!(
                                    user_id = %user_id,
                                    state_after = ?state.session_state,
                                    "authenticate command completed"
                                );
                                break;
                            }
                            // 「没拿到应答」只否决这条连接，不否决这次登录。
                            //
                            // `plan_authenticate_transport` 拿 `is_connected()` 做判断，而那是
                            // 客户端自己的信念——对端或中间 CDN 单方面关掉之后，半开的 socket
                            // 照样报 connected，ConnAuth 就写进了没人读的管子。真机实测：那段
                            // 时间服务端**完全没有这次 ConnAuth 的记录**（它的连接处理器每次
                            // <1ms、CPU 4%），而同一分钟另一个账号 0.4 秒就认证成功。
                            //
                            // 所以先证伪那条连接：走上面同一段拆除流程重建，再试一次。
                            // 第二次仍然没有应答才是真失败。
                            if round == 0 && matches!(result, Err(Error::RequestUnanswered { .. })) {
                                tracing::warn!(
                                    user_id = %user_id,
                                    "authenticate got no response; rebuilding transport and retrying once"
                                );
                                need_reconnect = true;
                                continue;
                            }
                            break;
                        }
                        if result.is_ok() {
                            // 新 token 生效，解除 Terminal 闸门、重启 auto-reconnect。
                            // SessionState 已经在 can() 里转回 Authenticated（含 AccessTokenRefreshNeeded → Authenticated 路径）。
                            state.auth_terminal_fired = false;
                            state.should_auto_reconnect = true;
                            state.sync_coordinator.note_network_available();
                            // [P0 根因修复] 这条路径是「token 过期 → 刷新 → 重新 authenticate」的重连主链：
                            // try_auto_reconnect 因 10002 失败、transport 被换，真正成功的握手走这里。
                            // 此前这里 **没有重启 inbound task** → 新 transport 上没有事件订阅 →
                            // 服务端 push（消息/presence/typing）全部收不到，而 RPC（请求/响应）照常，
                            // 表现为「App 能发不能收 + presence/typing 失效」。必须与 try_auto_reconnect
                            // 成功分支对齐：① 重启 inbound task（订阅新 transport + bump epoch）。
                            start_inbound_task(&mut state, actor_cmd_tx.clone(), &mut inbound_task)
                                .await;
                            if realtime_trace_enabled() { eprintln!(
                                "[SDK_RECONNECT_OK] (cmd_authenticate path) new_epoch={} state={:?} uid={:?}",
                                state.inbound_epoch, state.session_state, state.current_uid
                            ); }
                            // ② 防御性 replay：刷新前缓冲的 push 帧立即重放。
                            if let Err(err) = state.replay_prelogin_inbound_frames().await {
                                eprintln!("[SDK.actor] cmd_authenticate: prelogin replay failed: {err}");
                            }
                            // 重连后重放活跃订阅：恢复 presence_changed / typing / room 广播。
                            state.replay_subscriptions().await;
                            // ③ resume sync：把过期/重连窗口期漏掉的增量补回来。
                            if state.session_state == SessionState::Authenticated
                                && state.bootstrap_completed
                            {
                                if let Err(err) = state.ensure_synced(|event| {
                                    emit_sequenced_event(
                                        &actor_event_tx,
                                        &actor_event_history,
                                        &actor_event_seq,
                                        event_history_limit,
                                        event,
                                    );
                                }).await {
                                    eprintln!("[SDK.actor] cmd_authenticate: resume sync failed: {err}");
                                }
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
                        } else if let Err(ref e) = result {
                            // 手动 authenticate 拿到 Terminal 错：和 retry_sleep 分支同走 forced_logout，
                            // UI 会收到 ForcedLogout 事件，走清 token + 回登录页流程。
                            if e.is_auth_terminal() {
                                trigger_forced_logout(
                                    &mut state,
                                    &mut inbound_task,
                                    &actor_event_tx,
                                    &actor_event_history,
                                    &actor_event_seq,
                                    event_history_limit,
                                    e,
                                    ForcedLogoutSource::Manual,
                                )
                                .await;
                            }
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
                        let result = match state.require_authenticated() {
                            Ok(()) => match timeout(
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
                        let result = match state.require_authenticated() {
                            Ok(()) => {
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
                        let result = match state.require_authenticated() {
                            Ok(()) => match timeout(
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
                        let result = match state.require_authenticated() {
                            Ok(()) => match timeout(
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
                        let result = match state.require_authenticated() {
                            Ok(()) => match timeout(
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
                        let result = match state.require_authenticated() {
                            Ok(()) => match timeout(
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
                    Command::Transfer { channel_id, route, body, timeout_ms, resp } => {
                        let result = match state.require_authenticated() {
                            Ok(()) => state
                                .transfer_channel(channel_id, route, body, timeout_ms)
                                .await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::RpcCall {
                        route,
                        body_json,
                        resp,
                    } => {
                        // 白名单路由（auth/login/register/refresh/send-sms 等）是 server 白名单接口，
                        // 自带 token 校验（refresh 用 refresh_token JWT 自签名，login 用密码），
                        // 不需要 access_token 已认证的 IM session。SDK 强制 require_authenticated
                        // 会让"10002 → refreshAccessToken 恢复"路径在 Connected 状态下失败。
                        // 只要 transport 已连上（Connected/LoggedIn/Authenticated），就允许调用。
                        let auth_check = if is_unauth_rpc_route(&route) {
                            match state.session_state {
                                SessionState::Connected
                                | SessionState::LoggedIn
                                | SessionState::Authenticated => Ok(()),
                                SessionState::Shutdown => Err(Error::Shutdown),
                                _ => Err(Error::InvalidState(format!(
                                    "operation requires connected transport (current: {:?})",
                                    state.session_state
                                ))),
                            }
                        } else {
                            state.require_authenticated()
                        };
                        let result = match auth_check {
                            Ok(()) => {
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
                        // 显式路径：解除退避窗口，并且要知道这一轮到底跑没跑。
                        //
                        // 以前这里拿 Ok 就当 bootstrap 完成了，还照发 BootstrapCompleted
                        // 事件——而被闸门挡掉时 bootstrap_completed 仍是 false，接下来
                        // 每一个 local-first 操作都报 InvalidState。宿主拿到的是「成功」，
                        // 看到的是全面失灵。
                        let ran = state.ensure_synced_explicit(|event| {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                event,
                            );
                        }).await;
                        let result = match ran {
                            Ok(true) => {
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
                                Ok(())
                            }
                            Ok(false) if state.bootstrap_completed => {
                                // 本来就已经 bootstrap 过，这次被合并掉——语义上确实
                                // 「已经就绪」，报成功没有骗人。
                                Ok(())
                            }
                            Ok(false) => Err(Error::InvalidState(format!(
                                "bootstrap sync did not run (sync state: {:?}); local-first operations are not available",
                                state.sync_coordinator.snapshot().readiness
                            ))),
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::EnsureSynced { resp } => {
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd ensure_synced");
                        }
                        let bootstrap_was_completed = state.bootstrap_completed;
                        let result = state.ensure_synced(|event| {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                event,
                            );
                        }).await;
                        if result.is_ok() && !bootstrap_was_completed && state.bootstrap_completed {
                            if let Some(user_id) = state
                                .current_uid
                                .as_deref()
                                .and_then(|uid| uid.parse::<u64>().ok())
                            {
                                emit_sequenced_event(
                                    &actor_event_tx,
                                    &actor_event_history,
                                    &actor_event_seq,
                                    event_history_limit,
                                    SdkEvent::BootstrapCompleted { user_id },
                                );
                            }
                        }
                        let _ = resp.send(result);
                    }
                    Command::GetSyncState { resp } => {
                        let _ = resp.send(Ok(state.sync_coordinator.snapshot()));
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
                        state.reset_reconnect_backoff();
                        let from_state = state.session_state.as_connection_state();
                        state.bootstrap_completed = false;
                        state.convergence_run = None;
                        state
                            .sync_coordinator
                            .reset(chrono::Utc::now().timestamp_millis());
                        state.pending_events.push(SdkEvent::SyncStateChanged {
                            state: state.sync_coordinator.snapshot(),
                        });
                        state.session_state = SessionState::Connected;
                        let result = if let Some(uid) = &state.current_uid {
                            let clear = state.storage.clear_session(uid.clone()).await;
                            let clear_uid = state.storage.clear_current_uid().await;
                            state.current_uid = None;
                            state.session_epoch += 1;
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
                            // 置发送中 + 写 outbox 在同一事务里（MESSAGE_SPEC §8.3）：
                            // 不会出现「消息说在发、队列里却没有」。
                            Ok(_) => match state
                                .storage
                                .outbox_enqueue(message_id, "message", 0, payload, None)
                                .await
                            {
                                Ok(()) => Ok(message_id),
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
                                    .outbox_peek("message", limit, i64::MAX)
                                    .await
                                    .map(|items| {
                                        items
                                            .into_iter()
                                            .map(|(message_id, _t, _c, payload, _r, _n)| QueueMessage {
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
                            Ok(_) => {
                                let mut removed = 0usize;
                                for id in message_ids {
                                    if state.storage.outbox_drop(id).await.is_ok() {
                                        removed += 1;
                                    }
                                }
                                Ok(removed)
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::EnqueueOutboundAttachment {
                        message_id,
                        route_key,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state
                                .storage
                                .outbox_enqueue(
                                    message_id,
                                    "attachment",
                                    0,
                                    Vec::new(),
                                    Some(route_key),
                                )
                                .await
                                .map(|()| message_id),
                            Err(e) => Err(e),
                        };
                        if result.is_ok() {
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::OutboundQueueUpdated {
                                    kind: "file".to_string(),
                                    action: "enqueue".to_string(),
                                    message_id: Some(message_id),
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
                    Command::PeekOutboundFiles { limit, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state
                                .storage
                                .outbox_peek("attachment", limit, i64::MAX)
                                .await
                                .map(|items| {
                                    items
                                        .into_iter()
                                        .map(|(message_id, _t, _c, payload, _r, _n)| QueueMessage {
                                            message_id,
                                            payload,
                                        })
                                        .collect()
                                }),
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::AckOutboundFiles { message_ids, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                let mut removed = 0usize;
                                for id in message_ids {
                                    if state.storage.outbox_drop(id).await.is_ok() {
                                        removed += 1;
                                    }
                                }
                                Ok(removed)
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
                    Command::CreateLocalMessageQueued {
                        input,
                        local_message_id,
                        command_type,
                        payload,
                        route_key,
                        resp,
                    } => {
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
                                            .create_local_message_queued(
                                                input,
                                                local_message_id,
                                                &command_type,
                                                payload,
                                                route_key,
                                            )
                                            .await
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            Err(e) => Err(e),
                        };
                        // 事务提交之后才发事件：失败时界面上不该出现这条消息，
                        // 更不该出现一条没有命令负责发送的「发送中」。
                        if let Ok(message_id) = result {
                            state.invalidate_channel_cache_with_reason(
                                channel_id,
                                channel_type,
                                "create_local_message_queued",
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
                                    status: 1,
                                    server_message_id: None,
                                },
                            );
                            let _ = actor_cmd_tx.try_send(Command::KickOutboundDrain);
                        }
                        let _ = resp.send(result);
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
                        // 自动 repair（MESSAGE_PROJECTION_SPEC §2.4）:读时**发现**,
                        // 后台**修**。
                        //
                        // 这里只做检测和入队,一次网络都不发:打开会话是交互路径,
                        // 让它等一串 around 请求,坏数据没修好之前会话先卡住,用户
                        // 付出的代价比问题本身大。队列那边负责 singleflight、有界
                        // 并发、超时与退避,修好后单独发一次 TimelineUpdated。
                        //
                        // 判据只取「本地无法自愈、且服务端一定能补」的三种:媒体行没有
                        // metadata（图片永远加载不出来）、已确认行缺 pts（排序无权威）、
                        // 时间戳落在毫秒纪元之前（单位写错的旧行）。文本行缺 metadata 是
                        // 正常的,不在其列。
                        if let Ok(ref messages) = result {
                            let image_type =
                                i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(2);
                            let video_type =
                                i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(3);
                            for m in messages {
                                let smid = m.server_message_id.unwrap_or(0);
                                if smid == 0 {
                                    continue; // 还没上服务端的本地行,不是投影损坏
                                }
                                let is_media =
                                    m.message_type == image_type || m.message_type == video_type;
                                let media_without_metadata = is_media
                                    && !crate::canonical_inbound::CanonicalInboundMessage::
                                        from_sync_entity(
                                            0, 0, 0, 1, 0, 0,
                                            String::new(),
                                            m.extra.clone(),
                                            0, 0,
                                        )
                                        .has_metadata();
                                let missing_pts = m.pts.unwrap_or(0) <= 0;
                                let bad_timestamp =
                                    m.created_at > 0 && m.created_at < 100_000_000_000;
                                if media_without_metadata || missing_pts || bad_timestamp {
                                    state.enqueue_projection_repair(
                                        channel_id,
                                        channel_type,
                                        smid,
                                    );
                                }
                            }
                        }
                        // 缺缩略图的图片/视频**只入队**，一次 await 都不做。
                        //
                        // 这里原本对整页消息串行 await `file/get_url`：一页几千张图就是几千次
                        // 网络往返卡死 actor，宿主的所有查询排在后面饿死——真机实测登录后
                        // `loadAllData` 的四个查询 4 分钟一个都没回，界面永远停在「数据初始化中」。
                        // 网络往返现在交给 tick 限量做（[`drain_thumbnail_backfill`]）。
                        if let Ok(ref messages) = result {
                            // 兜底值写的是这两个类型自己的判别值,不是随便挑的:写错的兜底一旦被
                            // 用上,就会拿 Voice/File 去当图片匹配。
                            let image_type = i32::try_from(ContentMessageType::Image.as_u32()).unwrap_or(2);
                            let video_type = i32::try_from(ContentMessageType::Video.as_u32()).unwrap_or(3);
                            for msg in messages {
                                if (msg.message_type == image_type || msg.message_type == video_type)
                                    && msg.thumb_status == 0
                                {
                                    state.enqueue_thumbnail_backfill(
                                        msg.message_id,
                                        channel_id,
                                        channel_type,
                                        msg.created_at,
                                        &msg.extra,
                                    );
                                }
                            }
                        }
                        let _ = resp.send(result);
                    }
                    Command::ListMessagesAround {
                        channel_id,
                        channel_type,
                        anchor_server_message_id,
                        before_limit,
                        after_limit,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .list_messages_around(
                                        channel_id,
                                        channel_type,
                                        anchor_server_message_id,
                                        before_limit,
                                        after_limit,
                                    )
                                    .await
                            }
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
                        message_seq,
                        resp,
                    } => {
                        let message_ctx = match state.current_uid_required() {
                            Ok(_) => state.storage.get_message_by_id(message_id).await.ok().flatten(),
                            Err(_) => None,
                        };
                        let result = match state.current_uid_required() {
                            Ok(_) => state
                                .storage
                                .mark_message_sent(message_id, server_message_id, message_seq)
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
                    Command::FetchChannelHistory {
                        channel_id,
                        channel_type,
                        before_server_message_id,
                        limit,
                        resp,
                    } => {
                        let result = state
                            .fetch_and_store_channel_history(
                                channel_id,
                                channel_type,
                                before_server_message_id,
                                limit,
                            )
                            .await;
                        if result.is_ok() {
                            state.invalidate_channel_cache_with_reason(
                                channel_id,
                                channel_type,
                                "fetch_channel_history",
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::FetchMessagesAround {
                        channel_id,
                        channel_type,
                        message_id,
                        before_limit,
                        after_limit,
                        resp,
                    } => {
                        let result = state
                            .fetch_and_store_messages_around(
                                channel_id,
                                channel_type,
                                message_id,
                                before_limit,
                                after_limit,
                            )
                            .await;
                        if result.is_ok() {
                            state.invalidate_channel_cache_with_reason(
                                channel_id,
                                channel_type,
                                "fetch_messages_around",
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::RepairMessageProjection {
                        channel_id,
                        channel_type,
                        server_message_id,
                        resp,
                    } => {
                        let result = state
                            .repair_message_projection(channel_id, channel_type, server_message_id)
                            .await;
                        // 修好了就让 UI 重查这一条:投影原地更新,message.id 不变,
                        // 未读不动,cursor 不动——repair 不是「收到新消息」。
                        if let Ok(Some(message_id)) = result {
                            state.invalidate_channel_cache_with_reason(
                                channel_id,
                                channel_type,
                                "repair_message_projection",
                            );
                            if let (Some(tx), Some(history), Some(seq)) = (
                                state.event_tx.as_ref(),
                                state.event_history.as_ref(),
                                state.event_seq.as_ref(),
                            ) {
                                emit_sequenced_event(
                                    tx,
                                    history,
                                    seq,
                                    state.event_history_limit,
                                    SdkEvent::TimelineUpdated {
                                        channel_id,
                                        channel_type,
                                        message_id,
                                        reason: "message_projection_repaired".to_string(),
                                    },
                                );
                            }
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
                    Command::UpdateThumbStatus {
                        message_id,
                        thumb_status,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.update_thumb_status(message_id, thumb_status).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpdateMediaDownloaded {
                        message_id,
                        downloaded,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.update_media_downloaded(message_id, downloaded).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::CreateLocalAttachmentPlaceholder { input, local_message_id, resp } => {
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
                        let _ = resp.send(result);
                    }
                    Command::FinalizeAttachmentAndEnqueue {
                        message_id,
                        content,
                        thumb_status,
                        route_key,
                        payload,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .finalize_attachment_and_enqueue(
                                        message_id,
                                        content,
                                        thumb_status,
                                        route_key,
                                        payload,
                                    )
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        // 事务提交后才发事件：失败时不该出现一条「已完成、没人发」
                        // 的附件消息。
                        match result {
                            Ok((channel_id, channel_type)) => {
                                state.invalidate_channel_cache_with_reason(
                                    channel_id,
                                    channel_type,
                                    "finalize_attachment_and_enqueue",
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
                                        status: 1,
                                        server_message_id: None,
                                    },
                                );
                                let _ = actor_cmd_tx.try_send(Command::KickOutboundDrain);
                                let _ = resp.send(Ok(()));
                            }
                            Err(e) => {
                                let _ = resp.send(Err(e));
                            }
                        }
                    }
                    Command::FinalizeLocalAttachment {
                        message_id,
                        content,
                        thumb_status,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .finalize_local_attachment(message_id, content, thumb_status)
                                    .await
                            }
                            Err(e) => Err(e),
                        };
                        match result {
                            Ok((channel_id, channel_type)) => {
                                state.invalidate_channel_cache_with_reason(
                                    channel_id,
                                    channel_type,
                                    "finalize_local_attachment",
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
                                        reason: "outbound_prep_complete".to_string(),
                                    },
                                );
                                let _ = resp.send(Ok(()));
                            }
                            Err(e) => {
                                let _ = resp.send(Err(e));
                            }
                        }
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
                    Command::DeleteMessageLocal { message_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.delete_message_local(message_id).await,
                            Err(e) => Err(e),
                        };
                        if let Ok(Some(stored)) = &result {
                            state.invalidate_channel_cache_with_reason(
                                stored.channel_id,
                                stored.channel_type,
                                "delete_message_local",
                            );
                            emit_sequenced_event(
                                &actor_event_tx,
                                &actor_event_history,
                                &actor_event_seq,
                                event_history_limit,
                                SdkEvent::TimelineUpdated {
                                    channel_id: stored.channel_id,
                                    channel_type: stored.channel_type,
                                    message_id,
                                    reason: "delete_local".to_string(),
                                },
                            );
                        }
                        let _ = resp.send(result);
                    }
                    Command::SetChannelHidden {
                        channel_id,
                        hidden,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.set_channel_hidden(channel_id, hidden).await,
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::DeleteChannelLocal { channel_id, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.delete_channel_local(channel_id).await,
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
                    Command::GetPeerReadPts {
                        channel_id,
                        channel_type,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .get_peer_read_pts(channel_id, channel_type)
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
                            Ok(_) => {
                                let user_id = input.user_id;
                                let avatar = input.avatar.clone();
                                let r = state.storage.upsert_user(input).await;
                                if r.is_ok() {
                                    // FFI 直写路径（profile fetch 等）也触发头像缓存。
                                    state.ensure_avatar_cached(user_id, &avatar);
                                }
                                r
                            }
                            Err(e) => Err(e),
                        };
                        let _ = resp.send(result);
                    }
                    Command::UpdateUserAlias { user_id, alias, resp } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => state.storage.update_user_alias(user_id, alias).await,
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
                    Command::ListFriendRequests {
                        outgoing,
                        statuses,
                        limit,
                        offset,
                        resp,
                    } => {
                        let result = match state.current_uid_required() {
                            Ok(_) => {
                                state
                                    .storage
                                    .list_friend_requests(outgoing, statuses, limit, offset)
                                    .await
                            }
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
                    Command::RecacheAvatar { user_id, url, resp } => {
                        match state.current_uid_required() {
                            Ok(_) => {
                                // 下载可能慢 → spawn，避免阻塞 actor loop；完成后回 oneshot。
                                let storage = state.storage.clone();
                                tokio::spawn(async move {
                                    let r =
                                        avatar_cache::recache_user_avatar(&storage, user_id, &url)
                                            .await;
                                    let _ = resp.send(r);
                                });
                            }
                            Err(e) => {
                                let _ = resp.send(Err(e));
                            }
                        }
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
                                        display_name: entry.display_name.clone(),
                                        username: entry.username.clone(),
                                        login_mode: entry.login_mode.clone(),
                                        login_identifier: entry.login_identifier.clone(),
                                    })
                                    .collect::<Vec<_>>()
                            },
                        );
                        let _ = resp.send(result);
                    }
                    Command::SetCurrentUid { uid, resp } => {
                        // 只能在「还没有活跃会话」时用来选定账号。对一个正在运行的
                        // actor 改 uid 不等于切换账号：transport / inbound / 订阅 /
                        // 缓存 全都还属于上一个账号，改完就是跨账号状态污染。
                        // 活跃会话要换账号走 SwitchLocalAccount。
                        let result = if matches!(
                            state.session_state,
                            SessionState::Connected
                                | SessionState::LoggedIn
                                | SessionState::Authenticated
                        ) {
                            Err(Error::InvalidState(
                                "set_current_uid on an active session; use switch_local_account"
                                    .to_string(),
                            ))
                        } else {
                            match state.storage.list_local_accounts().await {
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
                            }
                        };
                        let _ = resp.send(result);
                    }
                    // 原子账号切换：停旧会话 → 作废在途 → 清会话作用域状态 →
                    // 装载新账号。全程在这一个命令里完成，App 不再拼接
                    // setCurrentUid + shutdown + quickEnter 这三步——那三步之间
                    // 的窗口正是跨账号污染的来源。
                    Command::SwitchLocalAccount { uid, resp } => {
                        if actor_logs_enabled() {
                            eprintln!("[SDK.actor] loop: cmd switch_local_account uid={uid}");
                        }
                        // 这条请求已被取走：销账。计数器只表达「还在排队的切换」，
                        // 不销账的话每一轮同步开始时都会看到一个早已处理完的请求，
                        // 于是永远开不了工。
                        state.switch_processed.fetch_add(1, Ordering::SeqCst);
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let from_state = state.session_state.as_connection_state();
                        // 顺序是这条命令的全部要害：**所有可能失败的 IO 都排在拆除之前**。
                        //
                        // 反过来写（先拆旧会话、再存 uid / 读 session）会留下「半切换」：
                        // 旧会话已经销毁、全局 active uid 可能已指向新账号、而
                        // state.current_uid 还是旧的，且失败路径不发事件——UI 以为旧账号
                        // 一切正常，实际底下什么都没有了。原子的意思是「要么整件事发生，
                        // 要么什么都没发生」，不是「按顺序做完这几件事」。
                        //
                        // 现在的次序：① 校验目标存在 → ② 读目标 session → ③ 落盘 active
                        // uid。这三步任一失败都直接返回，旧会话**一根汗毛都没动**。全部
                        // 成功之后才 ④ 拆旧会话 ⑤ 提交内存状态——这两步不可失败。
                        // 残留风险只剩「③ 与 ⑤ 之间进程崩溃」，那种情况下重启会按新 uid
                        // 启动，本来就是用户要的结果。
                        let result = async {
                            // ① 目标账号必须存在。
                            let (_, entries) = state.storage.list_local_accounts().await?;
                            if !entries.iter().any(|entry| entry.uid == uid) {
                                return Err(Error::InvalidState(format!(
                                    "local account not found: {uid}"
                                )));
                            }
                            // ② 先把目标 session 读出来（只读，失败不影响任何现存状态）。
                            let snapshot = state.storage.load_session(uid.clone()).await?;
                            // ③ 落盘 active uid。到这里为止全部可回退。
                            state.storage.save_current_uid(uid.clone()).await?;
                            Ok(snapshot)
                        }
                        .await
                        .map(|snapshot| {
                            // ④ 停旧会话：撤销重连意图 → 停 inbound → 断 transport。
                            //    reset_session_scoped_state 已把 should_auto_reconnect 置 false，
                            //    所以这里断开不会被自动重连拉回来。
                            state.reset_session_scoped_state(now_ms);
                            (snapshot,)
                        });
                        let result = match result {
                            Err(e) => Err(e),
                            Ok((snapshot,)) => {
                                stop_inbound_task(&mut inbound_task).await;
                                if let Err(e) = state.disconnect().await {
                                    // 断开失败不阻断切换：目标是不再使用这条 transport，
                                    // 而它已经被丢弃（state.transport = None）。
                                    eprintln!(
                                        "[SDK.actor] switch_local_account: disconnect old session failed: {e}"
                                    );
                                }
                                state.transport = None;

                                // ⑤ 提交内存状态。
                                state.current_uid = Some(uid.clone());
                                state.session_epoch += 1;
                                state.bootstrap_completed =
                                    snapshot.map(|s| s.bootstrap_completed).unwrap_or(false);
                                state.session_state = SessionState::New;
                                Ok(())
                            }
                        };
                        // ③ 只在成功后发一次状态事件。失败不发：否则 UI 会收到一个
                        //    「切过去了」的假象，而实际仍停在旧账号。
                        if result.is_ok() {
                            let to_state = state.session_state.as_connection_state();
                            if from_state != to_state {
                                emit_sequenced_event(
                                    &actor_event_tx,
                                    &actor_event_history,
                                    &actor_event_seq,
                                    event_history_limit,
                                    SdkEvent::ConnectionStateChanged {
                                        from: from_state,
                                        to: to_state,
                                    },
                                );
                            }
                        }
                        let _ = resp.send(result);
                    }
                    Command::SetLocalAccountDisplayName {
                        uid,
                        display_name,
                        username,
                        login_mode,
                        login_identifier,
                        resp,
                    } => {
                        let result = state
                            .storage
                            .save_account_display_name(
                                uid,
                                display_name,
                                username,
                                login_mode,
                                login_identifier,
                            )
                            .await;
                        let _ = resp.send(result);
                    }
                    // 「滚到哪儿就取哪儿的缩略图」。
                    //
                    // 缩略图的自动下载本来只挂在消息**入站**（push / sync）那条路径上，
                    // 历史翻页拉回来的消息从来不触发——用户看到的就是「历史图片必须点
                    // 一下才加载」。UI 在气泡进入可视区时调这里，语义是 ensure：已经有
                    // 了就什么都不做，没有才去取。
                    Command::EnsureMessageThumbnail { message_id, resp } => {
                        let result = async {
                            let Some(msg) = state.storage.get_message_by_id(message_id).await? else {
                                return Ok(());
                            };
                            // thumb_status: 1=已下载, 3=协议层确无缩略图（终态）。
                            if msg.thumb_status == 1 {
                                return Ok(());
                            }
                            if msg.thumb_status == 3 {
                                // 3 是终态，但历史上有一批 3 是**误判**写下的：早先
                                // history 回填丢了 metadata，extra 是空串，当时的代码
                                // 把「我没看到缩略图字段」当成了「确实没有缩略图」。
                                // 写入侧已经修好，这些行却永远停在灰块上。
                                //
                                // 判据不另写一份，就用写入侧那条：现在再看一次，服务端
                                // 是否**明确**说了没有缩略图。它说了，3 就是对的；没说，
                                // 这条 3 就是当年那次误判，退回 0 重新走一遍。
                                //
                                // 放在这里而不是启动扫全表：用户滑到哪修到哪，代价只落
                                // 在真正被看到的那几条上。
                                let says_none =
                                    crate::canonical_inbound::CanonicalInboundMessage::from_sync_entity(
                                        0,
                                        0,
                                        msg.channel_id,
                                        msg.channel_type,
                                        0,
                                        0,
                                        String::new(),
                                        msg.extra.clone(),
                                        0,
                                        0,
                                    )
                                    .server_says_no_thumbnail();
                                if says_none {
                                    return Ok(());
                                }
                                tracing::info!(
                                    message_id = msg.message_id,
                                    "thumb_status=3 是早期误判（服务端未明确表示没有缩略图）：退回待重试"
                                );
                                state.storage.update_thumb_status(msg.message_id, 0).await?;
                            }

                            // 当年那次事故丢的不只是状态，还有 `extra` 本身——metadata
                            // 没写进去，所以本地这条消息里根本没有 thumbnail_file_id。
                            // 只把状态改回 0 是修了「允许重试」却没给它可重试的东西：
                            // 下一步照样解析不出 file_id，用户看到的还是灰块。
                            //
                            // 所以先按 server_message_id 定向重取这条消息的投影，把
                            // metadata 补回本地，再重读一次拿修好的 extra。
                            // 只在 extra 确实没有缩略图字段时才走这一趟网络：正常消息
                            // 一次都不会多花。
                            let msg = if State::extract_thumbnail_file_id(&msg.extra).is_none() {
                                match msg.server_message_id {
                                    Some(server_id) if server_id != 0 => {
                                        let repaired = state
                                            .repair_message_projection(
                                                msg.channel_id,
                                                msg.channel_type,
                                                server_id,
                                            )
                                            .await;
                                        if let Err(e) = &repaired {
                                            tracing::warn!(
                                                message_id = msg.message_id,
                                                error = %e,
                                                "缩略图修复：定向重取投影失败，这一轮放弃"
                                            );
                                        }
                                        // 重读：上面那趟把 metadata 写回了本地，手里这份
                                        // 还是修复前的旧值。
                                        match state.storage.get_message_by_id(message_id).await? {
                                            Some(fresh) => fresh,
                                            None => return Ok(()),
                                        }
                                    }
                                    // 没有 server_message_id 就无从定向重取（本地草稿 /
                                    // 从未落到服务端）。没有缩略图可下，直接收工。
                                    _ => return Ok(()),
                                }
                            } else {
                                msg
                            };

                            let owner_uid = state.current_uid_required()?.to_string();
                            let paths = state.storage.get_storage_paths().await?;
                            let user_root = PathBuf::from(&paths.user_root);
                            let Some(thumbnail_file_id) = State::extract_thumbnail_file_id(&msg.extra)
                            else {
                                // 修完投影仍然没有缩略图字段 = 这条消息确实没有缩略图。
                                // 这才是 3 该表达的意思，现在有依据写它了。
                                tracing::info!(
                                    message_id = msg.message_id,
                                    "修复投影后仍无缩略图字段：这条消息确实没有缩略图"
                                );
                                return Ok(());
                            };
                            let ticket = state.resolve_thumbnail_ticket(thumbnail_file_id).await;
                            State::spawn_auto_download_thumbnail(
                                owner_uid.clone(),
                                &msg.extra,
                                ticket,
                                &user_root,
                                msg.message_id,
                                msg.created_at,
                                msg.channel_id,
                                msg.channel_type,
                                state.storage.clone(),
                                Some(actor_event_tx.clone()),
                                Some(actor_event_history.clone()),
                                Some(actor_event_seq.clone()),
                                event_history_limit,
                            );
                            Ok(())
                        }
                        .await;
                        let _ = resp.send(result);
                    }
                    Command::WipeCurrentUserFull { resp } => {
                        let from_state = state.session_state.as_connection_state();
                        state.should_auto_reconnect = false;
                        state.reset_reconnect_backoff();
                        state.bootstrap_completed = false;
                        state.clear_presence_cache();
                        state.session_state = SessionState::Connected;
                        let result = match state.current_uid.clone() {
                            Some(uid) => {
                                let clear_uid = state.storage.clear_current_uid().await;
                                let wipe = state.storage.wipe_user_full(uid).await;
                                state.current_uid = None;
                                state.session_epoch += 1;
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
                        state.reset_reconnect_backoff();
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
            switch_requested: switch_requested_sdk,
            switch_wakeup: switch_wakeup_sdk,
            startup_error,
            snowflake,
            presence_cache,
            typing_throttle: Arc::new(StdMutex::new(HashMap::new())),
            data_dir: Arc::new(data_dir_for_self),
            file_route_key: Arc::new(file_route_key),
            download_manager: media_download::DownloadManager::new(),
            pending_media_jobs,
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

    /// 读取会话快照（精确阶段 + 账号 + 会话世代）。宿主对账连接状态时用它，
    /// 不要用 [`connection_state`]——那个值不带身份，无法防串号。
    pub async fn session_status(&self) -> Result<SessionStatus> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetSessionStatus { resp: resp_tx })
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

    /// 读取最近一次 Terminal 认证错误原因（ForcedLogout 快照）。
    /// `Connect` 成功后清空，可用于宿主冷启动诊断 / debug UI。
    pub async fn last_terminal_reason(&self) -> Result<Option<TerminalReason>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetLastTerminalReason { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 读取当前 access_token——SDK 内部 refresh 后拿到的就是最新权威值。
    ///
    /// 典型用法：宿主需要拿 access_token 调其它业务 HTTP API 时，**每次临用临取**，
    /// 不要在宿主侧缓存；订阅 `SdkEvent::TokenRefreshed` 仅用于 UI 状态更新，
    /// 真正要使用 token 时再调一次本方法，保证拿到的是最新值。
    ///
    /// 未登录时返回 `Ok(None)`。
    pub async fn get_current_access_token(&self) -> Result<Option<String>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetCurrentAccessToken { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<SdkEvent> {
        self.event_tx.subscribe()
    }

    /// Publish an event through the sequenced event bus.
    /// Used by auxiliary tasks (media download, etc.) that live outside the actor loop
    /// but need to emit on the same broadcast+history channel that consumers subscribe to.
    pub fn emit_event(&self, event: SdkEvent) {
        emit_sequenced_event(
            &self.event_tx,
            &self.event_history,
            &self.event_seq,
            self.event_history_limit,
            event,
        );
    }

    /// Configured data directory (may be empty if the caller relied on the default).
    pub fn data_dir(&self) -> &str {
        self.data_dir.as_str()
    }

    /// Start a Telegram-style streaming download for `message_id`.
    ///
    /// Resolves the canonical target directory via [`media_store`] and dispatches
    /// to the embedded [`DownloadManager`](media_download::DownloadManager).
    /// Emits [`SdkEvent::MediaDownloadStateChanged`] at ~5Hz plus on transitions.
    pub async fn start_message_media_download(
        &self,
        message_id: u64,
        download_url: String,
        mime: String,
        filename_hint: Option<String>,
        created_at_ms: i64,
    ) -> Result<()> {
        let snapshot = self.session_snapshot().await?.ok_or_else(|| {
            Error::InvalidState("session is empty; login/authenticate required".to_string())
        })?;
        let uid = snapshot.user_id;
        let root = std::path::Path::new(self.data_dir.as_str());
        let target_dir =
            media_store::ensure_attachment_dir(root, uid, message_id as i64, created_at_ms)
                .map_err(|e| Error::Storage(format!("ensure attachment dir failed: {e}")))?;
        let payload_filename =
            media_store::payload_filename_with_fallback(&mime, filename_hint.as_deref());
        self.download_manager
            .start(
                self.clone(),
                message_id,
                download_url,
                target_dir,
                payload_filename,
            )
            .await
            .map_err(Error::InvalidState)
    }

    /// Start a streaming download for an attachment-encrypted (v1) message.
    ///
    /// Resolves the canonical download ticket via [`file/get_url`](Self::resolve_file_download)
    /// (signed URL + `encryption_version` + `cek`) and dispatches it. On completion
    /// the blob is AES-GCM decrypted before becoming the on-disk file. Legacy
    /// plaintext messages (no `file_id`) should keep using
    /// [`start_message_media_download`](Self::start_message_media_download).
    pub async fn start_message_media_download_by_file_id(
        &self,
        message_id: u64,
        file_id: u64,
        mime: String,
        filename_hint: Option<String>,
        created_at_ms: i64,
    ) -> Result<()> {
        let snapshot = self.session_snapshot().await?.ok_or_else(|| {
            Error::InvalidState("session is empty; login/authenticate required".to_string())
        })?;
        let uid = snapshot.user_id;
        let ticket = self.resolve_file_download(file_id).await?;
        let root = std::path::Path::new(self.data_dir.as_str());
        let target_dir =
            media_store::ensure_attachment_dir(root, uid, message_id as i64, created_at_ms)
                .map_err(|e| Error::Storage(format!("ensure attachment dir failed: {e}")))?;
        let payload_filename =
            media_store::payload_filename_with_fallback(&mime, filename_hint.as_deref());
        self.download_manager
            .start_with_ticket(
                self.clone(),
                message_id,
                ticket,
                target_dir,
                payload_filename,
            )
            .await
            .map_err(Error::InvalidState)
    }

    pub async fn pause_message_media_download(&self, message_id: u64) {
        self.download_manager.pause(self, message_id).await;
    }

    pub async fn resume_message_media_download(&self, message_id: u64) {
        self.download_manager.resume(self, message_id).await;
    }

    pub async fn cancel_message_media_download(&self, message_id: u64) {
        self.download_manager.cancel(self, message_id).await;
    }

    pub async fn get_media_download_state(&self, message_id: u64) -> MediaDownloadState {
        self.download_manager.get_state(message_id).await
    }

    pub(crate) fn runtime_provider(&self) -> &RuntimeProvider {
        &self._runtime_provider
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

    /// Plan 2 媒体作业回传。宿主（Kotlin/iOS）收到 `SdkEvent::MediaJobRequested`
    /// 处理完成后调用此接口。直接操作共享 `pending_media_jobs` 表、不经 actor
    /// 命令通道——此时 actor 正阻塞在同一 oneshot rx 上。
    ///
    /// 已过期（超时被 actor 丢弃）或 `job_id` 未知时返回 `Err`。
    pub fn submit_media_job_result(&self, job_id: String, result: MediaJobResult) -> Result<()> {
        let sender = {
            let mut locked = self
                .pending_media_jobs
                .lock()
                .expect("pending_media_jobs poisoned");
            locked.remove(&job_id)
        };
        match sender {
            Some(tx) => tx
                .send(result)
                .map_err(|_| Error::InvalidState(format!("media job {job_id} receiver dropped"))),
            None => Err(Error::InvalidState(format!(
                "media job {job_id} not pending (expired or unknown)"
            ))),
        }
    }

    /// 注册 / 清除网址预览回调。未注册时发送 Link 消息仅带 URL，客户端显示空白缩略图。
    pub async fn set_link_preview_hook(&self, hook: Option<LinkPreviewHook>) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetLinkPreviewHook {
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

    /// Channel Transfer client→app RPC. Sends a wire `TransferRequest` (biz_type=19),
    /// awaits the matching `TransferResponse` (biz_type=20), and returns it decoded
    /// as `TransferReply`. `timeout_ms` is the per-call wire timeout; default 5000ms
    /// when zero. See `02-server/CHANNEL_TRANSFER_SPEC.md` v2.0 and
    /// `07-application/BOT_INTERACTION_SPEC.md` for routes (e.g. `bot/menu/get`).
    pub async fn transfer(
        &self,
        channel_id: u64,
        route: String,
        body: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<TransferReply> {
        self.ensure_running()?;
        let timeout_ms = if timeout_ms == 0 { 5000 } else { timeout_ms };
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::Transfer {
                channel_id,
                route,
                body,
                timeout_ms,
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

    /// 下载前按 file_id 调 `file/get_url` 解析出下载票据（含解密所需 cek/version）。
    /// 附件加密 v1：消息只带 file_id，下载权威信息（signed_url + cek + version）走此 RPC，
    /// 不依赖消息里存的 file_url（那只作 legacy fallback）。CEK 不进日志。
    pub async fn resolve_file_download(&self, file_id: u64) -> Result<ResolvedFileDownload> {
        let payload = FileGetUrlRequest {
            file_id,
            user_id: 0, // 服务端按鉴权上下文填充
        };
        let resp: FileGetUrlResponse = self.rpc_call_typed(routes::file::GET_URL, &payload).await?;
        if resp.file_url.trim().is_empty() {
            return Err(Error::Serialization(
                "decode file/get_url response: missing file_url".to_string(),
            ));
        }
        Ok(ResolvedFileDownload {
            url: resp.file_url,
            encryption_version: resp.encryption_version,
            cek: resp.cek,
        })
    }

    /// AVATAR_CACHE_SPEC §8: 头像上传前客户端预处理。
    ///
    /// decode（白名单 jpeg/png/webp，gif/损坏格式直接 Err，不消耗上传流量）→
    /// 中心裁剪正方形 → 边长 >480 缩放到 480x480（≤480 不放大）→ 编码 PNG
    /// 写临时文件，返回处理后路径。App 选图后先过它再走上传管道。
    pub async fn prepare_avatar_image(&self, src_path: String) -> Result<String> {
        // 直接同步调用:uniffi 的 async 桥在自己的 foreign executor 上 poll 本 future,
        // 没有 Tokio runtime 上下文,spawn_blocking 会 panic「no reactor running」。
        // 头像预处理是一次性 CPU 工作(≤480 图,数十 ms),App 已在协程里调用,
        // 短暂阻塞该协程线程可接受。
        // 输出写 data_dir/tmp（app 沙箱内保证可写）；Android 上 std temp_dir =
        // /data/local/tmp 无写权限，不能用（见 prepare_avatar_image_sync doc）。
        let out_dir = std::path::Path::new(self.data_dir()).join("tmp");
        avatar_cache::prepare_avatar_image_sync(std::path::Path::new(&src_path), &out_dir)
            .map(|p| p.to_string_lossy().to_string())
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

    pub async fn ensure_synced(&self) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EnsureSynced { resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn sync_state(&self) -> Result<SyncStateSnapshot> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetSyncState { resp: resp_tx })
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

    /// 把一条已有的附件消息重新排进 outbox（重试用）。
    ///
    /// payload 留空：附件字节留在托管路径上，drain 发送时自己读。
    pub async fn enqueue_outbound_attachment(
        &self,
        message_id: u64,
        route_key: String,
    ) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EnqueueOutboundAttachment {
                message_id,
                route_key,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn peek_outbound_files(&self, limit: usize) -> Result<Vec<QueueMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::PeekOutboundFiles {
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn ack_outbound_files(&self, message_ids: Vec<u64>) -> Result<usize> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::AckOutboundFiles {
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

    pub async fn send_link_message(&self, input: LinkMessageInput) -> Result<u64> {
        let url = input.url.trim().to_string();
        if url.is_empty() {
            return Err(Error::InvalidState("url is empty".to_string()));
        }
        let title = input.title.and_then(non_empty_trimmed);
        let description = input.description.and_then(non_empty_trimmed);
        let display_content = title.clone().unwrap_or_else(|| url.clone());
        let metadata = MessageMetadata::Link(LinkMetadata {
            url,
            title,
            description,
            thumbnail_file_id: input.thumbnail_file_id,
        });
        self.send_structured_message(
            input.channel_id,
            input.channel_type,
            input.from_uid,
            ContentMessageType::Link,
            display_content,
            metadata,
            input.options,
        )
        .await
    }

    pub async fn send_location_message(&self, input: LocationMessageInput) -> Result<u64> {
        let name = input.name.and_then(non_empty_trimmed);
        let address = input.address.and_then(non_empty_trimmed);
        let display_content = name
            .clone()
            .or_else(|| address.clone())
            .unwrap_or_else(|| format!("{},{}", input.latitude, input.longitude));
        let metadata = MessageMetadata::Location(LocationMetadata {
            latitude: input.latitude,
            longitude: input.longitude,
            coordinate_system: input.coordinate_system.and_then(non_empty_trimmed),
            name,
            address,
            poi_id: input.poi_id.and_then(non_empty_trimmed),
            poi_source: input.poi_source.and_then(non_empty_trimmed),
            thumbnail_file_id: input.thumbnail_file_id,
        });
        self.send_structured_message(
            input.channel_id,
            input.channel_type,
            input.from_uid,
            ContentMessageType::Location,
            display_content,
            metadata,
            input.options,
        )
        .await
    }

    pub async fn send_contact_card_message(&self, input: ContactCardMessageInput) -> Result<u64> {
        let metadata = MessageMetadata::ContactCard(ContactCardMetadata {
            user_id: input.user_id,
        });
        self.send_structured_message(
            input.channel_id,
            input.channel_type,
            input.from_uid,
            ContentMessageType::ContactCard,
            String::new(),
            metadata,
            input.options,
        )
        .await
    }

    async fn send_structured_message(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content_type: ContentMessageType,
        display_content: String,
        metadata: MessageMetadata,
        options: StructuredSendOptions,
    ) -> Result<u64> {
        debug_assert!(
            matches!(
                (&content_type, &metadata),
                (ContentMessageType::Link, MessageMetadata::Link(_))
                    | (ContentMessageType::Location, MessageMetadata::Location(_))
                    | (
                        ContentMessageType::ContactCard,
                        MessageMetadata::ContactCard(_)
                    )
            ),
            "structured message type and protocol metadata must match",
        );
        let metadata_value = metadata.to_inner_json_value();
        let envelope = LocalMessagePayloadEnvelope {
            content: display_content.clone(),
            metadata: Some(metadata_value.clone()),
            reply_to_message_id: options.in_reply_to_message_id.map(|id| id.to_string()),
            mentioned_user_ids: if options.mentioned_user_ids.is_empty() {
                None
            } else {
                Some(options.mentioned_user_ids)
            },
            message_source: None,
        };
        let extra = serde_json::to_string(&envelope).map_err(|e| {
            Error::Serialization(format!("encode structured message envelope: {e}"))
        })?;
        // 建消息与入队命令是一个事务：分开做的话第二步失败就留下一条
        // 「幽灵消息」——UI 永远显示发送中，却没有命令负责把它发出去。
        self.create_local_message_queued(
            NewMessage {
                channel_id,
                channel_type,
                from_uid,
                message_type: i32::try_from(content_type.as_u32()).unwrap_or(0),
                content: display_content.clone(),
                searchable_word: display_content,
                setting: 0,
                extra,
                mime_type: None,
                media_downloaded: false,
                thumb_status: 0,
            },
            None,
            "message",
            Vec::new(),
            None,
        )
        .await
    }

    /// 建消息并入队出站命令，**一个事务**（MESSAGE_SPEC §8.2）。
    ///
    /// 发送入口应当用它，而不是 `create_local_message` + `enqueue_outbound_*`：
    /// 后者是两个事务，第二步失败会留下永远发送中的幽灵消息。
    pub async fn create_local_message_queued(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
        command_type: &str,
        payload: Vec<u8>,
        route_key: Option<String>,
    ) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::CreateLocalMessageQueued {
                input,
                local_message_id,
                command_type: command_type.to_string(),
                payload,
                route_key,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn create_local_message_with_id(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
    ) -> Result<u64> {
        self.ensure_running()?;
        let input = State::normalize_new_message(input);
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

    /// 重发一条失败的消息。**消息生命周期编排属于 Core**（SDK_LAYERED spec §4.1），
    /// FFI 只做薄委托，不得维护第二套重试状态机。
    ///
    /// 分流依据是消息类型与其持久化阶段：
    /// - 附件（image/video/voice/file）→ 回到 **file queue**，从上传阶段重来。此前不分
    ///   类型一律进普通队列，送出去的是「本地路径当 content、metadata 为空」的消息，
    ///   服务端必 20006，用户点多少次重试都发不出去（2026-07-26 生产阻断）。
    /// - 其余 → 普通队列（drain 时从本地库重读消息，payload 不透明）。
    ///
    /// 附件源文件已不在时返回 [`Error::AttachmentSourceMissing`]，让上层提示重新选择，
    /// 而不是排一条注定被拒的消息。
    pub async fn retry_message(&self, message_id: u64) -> Result<u64> {
        let msg = self
            .get_message_by_id(message_id)
            .await?
            .ok_or_else(|| Error::InvalidState(format!("message not found: {message_id}")))?;

        if is_attachment_message_type(msg.message_type) {
            let path = attachment_local_path(&msg.content)
                .ok_or(Error::AttachmentSourceMissing { message_id })?;
            // 只证明源文件此刻可读，**不把字节读进来**：drain 会在真正发送时
            // 从这条托管路径读盘（payload 为空即走该分支）。把几十上百 MB 复制
            // 进 outbox 的 BLOB 列，等于同一份数据存两遍，还要跟着事务一起写。
            match std::fs::File::open(&path).and_then(|f| f.metadata()) {
                Ok(meta) if meta.len() > 0 => {}
                Ok(_) => return Err(Error::AttachmentSourceMissing { message_id }),
                Err(e) => {
                    // 路径存在但读不出来（权限/占用）同样是「没有可重传的源」。
                    tracing::warn!(error = %e, path = %path, "attachment retry: source unreadable");
                    return Err(Error::AttachmentSourceMissing { message_id });
                }
            }
            let route_key = self.file_route_key.as_ref().clone().ok_or_else(|| {
                Error::InvalidState("no endpoint configured for attachment retry".to_string())
            })?;
            return self
                .enqueue_outbound_attachment(message_id, route_key)
                .await;
        }

        self.enqueue_outbound_message(message_id, Vec::new()).await
    }

    /// file queue 的路由键：同一 endpoint 的附件共享一条有序队列。
    fn endpoint_route_key(ep: &ServerEndpoint) -> String {
        let scheme = match ep.protocol {
            TransportProtocol::Quic => "quic",
            TransportProtocol::Tcp => "tcp",
            TransportProtocol::WebSocket => {
                if ep.use_tls {
                    "wss"
                } else {
                    "ws"
                }
            }
        };
        format!("{scheme}://{}:{}", ep.host, ep.port)
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

    /// 以 anchor（server_message_id）为轴按显示排序读取本地上下文窗口
    /// （spec §5：around 回填后 UI 从本地重查渲染）。anchor 本地不存在返回空。
    pub async fn list_messages_around(
        &self,
        channel_id: u64,
        channel_type: i32,
        anchor_server_message_id: u64,
        before_limit: usize,
        after_limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListMessagesAround {
                channel_id,
                channel_type,
                anchor_server_message_id,
                before_limit,
                after_limit,
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

    pub async fn mark_message_sent(
        &self,
        message_id: u64,
        server_message_id: u64,
        message_seq: u32,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::MarkMessageSent {
                message_id,
                server_message_id,
                message_seq,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 回填式拉取频道历史（spec §3/§6）：结果已写入本地库，UI 应随后从本地重查渲染。
    pub async fn fetch_channel_history(
        &self,
        channel_id: u64,
        channel_type: i32,
        before_server_message_id: Option<u64>,
        limit: Option<u32>,
    ) -> Result<privchat_protocol::rpc::message::history::MessageHistoryResponse> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::FetchChannelHistory {
                channel_id,
                channel_type,
                before_server_message_id,
                limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// SDK-HISTORY-5（MESSAGE_HISTORY spec §2.5 / §2.5.1）：上滑加载更早历史。
    ///
    /// **架构（Telegram 式，读本地 / get 只补缺口）**：聊天页渲染读本地库为真源；本地翻到
    /// 最早时用 `message/history/get` 回填（带真实 pts，SDK-HISTORY-2 已做 upsert），再从
    /// 本地库重查返回。**gap 水位（has_more_before）由 SDK 持久化在 KV**——已到顶的会话
    /// 不再空打网络，部分缓存的会话据此触发回填；跨会话/重启有效。主窗口只向更早**连续**
    /// 扩展，绝不制造/渲染不连续区间（around 孤岛由 §5 独立处理）。
    ///
    /// [before_server_message_id] = 当前已显示最早一条的 server_message_id（翻页游标）。
    /// 返回本次回填的更早消息（本地重查、显示序 DESC）+ has_more_before（服务端是否还有更早）。
    pub async fn load_older_history(
        &self,
        channel_id: u64,
        channel_type: i32,
        before_server_message_id: u64,
        limit: u32,
    ) -> Result<OlderHistoryPage> {
        let gap_key = format!("__hist_gap__:{channel_type}:{channel_id}");
        // 读持久化 gap 态：不存在 → 视为未知（允许探一次）；has_more_before=false → 已到顶。
        let persisted = self
            .kv_get_local(gap_key.clone())
            .await?
            .and_then(|b| serde_json::from_slice::<HistGapState>(&b).ok());
        if matches!(&persisted, Some(s) if !s.has_more_before) {
            return Ok(OlderHistoryPage {
                messages: Vec::new(),
                has_more_before: false,
            });
        }
        // 网络回填：SDK 内部按 server_message_id upsert 本地库（带真实 pts）。
        let resp = self
            .fetch_channel_history(
                channel_id,
                channel_type,
                Some(before_server_message_id),
                Some(limit),
            )
            .await?;
        // 持久化新 gap 水位（服务端是否还有更早）。
        let new_state = HistGapState {
            has_more_before: resp.has_more,
        };
        let _ = self
            .kv_put_local(gap_key, serde_json::to_vec(&new_state).unwrap_or_default())
            .await;
        // 从本地库重读更早窗口（复用规范 StoredMessage 映射）；过滤掉 anchor 及更新的。
        let window = self
            .list_messages_around(
                channel_id,
                channel_type,
                before_server_message_id,
                limit as usize,
                0,
            )
            .await?;
        let older: Vec<StoredMessage> = window
            .into_iter()
            .filter(|m| {
                m.server_message_id
                    .map(|id| id < before_server_message_id)
                    .unwrap_or(false)
            })
            .collect();
        Ok(OlderHistoryPage {
            messages: older,
            has_more_before: resp.has_more,
        })
    }

    /// 打开会话（SDK-HISTORY-7）：本地为渲染真源，本地为空时补一次**最新**窗口。
    ///
    /// 这个入口此前根本不存在——`list_messages` 是纯本地读，打开一个本地没有消息的会话
    /// 只会显示「暂无聊天内容」，而上滑翻页救不了它：翻页需要一个已存在的锚点往前翻，
    /// 一条都没有时连起点都没有。
    ///
    /// 「没补过」和「补过、结果是空」必须分开记（[HistHydratedState]）。只看「本地是否为空」
    /// 的话，一个真正空的会话每次打开都会白打一次网络，永远收敛不了。
    ///
    /// 空会话就返回空。**不注入任何占位/问候消息**——那是伪造历史。
    pub async fn open_conversation(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: u32,
    ) -> Result<OpenConversationPage> {
        let gap_key = format!("__hist_gap__:{channel_type}:{channel_id}");
        let read_has_more = |raw: Option<Vec<u8>>| -> bool {
            raw.and_then(|b| serde_json::from_slice::<HistGapState>(&b).ok())
                .map(|s| s.has_more_before)
                // 没有水位记录 = 未知，按「可能还有」处理：宁可多给一次上滑机会，
                // 也不要谎称到顶把用户挡在门外。
                .unwrap_or(true)
        };

        // 1. 先读本地。有内容就直接渲染，不打网络——冷启动秒开靠的就是这一步。
        let local = self
            .list_messages(channel_id, channel_type, limit as usize, 0)
            .await?;
        if !local.is_empty() {
            return Ok(OpenConversationPage {
                messages: local,
                has_more_before: read_has_more(self.kv_get_local(gap_key).await?),
                fetched_from_server: false,
            });
        }

        // 2. 本地空。补过一次且结果就是空 → 这是个真的空会话，不再空转。
        let hydrated_key = format!("__hist_hydrated__:{channel_type}:{channel_id}");
        let hydrated = self
            .kv_get_local(hydrated_key.clone())
            .await?
            .and_then(|b| serde_json::from_slice::<HistHydratedState>(&b).ok())
            .map(|s| s.hydrated)
            .unwrap_or(false);
        if hydrated {
            return Ok(OpenConversationPage {
                messages: Vec::new(),
                has_more_before: false,
                fetched_from_server: false,
            });
        }

        // 3. 从没补过 → 拉最新一页。`before = None` 即「最新」，服务端本来就支持，
        //    与上滑翻页复用同一个 RPC（回填带真实 pts，SDK-HISTORY-2 已做 upsert）。
        let resp = self
            .fetch_channel_history(channel_id, channel_type, None, Some(limit))
            .await?;

        // 只有网络这一步成功了才落 hydrated：失败就落的话，一次超时会让这个会话
        // 永远显示「暂无聊天内容」，再也不会重试。
        let _ = self
            .kv_put_local(
                hydrated_key,
                serde_json::to_vec(&HistHydratedState { hydrated: true }).unwrap_or_default(),
            )
            .await;
        let _ = self
            .kv_put_local(
                gap_key,
                serde_json::to_vec(&HistGapState {
                    has_more_before: resp.has_more,
                })
                .unwrap_or_default(),
            )
            .await;

        // 4. 从本地库重读（回填已 upsert）。本地始终是渲染真源，不直接渲染 RPC 响应。
        let messages = self
            .list_messages(channel_id, channel_type, limit as usize, 0)
            .await?;
        Ok(OpenConversationPage {
            messages,
            has_more_before: resp.has_more,
            fetched_from_server: true,
        })
    }

    /// jump-to-message 上下文（spec §5）：before/anchor/after 完整消息已回填本地库。
    pub async fn fetch_messages_around(
        &self,
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        before_limit: Option<u32>,
        after_limit: Option<u32>,
    ) -> Result<privchat_protocol::rpc::message::history::MessageHistoryAroundResponse> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::FetchMessagesAround {
                channel_id,
                channel_type,
                message_id,
                before_limit,
                after_limit,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 定向修复一条投影损坏的消息（MESSAGE_PROJECTION_SPEC §2.4）。
    ///
    /// 「投影损坏」= 本地这一行是旧代码或半截写入留下的：metadata 丢了（图片永远
    /// 加载不出来）、时间单位错了、pts 缺失。修法是按 server_message_id 走
    /// `message/history/around` 拿回权威消息，重新跑 canonical projection，原地
    /// upsert。
    ///
    /// 契约：
    /// - `message.id` 不变（upsert 按 server_message_id 命中既有行）；
    /// - 不增加未读、不推进 sync cursor —— 这不是「收到新消息」；
    /// - 成功后发 `TimelineUpdated{reason="message_projection_repaired"}`，宿主据此重查。
    ///
    /// 返回本地 message id；服务端也没有这条消息时返回 None。
    pub async fn repair_message_projection(
        &self,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) -> Result<Option<u64>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RepairMessageProjection {
                channel_id,
                channel_type,
                server_message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 云端历史搜索（spec §4）。scope 由 channel_id 推导：Some=CHANNEL / None=GLOBAL。
    /// 命中是 snippet 投影，**不落本地库**；点击命中后调 fetch_messages_around。
    /// 服务端限频 300ms/user——调用方（UI）应 debounce 300–500ms 且忽略过期结果。
    pub async fn search_message_history(
        &self,
        query: &str,
        channel_id: Option<u64>,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<privchat_protocol::rpc::message::history::MessageHistorySearchResponse> {
        let req = privchat_protocol::rpc::message::history::MessageHistorySearchRequest {
            query: query.to_string(),
            scope: if channel_id.is_some() {
                "CHANNEL".to_string()
            } else {
                "GLOBAL".to_string()
            },
            channel_id,
            cursor,
            limit,
        };
        self.rpc_call_typed(
            privchat_protocol::rpc::routes::message_history::SEARCH,
            &req,
        )
        .await
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

    pub async fn update_thumb_status(&self, message_id: u64, thumb_status: i32) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpdateThumbStatus {
                message_id,
                thumb_status,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    pub async fn update_media_downloaded(&self, message_id: u64, downloaded: bool) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpdateMediaDownloaded {
                message_id,
                downloaded,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 本地发送附件的占位 INSERT：拿 DB 自增 id，但不 emit 任何事件。
    /// 调用方必须在文件写盘后调用 `finalize_local_attachment`，由 finalize 负责 emit，
    /// 这样 UI 只会看到一次完整态的气泡，不会有"空内容 → 有内容"的闪动。
    pub async fn create_local_attachment_placeholder(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
    ) -> Result<u64> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::CreateLocalAttachmentPlaceholder {
                input,
                local_message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 本地发送附件：文件写盘完毕后调用，一次性写回 content / thumb_status /
    /// media_downloaded，并 emit TimelineUpdated 让 UI 刷新气泡。
    pub async fn finalize_local_attachment(
        &self,
        message_id: u64,
        content: String,
        thumb_status: i32,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::FinalizeLocalAttachment {
                message_id,
                content,
                thumb_status,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 附件定稿 + 入队命令，**一个事务**。
    ///
    /// 附件链路应当用它，而不是 `finalize_local_attachment` + `send_attachment_*`：
    /// 后者是两步，中间崩溃会留下一条已完成、已显示、却没人负责发送的附件。
    pub async fn finalize_attachment_and_enqueue(
        &self,
        message_id: u64,
        content: String,
        thumb_status: i32,
        route_key: String,
        payload: Vec<u8>,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::FinalizeAttachmentAndEnqueue {
                message_id,
                content,
                thumb_status,
                route_key,
                payload,
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

    pub async fn delete_message_local(&self, message_id: u64) -> Result<Option<StoredMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteMessageLocal {
                message_id,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 设置本地 channel 隐藏标记（不触达服务端）。返回 true 表示命中行被更新。
    pub async fn set_channel_hidden(&self, channel_id: u64, hidden: bool) -> Result<bool> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetChannelHidden {
                channel_id,
                hidden,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 本地删除 channel：隐藏 + 清空所有相关消息。不触达服务端；附件文件由调用方清理。
    pub async fn delete_channel_local(&self, channel_id: u64) -> Result<Vec<StoredMessage>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::DeleteChannelLocal {
                channel_id,
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

    /// Get the locally persisted peer read pts for cold start.
    pub async fn get_peer_read_pts(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<u64>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::GetPeerReadPts {
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

    pub async fn update_user_alias(&self, user_id: u64, alias: Option<String>) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::UpdateUserAlias {
                user_id,
                alias,
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

    /// F-sync.2: 列出本地 friend 表的"申请态"行（非 accepted）。
    ///
    /// - `outgoing=true`：我发出的（`is_outgoing=true`）；`outgoing=false`：我收到的。
    /// - `statuses` 留空 → 默认 0/3/4/5 全要；传具体集合做过滤（如 `[0]` 只看 pending）。
    pub async fn list_friend_requests(
        &self,
        outgoing: bool,
        statuses: Vec<i16>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredFriend>> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::ListFriendRequests {
                outgoing,
                statuses,
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

    /// 显式头像 re-cache（CLIENT_GLOBAL_STATE §4.3 P2）：下载 `url` 到本地并强制落库，
    /// 返回 `(avatar_local_path, avatar_cached_url)`。失败不污染旧缓存。
    pub async fn recache_user_avatar(&self, user_id: u64, url: &str) -> Result<(String, String)> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::RecacheAvatar {
                user_id,
                url: url.to_string(),
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

    pub async fn kv_put_local(&self, key: String, value: Vec<u8>) -> Result<()> {
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

    pub async fn kv_get_local(&self, key: String) -> Result<Option<Vec<u8>>> {
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

    /// 原子切换本地账号。
    ///
    /// 停旧会话（inbound / transport / 订阅 / 缓存 / sync 协调器）并装载新账号，
    /// 全部在一个 actor 命令内完成。调用方**不要**再自己拼
    /// `set_current_uid` + `shutdown` + 重新登录：那几步之间存在一个窗口，旧会话
    /// 仍在跑而 uid 已经指向新账号，旧账号的事件会被当成新账号的状态处理。
    ///
    /// 返回后 session 处于 `New`，由调用方发起新账号的连接流程。
    pub async fn switch_local_account(&self, uid: String) -> Result<()> {
        self.ensure_running()?;
        // 先记一笔再投命令：actor 可能正卡在一轮慢同步里内联 await，不先让它让出，
        // 这条命令只能排在队尾干等。顺序反过来就等于没有这个机制。
        //
        // 计数器先于唤醒：即使 actor 此刻没在等（唤醒落空），事实已经记下了，
        // 下一轮同步开始前会看到它。
        self.switch_requested.fetch_add(1, Ordering::SeqCst);
        self.switch_wakeup.notify_waiters();
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SwitchLocalAccount { uid, resp: resp_tx })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 记录某个本地账号的展示名，供「切换账号」列表渲染。
    ///
    /// 必须显式写：每个账号的资料只存在自己的库里，当前账号读不到别人的，
    /// 不冗余这一份，列表就只能显示 uid。
    pub async fn set_local_account_display_name(
        &self,
        uid: String,
        display_name: Option<String>,
        username: Option<String>,
        login_mode: Option<String>,
        login_identifier: Option<String>,
    ) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::SetLocalAccountDisplayName {
                uid,
                display_name,
                username,
                login_mode,
                login_identifier,
                resp: resp_tx,
            })
            .await
            .map_err(|_| self.actor_channel_error())?;
        resp_rx.await.map_err(|_| self.actor_channel_error())?
    }

    /// 确保某条消息的缩略图已在本地（没有才下载）。
    ///
    /// 供 UI 在气泡进入可视区时调用——历史消息的缩略图不会被入站路径自动拉取，
    /// 不调这里就只能等用户点开原图。已下载或协议层确无缩略图时是 no-op。
    pub async fn ensure_message_thumbnail(&self, message_id: u64) -> Result<()> {
        self.ensure_running()?;
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(Command::EnsureMessageThumbnail {
                message_id,
                resp: resp_tx,
            })
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
    /// 缩略图回填队列的不变量——**直接调用生产方法**。
    ///
    /// 上一版这个测试在测试体里自己复制了一份 enqueue 判定，于是它只证明了那份副本
    /// 自洽，生产代码改坏了照样绿。
    ///
    /// 回归背景：`ListMessages` 处理器曾对整页消息串行 await `file/get_url`，一页几千张图
    /// 就是几千次网络往返锁死 actor。真机实测（账号 aa1112，群里 9487 条消息）登录后
    /// `loadAllData` 的四个查询 4 分钟一个都没返回，界面永远停在「数据初始化中」，
    /// 而 SDK 其实 20 秒时就已经 SYNC_READY。
    #[tokio::test(flavor = "current_thread")]
    async fn thumbnail_backfill_queue_is_bounded_deduplicated_and_account_scoped() {
        let (mut state, _dir) = new_seeded_state("thumb_queue_invariants").await;

        // 同一条消息被多次查询命中，只应占一个位置。
        for _ in 0..50 {
            state.enqueue_thumbnail_backfill(1001, 7, 2, 0, "{}");
        }
        assert_eq!(state.thumbnail_backfill_queue.len(), 1, "重复的 message_id 不得重复排队");

        // 洪水必须被截断，而不是无限增长——一个大群能轻易灌进上万条。
        for id in 2000..20_000u64 {
            state.enqueue_thumbnail_backfill(id, 7, 2, 0, "{}");
        }
        assert_eq!(
            state.thumbnail_backfill_queue.len(),
            super::THUMBNAIL_BACKFILL_QUEUE_LIMIT,
            "队列必须有界",
        );

        // 切号必须清空：队列里存的是上一个账号的 message_id，而磁盘 active uid
        // 已经指向新账号了，不清就是跨账号写。
        state.reset_session_scoped_state(0);
        assert!(
            state.thumbnail_backfill_queue.is_empty() && state.thumbnail_backfill_seen.is_empty(),
            "切号必须清空缩略图队列，否则旧任务会写进新账号",
        );

        // 单次 tick 的批量必须远小于队列上限，否则问题只是从「一次查询卡死」
        // 搬成「一次 tick 卡死」。
        assert!(
            super::THUMBNAIL_BACKFILL_BATCH_LIMIT < super::THUMBNAIL_BACKFILL_QUEUE_LIMIT / 10,
            "单次 tick 的批量必须远小于队列上限",
        );
    }

    /// 队首那条属于上一个账号时必须被丢弃，而且**不许**因此停止消费。
    #[tokio::test(flavor = "current_thread")]
    async fn drain_skips_items_from_a_previous_session() {
        let (mut state, _dir) = new_seeded_state("thumb_drain_epoch").await;
        state.enqueue_thumbnail_backfill(1, 7, 2, 0, "{}");
        state.session_epoch += 1; // 切号
        state.enqueue_thumbnail_backfill(2, 7, 2, 0, "{}");
        assert_eq!(state.thumbnail_backfill_queue.len(), 2);

        state.drain_thumbnail_backfill(|| true).await;
        assert!(
            state.thumbnail_backfill_queue.is_empty(),
            "旧世代那条应被丢弃而不是卡住队列",
        );
    }

    /// 命令一到就必须让位——**每一条之间**都要重新判断，不是开头查一次。
    #[tokio::test(flavor = "current_thread")]
    async fn drain_yields_to_pending_commands_between_items() {
        let (mut state, _dir) = new_seeded_state("thumb_drain_yield").await;
        for id in 1..=5u64 {
            state.enqueue_thumbnail_backfill(id, 7, 2, 0, "{}");
        }
        let before = state.thumbnail_backfill_queue.len();

        // 一开始就有命令 → 一条都不消费。
        state.drain_thumbnail_backfill(|| false).await;
        assert_eq!(
            state.thumbnail_backfill_queue.len(),
            before,
            "队列里有待处理命令时，一条都不该被消费",
        );

        // 关键的那半：第一条**处理完之后**才来命令。只在开头查一次的实现会把
        // 整批做完，这个断言就是用来钉死「每条之间都要重新查」的。
        let calls = std::cell::Cell::new(0u32);
        state
            .drain_thumbnail_backfill(|| {
                let n = calls.get();
                calls.set(n + 1);
                n == 0
            })
            .await;
        assert_eq!(
            state.thumbnail_backfill_queue.len(),
            before - 1,
            "第一条之后命令到达，就必须停在那里，只消费一条",
        );
    }

    /// history 回填必须把 `metadata` 带进 `extra`。
    ///
    /// 生产事故的回归:翻页拉回来的图片消息 extra 是空串,而 file_id /
    /// thumbnail_file_id / thumbnail_url 全在 metadata 里。缩略图触发点因此找不到
    /// 任何可下载的东西,判定「这条消息本来就没有缩略图」并写下 thumb_status=3 ——
    /// 那是个终态,此后永不重试,气泡永远是灰块。实测受影响设备上五条图片行全是
    /// extra_len=0 + thumb_status=3。
    #[test]
    fn history_item_carries_metadata_into_extra() {
        let mut metadata = serde_json::Map::new();
        metadata.insert("file_id".into(), serde_json::json!(7120));
        metadata.insert("thumbnail_file_id".into(), serde_json::json!(7119));
        metadata.insert(
            "thumbnail_url".into(),
            serde_json::json!("https://example.invalid/7119.webp"),
        );
        let item = privchat_protocol::rpc::message::history::MessageHistoryItem {
            message_id: 604_621_803_637_178_368,
            channel_id: 45,
            sender_id: 100_000_028,
            content: "[图片]".to_string(),
            message_type: "image".to_string(),
            timestamp: 1_785_148_271_317,
            message_seq: Some(14),
            reply_to_message_id: None,
            metadata: Some(metadata),
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };

        let input = State::history_item_to_upsert_input(&item, 1);

        // 落库的是发送时间,不是本机现在几点。
        assert_eq!(input.timestamp, 1_785_148_271_317);
        assert_eq!(input.pts, 14);
        // 类型判别值:Image = 2。触发缩略图下载的两个点都按这个值匹配。
        assert_eq!(input.message_type, 2);
        // 而且 extra 里的东西必须真的能被缩略图路径找到——断言「extra 非空」是
        // 不够的,那正是这个 bug 骗过所有人的方式。
        assert_eq!(
            State::extract_thumbnail_file_id(&input.extra),
            Some(7119),
            "缩略图下载靠这个 file_id;取不到就会被判成「没有缩略图」"
        );
        assert_eq!(
            State::extract_thumbnail_url(&input.extra).as_deref(),
            Some("https://example.invalid/7119.webp"),
        );
    }

    use super::{
        channel_prefs_key, decode_channel_prefs, decode_group_settings_cache, error_codes,
        group_settings_key, outbound_queue_ready, plan_authenticate_transport, plan_connect,
        Action, AuthErrorKind, AuthenticateTransportPlan, CanonicalTimelineEvent, Command,
        ConnectPlan, ConnectionState, ContentMessageType, Error, ErrorCode, LoginResult,
        MessageCachePolicy, NetworkHint, NewMessage, PresenceStatus, PrivchatConfig, PrivchatSdk,
        ResumeEscalationScope, ResumeFailureClass, ResumeFailureTarget, SdkEvent, ServerCommit,
        SessionState, State, SyncCoordinator, UpsertChannelInput, UpsertFriendInput,
        UpsertGroupInput, UpsertGroupMemberInput, UpsertMessageReactionInput,
        UpsertRemoteMessageInput, UpsertUserInput, NETWORK_DISCONNECTED_MESSAGE,
    };
    use crate::local_store::LocalStore;
    use crate::receive_pipeline::ReceivePipeline;
    use crate::storage_actor::StorageHandle;
    use privchat_protocol::presence::{
        PresenceBatchStatusResponse, PresenceChangedNotification, PresenceSnapshot,
    };
    use privchat_protocol::rpc::sync::SyncEntityItem;
    use privchat_protocol::{
        EntityInvalidation, EntityInvalidationBatch, EntityMutationHint, FlatBufferMessage,
        PushMessageRequest, ENTITY_INVALIDATION_PUSH_TOPIC_V1,
    };
    use serde::Serialize;
    use std::collections::{HashMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use tokio::sync::oneshot;

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

    #[test]
    fn legacy_message_envelope_is_normalized_at_storage_boundary() {
        let serialized = serde_json::json!({
            "content": "归一化后的正文",
            "mentioned_user_ids": [],
            "reply_to_message_id": "600997771041832960"
        })
        .to_string();
        let (content, extra) =
            State::normalized_message_content_and_extra(&serde_json::Value::String(serialized));
        assert_eq!(content, "归一化后的正文");
        let extra = extra.expect("legacy envelope must remain available as message extra");
        assert!(extra.contains("600997771041832960"));
    }

    #[test]
    fn user_authored_json_text_is_not_unwrapped() {
        let literal = r#"{"content":"literal user JSON"}"#;
        let (content, extra) = State::normalized_message_content_and_extra(
            &serde_json::Value::String(literal.to_string()),
        );
        assert_eq!(content, literal);
        assert!(extra.is_none());
    }

    #[test]
    fn outbound_legacy_envelope_is_split_into_content_and_extra() {
        let input = NewMessage {
            content: serde_json::json!({
                "content": "正文",
                "mentioned_user_ids": [],
                "reply_to_message_id": "600997771041832960"
            })
            .to_string(),
            searchable_word: "legacy envelope".to_string(),
            extra: String::new(),
            ..NewMessage::default()
        };
        let normalized = State::normalize_new_message(input);
        assert_eq!(normalized.content, "正文");
        assert_eq!(normalized.searchable_word, "正文");
        assert!(normalized.extra.contains("600997771041832960"));
    }

    // ==================== 账号切换：真实命令级故障注入 ====================
    //
    // 上面那组走的是 State 辅助函数。这组必须跑**真的 actor 命令**：Codex 复审
    // 指出「先拆旧会话、再落盘」的顺序会留下半切换状态，而这条不变量只有让
    // 真实的 IO 失败才验得到——传一个不存在的 uid 走的是更早的校验分支，
    // 改坏顺序也照样提前返回，抓不到回归。

    /// 目标账号的 session 读不出来时：整条切换必须失败，且旧账号**一点都没动**。
    ///
    /// 修复前的顺序是「清会话 → 停 inbound → 断 transport → 存 uid → 读 session」，
    /// 于是读失败时旧会话已经销毁、磁盘 active uid 已经指向新账号、而
    /// state.current_uid 还是旧的，UI 还以为一切正常。
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_switch_leaves_the_old_account_intact() {
        let dir = unique_test_dir("switch-cmd-load-fails");
        let store = LocalStore::open_at(dir.clone()).expect("open local store");
        for uid in ["10001", "10002"] {
            let login = LoginResult {
                user_id: uid.parse().unwrap(),
                token: "token".to_string(),
                device_id: "device".to_string(),
                refresh_token: None,
                expires_at: 0,
            };
            store.save_login(uid, &login).expect("seed login");
            store
                .set_bootstrap_completed(uid, true)
                .expect("seed bootstrap");
        }
        // 目标账号的 session 密文写坏 → load_session 必然失败。
        store
            .corrupt_session_for_test("10002")
            .expect("corrupt target session");
        store.save_current_uid("10001").expect("seed active uid");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("select seeded account");

        let switched = sdk.switch_local_account("10002".to_string()).await;
        assert!(
            switched.is_err(),
            "目标 session 读不出来却报成功 —— 调用方会以为已经切过去了"
        );

        // 磁盘上的 active uid 没变——否则重启会启到那个切不过去的账号。
        let accounts = sdk.list_local_accounts().await.expect("list accounts");
        let active = accounts.iter().find(|a| a.is_active).map(|a| a.uid.clone());
        assert_eq!(
            active.as_deref(),
            Some("10001"),
            "切换失败后磁盘 active uid 已经指向新账号：重启就会落到半切换状态"
        );
        // 旧账号仍然可用（bootstrap 状态还在，说明会话没被拆掉）。
        assert!(
            sdk.is_bootstrap_completed().await.expect("query bootstrap"),
            "切换失败却把旧会话拆了"
        );
    }

    /// 目标账号根本不存在：同样必须整条失败且不留痕迹。
    #[tokio::test(flavor = "current_thread")]
    async fn switching_to_an_unknown_account_changes_nothing() {
        let (sdk, _dir) = new_seeded_sdk("switch-cmd-unknown").await;

        let switched = sdk.switch_local_account("99999".to_string()).await;
        assert!(switched.is_err());

        let accounts = sdk.list_local_accounts().await.expect("list accounts");
        assert_eq!(
            accounts
                .iter()
                .find(|a| a.is_active)
                .map(|a| a.uid.as_str()),
            Some("10001"),
        );
        assert!(sdk.is_bootstrap_completed().await.expect("query bootstrap"));
    }

    /// 成功路径：切过去之后当前账号确实变了，且新账号的 bootstrap 状态被装载。
    #[tokio::test(flavor = "current_thread")]
    async fn a_successful_switch_moves_the_current_account() {
        let dir = unique_test_dir("switch-cmd-ok");
        let store = LocalStore::open_at(dir.clone()).expect("open local store");
        for (uid, bootstrapped) in [("10001", true), ("10002", false)] {
            let login = LoginResult {
                user_id: uid.parse().unwrap(),
                token: "token".to_string(),
                device_id: "device".to_string(),
                refresh_token: None,
                expires_at: 0,
            };
            store.save_login(uid, &login).expect("seed login");
            store
                .set_bootstrap_completed(uid, bootstrapped)
                .expect("seed bootstrap");
        }
        store.save_current_uid("10001").expect("seed active uid");
        drop(store);

        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("select seeded account");

        sdk.switch_local_account("10002".to_string())
            .await
            .expect("switch should succeed");

        let accounts = sdk.list_local_accounts().await.expect("list accounts");
        assert_eq!(
            accounts
                .iter()
                .find(|a| a.is_active)
                .map(|a| a.uid.as_str()),
            Some("10002"),
        );
        // 新账号自己的 bootstrap 状态，不是继承旧账号的。
        assert!(
            !sdk.is_bootstrap_completed().await.expect("query bootstrap"),
            "装载的是旧账号的 bootstrap 状态"
        );
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
            transport_events: Arc::new(tokio::sync::Mutex::new(None)),
            session_state: SessionState::Authenticated,
            bootstrap_completed: true,
            sync_coordinator: SyncCoordinator::new(),
            snowflake: Arc::new(
                snowflake_me::Snowflake::builder()
                    .machine_id(&|| Ok((std::process::id() as u16) & 0x1f))
                    .data_center_id(&|| Ok((chrono::Utc::now().timestamp_millis() as u16) & 0x1f))
                    .finalize()
                    .expect("test snowflake"),
            ),
            storage,
            skip_inbound_materialization_for_load_testing: false,
            current_uid: Some("10001".to_string()),
            session_epoch: 0,
            thumbnail_backfill_queue: VecDeque::new(),
            thumbnail_backfill_seen: std::collections::HashSet::new(),
            should_auto_reconnect: false,
            reconnect_attempt: 0,
            next_reconnect_at: None,
            auth_terminal_fired: false,
            inbound_epoch: 0,
            last_resume_synced: None,
            last_anti_entropy_at: Instant::now(),
            convergence_run: None,
            resume_run_id: 0,
            anti_entropy_jitter: Duration::ZERO,
            room_seen_msg_ids: HashMap::new(),
            last_terminal_reason: None,
            network_hint: NetworkHint::Unknown,
            receive_pipeline: ReceivePipeline::default(),
            last_sync_queued: 0,
            last_sync_dropped_duplicates: 0,
            last_sync_entity_events: Vec::new(),
            video_process_hook: None,
            link_preview_hook: None,
            last_tmp_cleanup_day: None,
            pending_events: Vec::new(),
            message_cache_policy: MessageCachePolicy::default(),
            channel_message_cache: HashMap::new(),
            channel_cache_generation: HashMap::new(),
            switch_requested: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            switch_processed: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            switch_wakeup: Arc::new(tokio::sync::Notify::new()),
            sync_stall_for_test: None,
            channel_cache_lru: VecDeque::new(),
            channel_cache_total_bytes: 0,
            cache_debug_log: false,
            cache_hit_count: 0,
            cache_miss_count: 0,
            pending_prelogin_inbound_frames: Vec::new(),
            presence_cache: Arc::new(StdMutex::new(HashMap::new())),
            event_tx: None,
            event_history: None,
            event_seq: None,
            event_history_limit: 0,
            pending_media_jobs: Arc::new(StdMutex::new(HashMap::new())),
            active_subscriptions: HashMap::new(),
            avatar_cache: crate::avatar_cache::AvatarCacheManager::default(),
            repair_queue: std::collections::VecDeque::new(),
            repair_seen: std::collections::HashSet::new(),
            repair_backoff: std::collections::HashMap::new(),
        };
        (state, dir)
    }

    use super::{plan_anti_entropy_page, AntiEntropyObservation, AntiEntropyPage};
    use crate::sync_coordinator::{Readiness, SyncRunKind};

    /// 收敛判定：`Ok(0)` 曾被当成「全账号收敛」，但它只代表本页修了 0 条。
    /// 游标还停在中间时宣布 Converged，后面的频道就再也不会被检查。
    #[test]
    fn convergence_requires_a_full_clean_cycle() {
        // 本页干净，但还没走完一圈 —— 不能收敛
        let mid_cycle = AntiEntropyPage {
            page_scanned: 100,
            stale_found: 0,
            channels_repaired: 0,
            messages_applied: 0,
            deferred: 0,
            unknown_channels: 0,
            cycle_completed: false,
        };
        assert!(!mid_cycle.is_converged(), "游标停在中间就宣布收敛了");

        // 走完一圈但有 stale 因预算被推迟 —— 不能收敛
        let deferred = AntiEntropyPage {
            page_scanned: 8,
            stale_found: 9,
            channels_repaired: 8,
            messages_applied: 80,
            deferred: 1,
            unknown_channels: 0,
            cycle_completed: false,
        };
        assert!(!deferred.is_converged(), "还有 stale 没修就宣布收敛了");

        // 走完一圈且全部处理干净 —— 才算收敛
        let clean = AntiEntropyPage {
            page_scanned: 100,
            stale_found: 3,
            channels_repaired: 3,
            // 应用消息数与频道数无关，不能拿它参与收敛判定。
            messages_applied: 37,
            deferred: 0,
            unknown_channels: 0,
            cycle_completed: true,
        };
        assert!(clean.is_converged());

        // 一个频道都没有：走完一圈，算收敛
        let empty = AntiEntropyPage {
            page_scanned: 0,
            stale_found: 0,
            channels_repaired: 0,
            messages_applied: 0,
            deferred: 0,
            unknown_channels: 0,
            cycle_completed: true,
        };
        assert!(empty.is_converged());
    }

    #[test]
    fn thousand_channels_with_three_stale_only_repairs_three() {
        let stale = [17_u64, 511, 999];
        let observations: Vec<_> = (1_u64..=1000)
            .map(|channel_id| AntiEntropyObservation {
                key: (channel_id, 1),
                local_pts: 10,
                server_pts: Some(if stale.contains(&channel_id) { 11 } else { 10 }),
            })
            .collect();

        let mut repaired = Vec::new();
        for page in observations.chunks(100) {
            let plan = plan_anti_entropy_page(page, 8);
            assert_eq!(plan.consumed, 100);
            assert_eq!(plan.deferred, 0);
            repaired.extend(plan.repair.into_iter().map(|(channel_id, _)| channel_id));
        }

        assert_eq!(repaired, stale);
    }

    /// 服务端不认识的频道**不得**卡住游标。
    ///
    /// 真机实测(429 会话账号)：`server_pts=None` 时原实现 break 且不推进
    /// `last_consumed`，游标永远停在该频道之前，每轮从它重来 —— 日志刷出
    /// `scanned=0 deferred=1` 每 80ms 一次、永不收敛。
    #[test]
    fn unknown_server_channel_does_not_stall_the_cursor() {
        let observations = vec![
            // 服务端没有它（已删除 / 非消息频道）
            AntiEntropyObservation {
                key: (7, 1),
                local_pts: 3,
                server_pts: None,
            },
            // 它后面还有真正需要修的
            AntiEntropyObservation {
                key: (8, 1),
                local_pts: 3,
                server_pts: Some(9),
            },
        ];

        let plan = plan_anti_entropy_page(&observations, 8);
        assert_eq!(
            plan.deferred, 0,
            "服务端缺失被当成了「待修」，会让收敛永不完成"
        );
        assert_eq!(
            plan.last_consumed,
            Some((8, 1)),
            "游标没能推进到本页末尾，下一轮会从同一个频道重来"
        );
        assert_eq!(plan.repair, vec![(8, 1)], "缺失频道后面的 stale 被跳过了");
    }

    #[test]
    fn budget_stops_before_the_first_deferred_channel() {
        let observations: Vec<_> = (1_u64..=12)
            .map(|channel_id| AntiEntropyObservation {
                key: (channel_id, 1),
                local_pts: 0,
                server_pts: Some(1),
            })
            .collect();

        let first = plan_anti_entropy_page(&observations, 8);
        assert_eq!(first.repair.len(), 8);
        assert_eq!(first.last_consumed, Some((8, 1)));
        assert_eq!(first.deferred, 1);

        let second = plan_anti_entropy_page(&observations[8..], 8);
        assert_eq!(second.repair.len(), 4);
        assert_eq!(second.last_consumed, Some((12, 1)));
        assert_eq!(second.deferred, 0);
    }

    // ==================== 账号切换：会话作用域隔离 ====================
    //
    // 2026-07-28 真机 P0 的回归门禁。当时的「切换」只改 current_uid +
    // bootstrap_completed，旧会话原地不动，于是旧账号的失败被解释成新账号的状态。
    // 下面几条盯的就是「切完之后，属于上一个 owner 的东西一样都不许留下」。

    /// 切账号必须推进 inbound epoch —— 这是丢弃旧账号在途帧的唯一判据。
    ///
    /// 没有它，A 的迟到 push 会在 B 已经是 current_uid 时落库，直接写进 B 的会话。
    #[tokio::test(flavor = "current_thread")]
    async fn switching_accounts_invalidates_in_flight_inbound_frames() {
        let (mut state, _dir) = new_seeded_state("switch-epoch").await;
        state.inbound_epoch = 7;
        let stale_epoch = state.inbound_epoch;

        state.reset_session_scoped_state(1_000);

        assert_ne!(
            state.inbound_epoch, stale_epoch,
            "epoch 没推进：A 的在途帧会被当成 B 的消息"
        );
        // actor loop 的丢弃判据就是这个不等式。
        assert!(stale_epoch != state.inbound_epoch);
    }

    /// A 的同步失败不得把 B 拖进重连：切换会撤销自动重连意图并清空退避。
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_sync_cannot_drive_reconnect_after_switching_to_b() {
        let (mut state, _dir) = new_seeded_state("switch-no-reconnect").await;
        // A 正在重连 + sync 已经失败进了退避。
        state.should_auto_reconnect = true;
        state.reconnect_attempt = 5;
        state
            .sync_coordinator
            .begin(SyncRunKind::Resume, 0)
            .unwrap();
        state.sync_coordinator.fail(
            SyncRunKind::Resume,
            false,
            Some(9),
            "transport error: disconnected".to_string(),
            0,
        );

        state.reset_session_scoped_state(1_000);

        assert!(
            !state.should_auto_reconnect,
            "切换后仍开着自动重连：A 的失败会继续驱动 B 的重连"
        );
        assert_eq!(state.reconnect_attempt, 0);
        assert_eq!(
            state.sync_coordinator.snapshot().readiness,
            Readiness::Disconnected
        );
        assert_eq!(state.sync_coordinator.snapshot().attempt, 0);
    }

    /// 切回来必须能**立刻**开跑，不继承上一个账号的退避/终态。
    ///
    /// 真机那次正是这里：切回 A 之后必须杀进程才能恢复。
    #[tokio::test(flavor = "current_thread")]
    async fn switching_back_can_sync_immediately_without_a_cold_start() {
        let (mut state, _dir) = new_seeded_state("switch-back").await;
        // 上一个账号留下一个终态失败 + 一段退避。
        state
            .sync_coordinator
            .begin(SyncRunKind::Resume, 0)
            .unwrap();
        state.sync_coordinator.fail(
            SyncRunKind::Resume,
            true,
            Some(10_002),
            "token expired".to_string(),
            0,
        );

        state.reset_session_scoped_state(1_000);

        assert!(
            state
                .sync_coordinator
                .begin(SyncRunKind::Bootstrap, 1_001)
                .is_ok(),
            "切回来还被上一个账号的终态/退避挡着 —— 只能靠冷启动恢复"
        );
    }

    /// 会话作用域数据全部归零：订阅、presence、消息缓存、room 去重、repair 队列。
    ///
    /// 逐项断言而不是只看一两个：这份清单本身就是修复内容，漏一项就是下一次串号。
    #[tokio::test(flavor = "current_thread")]
    async fn switching_clears_every_session_scoped_structure() {
        let (mut state, _dir) = new_seeded_state("switch-clear").await;
        state
            .active_subscriptions
            .insert((45, 1), Some("tok".into()));
        state.update_presence_cache(&[]);
        state
            .room_seen_msg_ids
            .insert(45, VecDeque::from(vec![604_621_803_637_178_368]));
        state.channel_cache_total_bytes = 4096;
        state.cache_hit_count = 12;
        state
            .repair_queue
            .push_back((1, 45, 604_621_803_637_178_368));
        state.repair_seen.insert((1, 45, 604_621_803_637_178_368));
        state
            .repair_backoff
            .insert((1, 45, 1), (3, std::time::Instant::now()));
        state
            .pending_prelogin_inbound_frames
            .push((7, vec![1, 2, 3]));
        state.last_resume_synced = Some((7, Instant::now()));
        state.last_sync_queued = 9;
        state.pending_events.push(SdkEvent::SyncStateChanged {
            state: state.sync_coordinator.snapshot(),
        });

        state.reset_session_scoped_state(1_000);

        assert!(state.active_subscriptions.is_empty(), "订阅未清");
        assert!(state.room_seen_msg_ids.is_empty(), "room 去重表未清");
        assert!(state.channel_message_cache.is_empty(), "消息缓存未清");
        assert_eq!(state.channel_cache_total_bytes, 0, "缓存字节数未清");
        assert_eq!(state.cache_hit_count, 0, "缓存计数未清");
        assert!(state.repair_queue.is_empty(), "repair 队列未清");
        assert!(state.repair_seen.is_empty(), "repair singleflight 未清");
        assert!(state.repair_backoff.is_empty(), "repair 退避未清");
        assert!(
            state.pending_prelogin_inbound_frames.is_empty(),
            "登录前缓冲帧未清：会被 replay 进新账号"
        );
        assert!(state.last_resume_synced.is_none(), "resume 水位未清");
        assert_eq!(state.last_sync_queued, 0);
        assert!(state.pending_events.is_empty(), "待发事件未清");
        assert!(state.last_terminal_reason.is_none());
        assert!(!state.auth_terminal_fired);
    }

    /// 连续切 10 次：每一次都必须换代且不残留。
    #[tokio::test(flavor = "current_thread")]
    async fn ten_consecutive_switches_stay_isolated() {
        let (mut state, _dir) = new_seeded_state("switch-ten").await;
        let mut epochs = Vec::new();
        let mut generations = Vec::new();

        for round in 0..10_i64 {
            // 每一轮都先脏起来，模拟这个账号跑过一段。
            state.active_subscriptions.insert((round as u64, 1), None);
            state.repair_seen.insert((1, round as u64, 1));
            state.should_auto_reconnect = true;

            state.reset_session_scoped_state(round * 1_000);

            epochs.push(state.inbound_epoch);
            generations.push(state.sync_coordinator.generation());
            assert!(
                state.active_subscriptions.is_empty(),
                "第 {round} 轮订阅残留"
            );
            assert!(state.repair_seen.is_empty(), "第 {round} 轮 repair 残留");
            assert!(!state.should_auto_reconnect, "第 {round} 轮仍开着自动重连");
        }

        // 全程严格递增 = 每一次切换都是新的一代，旧代的东西一律作废。
        assert!(
            epochs.windows(2).all(|w| w[1] != w[0]),
            "epoch 未逐轮推进: {epochs:?}"
        );
        assert!(
            generations.windows(2).all(|w| w[1] == w[0] + 1),
            "sync 世代未逐轮推进: {generations:?}"
        );
    }

    /// 切换请求在这一轮同步**开始之前**到达：绝不能被吞掉。
    ///
    /// 这是裸 `notify_waiters()` 的致命窗口——它只唤醒此刻已经在等的人，早到的一声
    /// 永久丢失，慢同步照样把切换堵死。计数器版本必须立刻让出。
    #[tokio::test]
    async fn a_switch_request_that_arrives_before_the_run_is_not_lost() {
        let (mut state, _dir) = new_seeded_state("switch-before-run").await;
        state.sync_stall_for_test = Some(Duration::from_secs(30));
        state.switch_requested.fetch_add(1, Ordering::SeqCst);

        let started = std::time::Instant::now();
        state.ensure_synced(|_| {}).await.expect("ensure_synced");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "切换请求在同步开始前到达却被吞掉，等满了 30s 的同步：{:?}",
            started.elapsed()
        );
    }

    /// 切换请求在同步**跑到一半**时到达：让出，且这一轮不留下任何痕迹。
    #[tokio::test]
    async fn a_slow_sync_yields_to_a_switch_and_writes_nothing_back() {
        let (mut state, _dir) = new_seeded_state("switch-mid-run").await;
        state.sync_stall_for_test = Some(Duration::from_secs(30));

        let requested = state.switch_requested.clone();
        let wakeup = state.switch_wakeup.clone();
        let started = std::time::Instant::now();

        let (result, _) = tokio::join!(state.ensure_synced(|_| {}), async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            requested.fetch_add(1, Ordering::SeqCst);
            wakeup.notify_waiters();
        },);
        result.expect("ensure_synced");

        assert!(
            started.elapsed() < Duration::from_secs(2),
            "慢同步没有让出，切换被堵了 {:?}",
            started.elapsed()
        );

        // 被打断的一轮属于上一个 owner：既不能记成成功，也不能记成失败留下退避，
        // 否则新账号一上来就背着上一个账号的 attempt。
        let snapshot = state.sync_coordinator.snapshot();
        assert_eq!(
            snapshot.attempt, 0,
            "被打断的一轮把 attempt 写了回去，新账号会背上它"
        );
        assert_ne!(
            snapshot.readiness,
            Readiness::Ready,
            "被打断的一轮被记成同步成功了"
        );
    }

    /// 让出之后闸门必须重新打开：新账号的同步要能照常开工。
    #[tokio::test]
    async fn the_new_account_can_sync_after_the_previous_run_yielded() {
        let (mut state, _dir) = new_seeded_state("switch-then-sync").await;
        state.sync_stall_for_test = Some(Duration::from_secs(30));
        state.switch_requested.fetch_add(1, Ordering::SeqCst);
        state.ensure_synced(|_| {}).await.expect("interrupted run");

        // 切换命令被取走 = 销账；新账号这一轮不该再被「有切换排队」挡住。
        state.switch_processed.fetch_add(1, Ordering::SeqCst);
        state.sync_stall_for_test = None;
        assert!(
            !state.switch_is_pending(),
            "切换已处理，闸门却仍认为有请求在排队"
        );

        // 这一轮会真的去跑同步（没有服务端，失败是预期的）——要证的是它**开工了**，
        // 而不是被闸门挡回来。
        let _ = state.ensure_synced(|_| {}).await;
        assert_ne!(
            state.sync_coordinator.snapshot().readiness,
            Readiness::Disconnected,
            "让出之后新账号的同步没能开工，闸门卡死了"
        );
    }

    /// 上一个 owner 的同步结果不得回写。
    ///
    /// 这条断言的是守卫的前提：切换会推进世代，于是 `ensure_synced` await 前后
    /// 取到的值不相等。真正「同步跑到一半被切走」的场景由上面三条覆盖
    /// （用 sync_stall_for_test 把一轮同步挂起，再从外部提交切换请求）。
    #[tokio::test(flavor = "current_thread")]
    async fn a_stale_sync_result_is_dropped_after_the_owner_changed() {
        let (mut state, _dir) = new_seeded_state("switch-stale-result").await;
        state
            .sync_coordinator
            .begin(SyncRunKind::Resume, 0)
            .unwrap();
        let generation_at_start = state.sync_coordinator.generation();

        // sync 还在 await 里，账号被切走。
        state.reset_session_scoped_state(1_000);

        assert_ne!(
            state.sync_coordinator.generation(),
            generation_at_start,
            "世代没变 → 旧 owner 的结果会被当成新账号的同步结果写回去"
        );
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
    fn explicit_connect_reconciles_stale_session_without_query_side_effects() {
        assert_eq!(
            plan_connect(SessionState::Authenticated, true, true).unwrap(),
            ConnectPlan::AlreadyReady
        );
        assert_eq!(
            plan_connect(SessionState::Authenticated, true, false).unwrap(),
            ConnectPlan::RestorePersistedSession
        );
        assert_eq!(
            plan_connect(SessionState::Connected, true, false).unwrap(),
            ConnectPlan::RestorePersistedSession
        );
        assert_eq!(
            plan_connect(SessionState::New, false, false).unwrap(),
            ConnectPlan::ConnectTransportOnly
        );
        assert!(matches!(
            plan_connect(SessionState::Shutdown, true, false),
            Err(Error::Shutdown)
        ));
    }

    #[test]
    fn authenticate_reconnects_transport_lost_after_connect() {
        assert_eq!(
            plan_authenticate_transport(SessionState::Connected, true).unwrap(),
            AuthenticateTransportPlan::UseCurrent
        );
        assert_eq!(
            plan_authenticate_transport(SessionState::Connected, false).unwrap(),
            AuthenticateTransportPlan::ReconnectTransport
        );
        assert_eq!(
            plan_authenticate_transport(SessionState::Authenticated, false).unwrap(),
            AuthenticateTransportPlan::ReconnectTransport
        );
        assert_eq!(
            plan_authenticate_transport(SessionState::New, false).unwrap(),
            AuthenticateTransportPlan::ReconnectTransport
        );
        assert!(matches!(
            plan_authenticate_transport(SessionState::Shutdown, false),
            Err(Error::Shutdown)
        ));
    }

    #[test]
    fn only_connection_errors_prove_that_the_transport_is_dead() {
        use msgtrans::TransportError;

        assert!(State::transport_error_proves_disconnect(
            &TransportError::connection_error("socket closed", true)
        ));
        assert!(!State::transport_error_proves_disconnect(
            &TransportError::timeout_error("sync/get_difference", Duration::from_secs(5))
        ));
        assert!(!State::transport_error_proves_disconnect(
            &TransportError::protocol_error("rpc", "malformed response")
        ));
        assert!(!State::transport_error_proves_disconnect(
            &TransportError::resource_error("pending requests", 32, 16)
        ));
        assert!(!State::transport_error_proves_disconnect(
            &TransportError::config_error("route", "unsupported")
        ));
    }

    /// 「没拿到应答」必须是**可重试**的。
    ///
    /// 这条测试是为一个真实回归立的：把传输超时改成类型化的
    /// [`Error::RequestUnanswered`] 时漏了 [`Error::is_retryable`]，于是所有发送超时
    /// 都掉进「永久失败」分支——outbox 条目被 reject 并删除，用户的消息直接消失。
    /// 分类的语义变了，就必须把依赖这个语义的每一处一起过一遍。
    #[test]
    fn an_unanswered_request_is_retryable_so_the_outbox_survives() {
        assert!(
            Error::RequestUnanswered {
                context: "message/send".to_string(),
            }
            .is_retryable(),
            "发送超时必须可重试，否则 outbox 会被当成永久失败删掉",
        );
        // 对照：鉴权失败仍然不可重试（原地重试只会反复撞未授权会话）。
        assert!(!Error::Auth("token expired".to_string()).is_retryable());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_request_timeout_does_not_tear_down_an_authenticated_session() {
        let (mut state, dir) = new_seeded_state("request-timeout-keeps-session").await;

        let error = state.handle_transport_request_error(
            "rpc get_difference",
            msgtrans::TransportError::timeout_error("sync/get_difference", Duration::from_secs(5)),
        );

        // 超时被归为 [`Error::RequestUnanswered`]——比 `Transport` 更具体：请求没拿到
        // 应答，但这**不构成断线证据**（只有 `TransportError::Connection` 才是）。
        // 下面两条才是本测试真正守的东西：会话一根汗毛都不许动。
        assert!(matches!(error, Error::RequestUnanswered { .. }));
        assert_eq!(state.session_state, SessionState::Authenticated);
        assert_eq!(state.current_uid.as_deref(), Some("10001"));
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_connection_error_still_tears_down_and_arms_reconnect() {
        let (mut state, dir) = new_seeded_state("request-connection-drops-session").await;
        state.should_auto_reconnect = true;

        let error = state.handle_transport_request_error(
            "rpc get_difference",
            msgtrans::TransportError::connection_error("socket closed", true),
        );

        assert!(matches!(
            error,
            Error::Transport(ref message) if message == NETWORK_DISCONNECTED_MESSAGE
        ));
        assert_eq!(state.session_state, SessionState::New);
        assert!(state.next_reconnect_at.is_some());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connection_status_queries_do_not_reconcile_or_tear_down_the_session() {
        let dir = unique_test_dir("pure-connection-queries");
        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);

        let (resp_tx, resp_rx) = oneshot::channel();
        sdk.tx
            .send(Command::SetSessionStateForTest {
                session_state: SessionState::Authenticated,
                resp: resp_tx,
            })
            .await
            .expect("actor accepts test state");
        resp_rx.await.expect("actor applies test state");

        assert_eq!(
            sdk.connection_state().await.expect("read logical state"),
            ConnectionState::Authenticated
        );
        assert!(
            !sdk.is_connected().await.expect("read transport state"),
            "test fixture intentionally has no transport"
        );
        assert_eq!(
            sdk.connection_state()
                .await
                .expect("read logical state again"),
            ConnectionState::Authenticated,
            "a status query reconciled transport health and tore down the session"
        );

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn entity_invalidation_is_control_plane_and_coalesces_pull_keys() {
        let batch = EntityInvalidationBatch::new_v1(
            9_007_199_254_740_993,
            vec![
                EntityInvalidation {
                    entity_type: "friend".to_string(),
                    entity_id: Some("10002".to_string()),
                    scope: None,
                    target_version: 8,
                    mutation_hint: EntityMutationHint::Upsert,
                },
                EntityInvalidation {
                    entity_type: "friend".to_string(),
                    entity_id: Some("10003".to_string()),
                    scope: None,
                    target_version: 9,
                    mutation_hint: EntityMutationHint::Delete,
                },
                EntityInvalidation {
                    entity_type: "group_member".to_string(),
                    entity_id: Some("10004".to_string()),
                    scope: Some("20001".to_string()),
                    target_version: 10,
                    mutation_hint: EntityMutationHint::Upsert,
                },
            ],
            1_780_000_000_123,
        )
        .unwrap();
        let push = PushMessageRequest {
            topic: ENTITY_INVALIDATION_PUSH_TOPIC_V1.to_string(),
            payload: batch.encode_fb().unwrap(),
            ..PushMessageRequest::default()
        };

        let keys = State::entity_invalidation_pull_keys(&push)
            .unwrap()
            .expect("control push");
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&("friend".to_string(), None)));
        assert!(keys.contains(&("group_member".to_string(), Some("20001".to_string()))));
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

        let typed_push = PushMessageRequest {
            server_message_id: 900002,
            message_seq: 78,
            channel_id: 100,
            channel_type: 1,
            from_uid: 100002,
            message_type: ContentMessageType::Text.as_u32(),
            payload: privchat_protocol::encode_message(
                &privchat_protocol::MessagePayloadEnvelope {
                    content: "typed hello".to_string(),
                    ..Default::default()
                },
            )
            .expect("encode typed payload"),
            ..PushMessageRequest::default()
        };
        let typed_item = State::push_message_to_sync_item(typed_push);
        assert_eq!(
            typed_item
                .payload
                .expect("typed sync payload")
                .get("content")
                .and_then(|value| value.as_str()),
            Some("typed hello")
        );
    }

    /// 回归(2026-07-24 生产三修):断网后重连驱动被「15s health_tick 反复推迟 60s
    /// 离线 deadline」饿死 → 永久卡「网络已断开」。根治=离线降频**烘进设置时的绝对
    /// deadline**(schedule_next_reconnect 里 delay.max(60s)),而不是读取时按 now 现算。
    /// 本测试锁死:离线时 next_reconnect_at ≥ now+60s(稳定绝对值);在线时走正常快退避
    /// (首次 ≤ ~2s),证明节流确实在设置时生效、且离线不会退化成读取时重算。
    /// 回归(2026-07-26 生产事故):系统 reachability 卡 Offline 后,用户**能收到消息**
    /// (inbound 走活着的 transport,不看 hint)但**自己发的永远停在「发送中」**——因为出站
    /// 队列的判据里混进了 `network_hint.is_online()`,假 Offline 一票否决了排空。
    ///
    /// 判据现在是自由函数 [`outbound_queue_ready`],签名里**根本没有 NetworkHint**,可达性
    /// 在类型层面无法参与决策。本测试锁死真值表:已鉴权 + 有 uid + transport 在 → 必须排空,
    /// 与任何可达性判断无关;缺任一条则不排空(未鉴权的通道 drain 只会撞 10000)。
    #[test]
    fn outbound_queue_readiness_ignores_reachability() {
        // 三条事实齐备 → 必须排空。此前 hint=Offline 会让它变 false,消息永久卡「发送中」。
        assert!(
            outbound_queue_ready(SessionState::Authenticated, true, true),
            "authenticated session with a live transport must drain regardless of reachability"
        );

        // 缺 transport:真断网/重连中,发不出去,等重连后再排。
        assert!(!outbound_queue_ready(
            SessionState::Authenticated,
            true,
            false
        ));
        // 缺 uid:没有当前用户,无从发送。
        assert!(!outbound_queue_ready(
            SessionState::Authenticated,
            false,
            true
        ));
        // 未鉴权的通道(握好 TCP 但 ConnAuth 未回)上 drain 只会撞 10000,必须等 Authenticated。
        for st in [
            SessionState::New,
            SessionState::Connected,
            SessionState::LoggedIn,
            SessionState::Shutdown,
        ] {
            assert!(
                !outbound_queue_ready(st, true, true),
                "must not drain on unauthenticated session state {st:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn offline_reconnect_deadline_is_baked_stable_not_recomputed() {
        let (mut state, dir) = new_seeded_state("offline-reconnect-deadline").await;
        state.session_state = SessionState::New;
        state.should_auto_reconnect = true;

        // 在线:首次退避基数 1s(±30% jitter),deadline 应在很近的将来(< 3s)。
        state.network_hint = NetworkHint::Unknown;
        state.reconnect_attempt = 0;
        let before = Instant::now();
        state.schedule_next_reconnect();
        let online_at = state.next_reconnect_at.expect("armed online");
        let online_delay = online_at.saturating_duration_since(before);
        assert!(
            online_delay < Duration::from_secs(3),
            "online first retry should be prompt, got {:?}",
            online_delay
        );

        // 离线:同样首次退避,但离线降频把 delay 抬到 ≥60s,烘进绝对 deadline。
        state.network_hint = NetworkHint::Offline;
        state.reconnect_attempt = 0;
        let before = Instant::now();
        state.schedule_next_reconnect();
        let offline_at = state.next_reconnect_at.expect("armed offline");
        assert!(
            offline_at.saturating_duration_since(before) >= Duration::from_secs(60),
            "offline retry deadline must be baked to >= now+60s"
        );

        // 关键不变量:deadline 是稳定绝对值——多次读取(模拟多次 health_tick)返回同一时刻,
        // 绝不因「读取时 now 前进」而被推迟(旧 bug 正是每次读取 at.max(now+60s) 把它推远)。
        let read1 = state.next_reconnect_at.expect("armed");
        let read2 = state.next_reconnect_at.expect("armed");
        assert_eq!(
            read1, read2,
            "next_reconnect_at must be a stable absolute instant"
        );

        drop(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn load_test_mode_acks_transport_but_skips_timeline_materialization() {
        let (mut state, dir) = new_seeded_state("skip-inbound-materialization").await;
        state.skip_inbound_materialization_for_load_testing = true;
        let server_message_id = 904_001;
        let push = PushMessageRequest {
            server_message_id,
            message_seq: 43,
            channel_id: 94_001,
            channel_type: 2,
            from_uid: 20_001,
            message_type: ContentMessageType::Text.as_u32(),
            payload: privchat_protocol::encode_message(
                &privchat_protocol::MessagePayloadEnvelope {
                    content: "load-only delivery".to_string(),
                    ..Default::default()
                },
            )
            .expect("encode typed payload"),
            ..Default::default()
        };

        let applied = state
            .handle_inbound_frame(
                u8::from(privchat_protocol::MessageType::PushMessageRequest),
                privchat_protocol::encode_message(&push).expect("encode push"),
            )
            .await
            .expect("handle load-test push");

        assert_eq!(applied, 0);
        assert!(state.pending_events.is_empty());
        assert!(state
            .storage
            .get_message_id_by_server_message_id(94_001, 2, server_message_id)
            .await
            .expect("query message")
            .is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replayed_canonical_message_does_not_emit_duplicate_timeline_update() {
        use privchat_protocol::{
            CanonicalTimelineEvent, NewMessageEvent, CANONICAL_TIMELINE_PUSH_TOPIC_V1,
        };

        let (mut state, dir) = new_seeded_state("canonical-message-idempotent-event").await;
        state
            .storage
            .upsert_channel(UpsertChannelInput {
                channel_id: 93_100,
                channel_type: 2,
                channel_name: "idempotent-room".to_string(),
                channel_remark: String::new(),
                avatar: String::new(),
                unread_count: 0,
                top: 0,
                mute: 0,
                last_msg_timestamp: 0,
                last_local_message_id: 0,
                last_msg_content: String::new(),
                version: 1,
                peer_user_id: None,
            })
            .await
            .expect("seed channel");
        let event = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: ContentMessageType::Text,
            payload: privchat_protocol::MessagePayloadEnvelope {
                content: "deliver once".to_string(),
                ..Default::default()
            },
        });
        let push = PushMessageRequest {
            server_message_id: 903_100,
            message_seq: 42,
            channel_id: 93_100,
            channel_type: 2,
            from_uid: 20_001,
            message_type: ContentMessageType::System.as_u32(),
            timestamp: 1_710_000_000,
            topic: CANONICAL_TIMELINE_PUSH_TOPIC_V1.to_string(),
            payload: event.encode_fb().expect("encode canonical message"),
            ..Default::default()
        };

        state
            .apply_canonical_timeline_push(&push)
            .await
            .expect("apply first delivery");
        assert_eq!(
            state
                .last_sync_entity_events
                .iter()
                .filter(|event| matches!(
                    event,
                    SdkEvent::TimelineUpdated { .. } | SdkEvent::SyncEntityChanged { .. }
                ))
                .count(),
            2
        );

        state
            .apply_canonical_timeline_push(&push)
            .await
            .expect("apply replayed delivery");
        assert_eq!(
            state
                .last_sync_entity_events
                .iter()
                .filter(|event| matches!(
                    event,
                    SdkEvent::TimelineUpdated { .. } | SdkEvent::SyncEntityChanged { .. }
                ))
                .count(),
            0
        );

        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
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
            event_id: None,
            event_schema_version: None,
            canonical_event: None,
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
            event_id: None,
            event_schema_version: None,
            canonical_event: None,
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
    fn difference_commit_prefers_complete_canonical_event_and_reports_mismatch() {
        #[derive(Serialize)]
        struct LegacyReaction<'a> {
            message_id: &'a str,
            uid: &'a str,
            emoji: &'a str,
            deleted: bool,
        }

        let canonical = privchat_protocol::CanonicalTimelineEvent::ReactionChange(
            privchat_protocol::ReactionChangeEvent {
                target_server_message_id: 9_007_199_254_740_993,
                actor_id: 9_007_199_254_740_995,
                emoji: "thumbs-up".to_string(),
                operation: privchat_protocol::ReactionOperation::Remove,
            },
        );
        let before = super::CANONICAL_LEGACY_MISMATCH_COUNT.load(Ordering::Relaxed);
        let commit = privchat_protocol::rpc::sync::ServerCommit {
            event_id: Some(77),
            pts: 35,
            server_msg_id: 9_007_199_254_740_997,
            local_message_id: None,
            channel_id: 101,
            channel_type: 1,
            message_type: "message_reaction".to_string(),
            content: serde_json::to_value(LegacyReaction {
                message_id: "9007199254740993",
                uid: "9007199254740995",
                emoji: "thumbs-up",
                deleted: false,
            })
            .expect("serialize legacy reaction"),
            server_timestamp: 1_700_000_100_000,
            sender_id: 9_007_199_254_740_995,
            sender_info: None,
            event_schema_version: Some(privchat_protocol::CANONICAL_TIMELINE_EVENT_SCHEMA_V1),
            canonical_event: Some(canonical.encode_fb().expect("encode canonical event")),
        };

        let (entity_type, item) = State::sync_item_from_difference_commit(&commit);
        assert_eq!(entity_type, "message_reaction");
        assert_eq!(
            item.entity_id,
            "9007199254740993:9007199254740995:thumbs-up"
        );
        let payload = item.payload.expect("payload");
        assert_eq!(payload.get("deleted").and_then(|v| v.as_bool()), Some(true));
        assert!(
            super::CANONICAL_LEGACY_MISMATCH_COUNT.load(Ordering::Relaxed) > before,
            "canonical/legacy mismatch must be observable"
        );
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
    async fn room_dedup_drops_repeats_keeps_distinct_and_null() {
        let (mut state, dir) = new_seeded_state("room-dedup").await;
        // 同 channel 同 id → 第二次判重
        assert!(!state.room_message_is_duplicate(7, Some(100)));
        assert!(state.room_message_is_duplicate(7, Some(100)));
        // 不同 id 放行
        assert!(!state.room_message_is_duplicate(7, Some(101)));
        // 不同 channel 独立空间
        assert!(!state.room_message_is_duplicate(8, Some(100)));
        // None id 永不判重（无法去重）
        assert!(!state.room_message_is_duplicate(7, None));
        assert!(!state.room_message_is_duplicate(7, None));
        // 有界：窗口滚过后最旧 id 被逐出，可再次出现（不会无限增长）
        for i in 0..300u64 {
            state.room_message_is_duplicate(9, Some(1000 + i));
        }
        assert!(!state.room_message_is_duplicate(9, Some(1000)));
        state.storage.shutdown();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
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
        // 指向一个确定关闭的端口。默认配置指的是 127.0.0.1:9001——开发机上
        // 常常真的有服务在听，于是这条用例的结果取决于那个服务当时回什么
        // （实测过：本机有服务时它连得上，靠认证被拒才「通过」）。断言的是
        // 「hint 没有短路 connect」，不该依赖环境里有没有人监听。
        let mut config = PrivchatConfig::default();
        config.endpoints = vec![super::ServerEndpoint {
            protocol: super::TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 1,
            path: None,
            use_tls: false,
        }];
        config.connection_timeout_secs = 1;
        let sdk = PrivchatSdk::new(config);
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

        // 契约变更(2026-07-24/26 三次生产事故后):可达性提示**不再短路显式 connect**。
        // 旧断言要求「offline 时 connect 必须以『网络已断开』失败」——正是那个硬闸门让
        // 宿主的前台恢复 / 网络回调 / 用户点重连三个入口全部空转,系统 reachability 一旦
        // 卡在 Offline(iOS 常见,恢复回调不投递)就永远连不回来。现在 connect 一律真尝试,
        // 成败由真实网络决定;这里断言的正是「没有被 hint 短路」。
        let err = sdk
            .connect()
            .await
            .expect_err("connect fails here for lack of endpoints, not because of the hint");
        assert!(
            !err.to_string().contains(NETWORK_DISCONNECTED_MESSAGE),
            "connect must not be short-circuited by an Offline hint, got: {err}"
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
        use privchat_protocol::message::ContentMessageType;
        let voice = ContentMessageType::Voice as i32;
        let image = ContentMessageType::Image as i32;
        let video = ContentMessageType::Video as i32;
        let file = ContentMessageType::File as i32;

        // 第一分层：message_type 独立类型直接决定文案，file_type 无法覆盖。
        assert_eq!(State::attachment_placeholder_text(voice, "file"), "[语音]");
        assert_eq!(State::attachment_placeholder_text(image, ""), "[图片]");
        assert_eq!(State::attachment_placeholder_text(video, ""), "[视频]");

        // 第二分层：File 消息才按 file_type 细分。
        assert_eq!(State::attachment_placeholder_text(file, "image"), "[图片]");
        assert_eq!(State::attachment_placeholder_text(file, "video"), "[视频]");
        assert_eq!(State::attachment_placeholder_text(file, "file"), "[文件]");
        assert_eq!(
            State::attachment_placeholder_text(file, "unknown"),
            "[文件]"
        );
    }

    // 旧测试 conversation_preview_is_rendered_in_sdk_layer 已删除——
    // SDK 不再渲染会话预览（架构归正：preview 是 UI 层职责，参见 SYSTEM_MESSAGE_SPEC）。

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
                status: 1,
                is_outgoing: None,
                request_message: None,
                request_source: None,
                request_source_id: None,
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
                peer_user_id: None,
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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
            event_id: None,
            event_schema_version: None,
            canonical_event: None,
        };
        let (entity_type, item) = State::sync_item_from_difference_commit(&revoke_commit);
        state
            .enqueue_and_apply_sync_items(
                entity_type,
                Some("2:92001".to_string()),
                vec![item],
                true,
            )
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
            event_id: None,
            event_schema_version: None,
            canonical_event: None,
        };
        let (entity_type, item) = State::sync_item_from_difference_commit(&reaction_commit);
        state
            .enqueue_and_apply_sync_items(
                entity_type,
                Some("2:92001".to_string()),
                vec![item],
                true,
            )
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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
    async fn realtime_message_only_bumps_unread_after_local_read_cursor() {
        let (mut state, dir) = new_seeded_state("realtime-unread-read-cursor").await;
        let channel_id = 96010;
        let channel_type = 1;

        state
            .storage
            .upsert_channel(UpsertChannelInput {
                channel_id,
                channel_type,
                channel_name: "alice".to_string(),
                channel_remark: String::new(),
                avatar: String::new(),
                unread_count: 0,
                top: 0,
                mute: 0,
                last_msg_timestamp: 0,
                last_local_message_id: 0,
                last_msg_content: String::new(),
                version: 1,
                peer_user_id: Some(20001),
            })
            .await
            .expect("seed channel");
        state
            .storage
            .project_channel_read_cursor(channel_id, channel_type, 20)
            .await
            .expect("seed local read cursor");

        for (server_message_id, pts, expected_unread) in
            [(896001_u64, 10_i64, 0), (896002_u64, 30_i64, 1)]
        {
            state
                .apply_sync_entities(
                    "message",
                    None,
                    &[SyncEntityItem {
                        entity_id: server_message_id.to_string(),
                        version: pts as u64,
                        deleted: false,
                        payload: Some(serde_json::json!({
                            "server_message_id": server_message_id,
                            "channel_id": channel_id,
                            "channel_type": channel_type,
                            "from_uid": 20001,
                            "message_type": 0,
                            "content": format!("message-{pts}"),
                            "status": 2,
                            "pts": pts,
                            "order_seq": pts
                        })),
                    }],
                    true,
                )
                .await
                .expect("apply realtime message");

            let channel = state
                .storage
                .get_channel_by_id(channel_id)
                .await
                .expect("read channel")
                .expect("channel exists");
            assert_eq!(channel.unread_count, expected_unread, "pts={pts}");
        }

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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                mime_type: None,
                timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                peer_user_id: None,
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
                    peer_user_id: None,
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
                    peer_user_id: None,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
        assert_eq!(
            channel.last_msg_content,
            "{\"content\":\"materialized-message\"}"
        );
        assert_eq!(channel.last_message_type, Some(1));
        assert!(!channel.last_message_is_revoked);
        assert_eq!(channel.last_local_message_id, inserted.message_id);

        let channels = sdk.list_channels(20, 0).await.expect("sdk list channels");
        let listed = channels
            .into_iter()
            .find(|row| row.channel_id == 97002)
            .expect("listed channel exists");
        assert_eq!(listed.last_msg_timestamp, messages[0].created_at);
        assert_eq!(
            listed.last_msg_content,
            "{\"content\":\"materialized-message\"}"
        );
        assert_eq!(listed.last_message_type, Some(1));
        assert!(!listed.last_message_is_revoked);
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
                    status: 1,
                    is_outgoing: None,
                    request_message: None,
                    request_source: None,
                    request_source_id: None,
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
                    member_count: None,
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
                    peer_user_id: None,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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

    /// #4 返工：重试路由的**真路径**测试——断言消息实际落到哪条队列、payload 是什么，
    /// 而不是只测辅助函数。生产故障正是「附件被塞进普通队列（content=本地路径、
    /// metadata 为空）」，服务端必 20006。
    async fn retry_test_sdk(dir: &std::path::Path) -> PrivchatSdk {
        {
            let store = LocalStore::open_at(dir.to_path_buf()).expect("open local store");
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
        }
        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        config.endpoints = vec![super::ServerEndpoint {
            protocol: crate::TransportProtocol::Tcp,
            host: "127.0.0.1".to_string(),
            port: 19001,
            path: None,
            use_tls: false,
        }];
        let sdk = PrivchatSdk::new(config);
        sdk.set_current_uid("10001".to_string())
            .await
            .expect("set current uid");
        sdk
    }

    fn retry_test_message(message_type: i32, content: String) -> NewMessage {
        NewMessage {
            channel_id: 97201,
            channel_type: 2,
            from_uid: 10001,
            message_type,
            content,
            searchable_word: String::new(),
            setting: 0,
            extra: "{}".to_string(),
            mime_type: Some("image/jpeg".to_string()),
            media_downloaded: true,
            thumb_status: 0,
        }
    }

    async fn retry_test_file_items(sdk: &PrivchatSdk) -> Vec<super::QueueMessage> {
        sdk.peek_outbound_files(64).await.unwrap_or_default()
    }

    /// 未鉴权（连接中/重连中）时调用业务 RPC，必须得到**结构化可重试**错误，
    /// 而不是把 `invalid state: operation requires authenticated session (current: New)`
    /// 这种内部状态串抛给上层（用户曾在生产界面看到英文原文）。
    #[tokio::test(flavor = "current_thread")]
    async fn business_rpc_before_authentication_returns_typed_session_not_ready() {
        let dir = unique_test_dir("session-not-ready");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let sdk = retry_test_sdk(&dir).await;

        let err = sdk
            .sync_channel(97_301, 2)
            .await
            .expect_err("unauthenticated session must not run business RPC");
        match &err {
            Error::SessionNotReady { state } => {
                assert!(!state.is_empty(), "state label is for logs, not for UI");
            }
            other => panic!("expected SessionNotReady, got {other:?}"),
        }
        assert_eq!(
            err.protocol_code(),
            privchat_protocol::ErrorCode::SessionNotReady as u32
        );
        assert_eq!(err.sdk_code(), error_codes::SESSION_NOT_READY);

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_message_routes_attachment_to_outbox_without_copying_bytes() {
        let dir = unique_test_dir("retry-attachment-file-queue");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let source = dir.join("photo.jpg");
        std::fs::write(&source, b"jpeg-bytes-payload").expect("write source file");

        let sdk = retry_test_sdk(&dir).await;
        let message_id = sdk
            .create_local_message(retry_test_message(
                privchat_protocol::ContentMessageType::Image as i32,
                format!("file://{}", source.display()),
            ))
            .await
            .expect("create local attachment message");

        let returned = sdk
            .retry_message(message_id)
            .await
            .expect("retry attachment");
        assert_eq!(returned, message_id);

        let files = retry_test_file_items(&sdk).await;
        assert_eq!(
            files.len(),
            1,
            "attachment must land in the attachment outbox"
        );
        assert_eq!(files[0].message_id, message_id);
        // 早期版本把源文件字节复制进队列 payload。主发送路径
        // (`finalize_attachment_and_enqueue`) 从来不这样做——字节留在托管路径
        // 上，drain 发送时读盘。retry 也必须是同一套语义，否则同一张 outbox 上
        // 挂着两种「payload 是什么」的约定，还要把上百 MB 塞进 SQLite BLOB 并
        // 跟着事务一起写。
        assert!(
            files[0].payload.is_empty(),
            "retry must not copy the attachment into the outbox row"
        );
        // 空 payload 只有在 drain 能从消息行拿到源文件时才成立。这条断言就是
        // 那个前提——丢了它，空 payload 就从「按约定读盘」退化成「什么都发不出去」。
        let row = sdk
            .get_message_by_id(message_id)
            .await
            .expect("load message")
            .expect("message exists");
        let resolved = row
            .content
            .strip_prefix("file://")
            .unwrap_or(&row.content)
            .to_string();
        assert_eq!(
            std::fs::read(&resolved).expect("drain must be able to read the managed source"),
            b"jpeg-bytes-payload",
            "message content must still point at the bytes the drain will upload"
        );
        assert!(
            sdk.peek_outbound_messages(16)
                .await
                .expect("peek normal queue")
                .is_empty(),
            "attachment must NOT enter the normal queue (that is the 20006 bug)"
        );

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_message_routes_text_to_normal_queue() {
        let dir = unique_test_dir("retry-text-normal-queue");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let sdk = retry_test_sdk(&dir).await;
        let message_id = sdk
            .create_local_message(retry_test_message(
                privchat_protocol::ContentMessageType::Text as i32,
                "{\"content\":\"hello\"}".to_string(),
            ))
            .await
            .expect("create local text message");

        sdk.retry_message(message_id).await.expect("retry text");

        let normal = sdk
            .peek_outbound_messages(16)
            .await
            .expect("peek normal queue");
        assert_eq!(normal.len(), 1);
        assert_eq!(normal[0].message_id, message_id);
        assert!(
            retry_test_file_items(&sdk).await.is_empty(),
            "text must not enter the file queue"
        );

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_message_without_source_file_is_typed_error_and_queues_nothing() {
        let dir = unique_test_dir("retry-attachment-missing-source");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let sdk = retry_test_sdk(&dir).await;
        let message_id = sdk
            .create_local_message(retry_test_message(
                privchat_protocol::ContentMessageType::Image as i32,
                format!("file://{}", dir.join("deleted.jpg").display()),
            ))
            .await
            .expect("create local attachment message");

        let err = sdk
            .retry_message(message_id)
            .await
            .expect_err("missing source must fail");
        match &err {
            Error::AttachmentSourceMissing { message_id: id } => assert_eq!(*id, message_id),
            other => panic!("expected AttachmentSourceMissing, got {other:?}"),
        }
        assert_eq!(
            err.protocol_code(),
            privchat_protocol::ErrorCode::AttachmentSourceMissing as u32,
            "UI relies on the typed code to offer re-picking the file"
        );
        assert!(retry_test_file_items(&sdk).await.is_empty());
        assert!(sdk
            .peek_outbound_messages(16)
            .await
            .expect("peek normal queue")
            .is_empty());

        sdk.shutdown().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_message_file_queue_entry_survives_restart() {
        let dir = unique_test_dir("retry-attachment-restart");
        std::fs::create_dir_all(&dir).expect("create test dir");
        let source = dir.join("clip.mp4");
        std::fs::write(&source, b"video-bytes").expect("write source file");

        let sdk = retry_test_sdk(&dir).await;
        let message_id = sdk
            .create_local_message(retry_test_message(
                privchat_protocol::ContentMessageType::Video as i32,
                source.display().to_string(),
            ))
            .await
            .expect("create local attachment message");
        sdk.retry_message(message_id)
            .await
            .expect("retry attachment");
        sdk.shutdown().await;

        let reopened = retry_test_sdk(&dir).await;
        let files = retry_test_file_items(&reopened).await;
        assert_eq!(files.len(), 1, "queued retry must survive a restart");
        assert_eq!(files[0].message_id, message_id);
        assert!(
            files[0].payload.is_empty(),
            "bytes stay on disk, not in the row"
        );
        let row = reopened
            .get_message_by_id(message_id)
            .await
            .expect("load message")
            .expect("message exists");
        assert_eq!(
            std::fs::read(&row.content).expect("source must survive the restart too"),
            b"video-bytes",
            "an outbox row that outlives its source file is an unsendable command"
        );

        reopened.shutdown().await;
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
                    peer_user_id: None,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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
                    peer_user_id: None,
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
                    mime_type: None,
                    timestamp_precision: crate::canonical_inbound::TimePrecision::Milliseconds,
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

    #[tokio::test]
    async fn canonical_mutations_wait_for_target_then_replay_in_pts_order() {
        use privchat_protocol::{
            NewMessageEvent, ReactionChangeEvent, ReactionOperation, RevokeEvent,
            CANONICAL_TIMELINE_EVENT_SCHEMA_V1,
        };

        let (mut state, dir) = new_seeded_state("pending-mutation-replay").await;
        state
            .storage
            .upsert_channel(UpsertChannelInput {
                channel_id: 92_500,
                channel_type: 2,
                channel_name: "pending-room".to_string(),
                channel_remark: String::new(),
                avatar: String::new(),
                unread_count: 0,
                top: 0,
                mute: 0,
                last_msg_timestamp: 0,
                last_local_message_id: 0,
                last_msg_content: String::new(),
                version: 1,
                peer_user_id: None,
            })
            .await
            .expect("seed channel");

        let target = 70_500;
        let events = [
            (
                80_501,
                11,
                CanonicalTimelineEvent::Revoke(RevokeEvent {
                    target_server_message_id: target,
                    revoked_by: 10_001,
                    revoked_at: 1_710_000_000_000,
                }),
            ),
            (
                80_502,
                12,
                CanonicalTimelineEvent::ReactionChange(ReactionChangeEvent {
                    target_server_message_id: target,
                    actor_id: 20_001,
                    emoji: "ok".to_string(),
                    operation: ReactionOperation::Add,
                }),
            ),
        ];
        for (event_id, pts, event) in events {
            let push = PushMessageRequest {
                setting: Default::default(),
                msg_key: format!("event_{event_id}"),
                server_message_id: event_id,
                message_seq: pts as u32,
                local_message_id: 0,
                stream_no: String::new(),
                stream_seq: 0,
                stream_flag: 0,
                timestamp: 1_710_000_000,
                channel_id: 92_500,
                channel_type: 2,
                message_type: ContentMessageType::System.as_u32(),
                expire: 0,
                topic: privchat_protocol::CANONICAL_TIMELINE_PUSH_TOPIC_V1.to_string(),
                from_uid: 10_001,
                payload: event.encode_fb().expect("encode mutation"),
                deleted: false,
            };
            assert_eq!(
                state
                    .apply_canonical_timeline_push(&push)
                    .await
                    .expect("apply mutation push"),
                Some(0)
            );
        }

        let new_message = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: ContentMessageType::Text,
            payload: privchat_protocol::MessagePayloadEnvelope {
                content: "arrived after mutations".to_string(),
                ..Default::default()
            },
        });
        let commit = ServerCommit {
            event_id: Some(target),
            pts: 10,
            server_msg_id: target,
            local_message_id: None,
            channel_id: 92_500,
            channel_type: 2,
            message_type: "text".to_string(),
            content: serde_json::Value::Null,
            server_timestamp: 1_709_999_999_000,
            sender_id: 20_001,
            sender_info: None,
            event_schema_version: Some(CANONICAL_TIMELINE_EVENT_SCHEMA_V1),
            canonical_event: Some(new_message.encode_fb().expect("encode message")),
        };
        let (entity_type, item) = State::sync_item_from_difference_commit(&commit);
        state
            .enqueue_and_apply_sync_items(
                entity_type,
                Some("2:92500".to_string()),
                vec![item],
                true,
            )
            .await
            .expect("materialize target");
        assert_eq!(
            state
                .replay_pending_timeline_mutations(92_500, 2, target, true)
                .await
                .expect("replay mutations"),
            2
        );

        let message_id = state
            .storage
            .get_message_id_by_server_message_id(92_500, 2, target)
            .await
            .expect("resolve target")
            .expect("target exists");
        assert!(
            state
                .storage
                .get_message_extra(message_id)
                .await
                .expect("message extra")
                .expect("message extra exists")
                .revoke
        );
        let reactions = state
            .storage
            .list_message_reactions(message_id, 20, 0)
            .await
            .expect("list reactions");
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].emoji, "ok");
        assert!(state
            .storage
            .list_pending_timeline_mutations(92_500, 2, target)
            .await
            .expect("pending after replay")
            .is_empty());
        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    // ==================== TOKEN_REFRESH_SPEC v1.0 — C1 测试 ====================

    /// 10002 AccessTokenExpired 必须分类为 Recoverable，**不**触发 ForcedLogout。
    /// 业务层接到 10002 后调 refreshAccessToken + authenticate 恢复。
    #[test]
    fn recoverable_does_not_trigger_forced_logout() {
        let err = Error::Auth("[10002] access token expired".to_string());
        assert_eq!(err.auth_kind(), Some(AuthErrorKind::Recoverable));
        assert!(
            !err.is_auth_terminal(),
            "10002 must NOT be classified as Terminal"
        );
    }

    /// 10009 / 10010 是 refresh token 失效相关码，必须 Terminal。
    #[test]
    fn terminal_codes_classified_correctly() {
        for code in [10001u32, 10003, 10005, 10007, 10009, 10010] {
            let err = Error::Auth(format!("[{}] something", code));
            assert!(
                err.is_auth_terminal(),
                "code {} must be classified as Terminal",
                code
            );
            assert_eq!(err.auth_kind(), Some(AuthErrorKind::Terminal));
        }
    }

    /// authenticate 在 Authenticated 状态下可调用——业务层 refresh 后直接换 token。
    #[test]
    fn authenticate_allowed_in_authenticated_state() {
        assert!(matches!(
            SessionState::Authenticated.can(Action::Authenticate),
            Ok(SessionState::Authenticated)
        ));
    }

    /// authenticate 在 Terminated 状态下也允许——ForcedLogout 后业务层手动重登路径。
    #[test]
    fn authenticate_allowed_in_terminated_state() {
        assert!(matches!(
            SessionState::Terminated.can(Action::Authenticate),
            Ok(SessionState::Authenticated)
        ));
    }
}
pub mod message_content;
