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

#![allow(clippy::new_without_default)]

mod qr;

use privchat_protocol::rpc::account::user::DetailSourceType;
use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::{
    AccountPrivacyGetRequest,
    AccountPrivacyGetResponse,
    AccountPrivacyUpdateRequest,
    AccountPrivacyUpdateResponse,
    AccountProfileGetRequest,
    AccountProfileGetResponse,
    AccountProfileUpdateRequest,
    AccountProfileUpdateResponse,
    AccountSearchByQRCodeRequest,
    AccountSearchQueryRequest,
    AccountSearchResponse,
    AccountUserDetailRequest,
    AccountUserDetailResponse,
    AccountUserShareCardRequest,
    AccountUserShareCardResponse,
    AccountUserUpdateRequest,
    AccountUserUpdateResponse,
    BatchGetChannelPtsRequest,
    BatchGetChannelPtsResponse,
    BlacklistAddRequest,
    BlacklistAddResponse,
    BlacklistCheckRequest,
    BlacklistCheckResponse,
    BlacklistListRequest,
    BlacklistListResponse,
    BlacklistRemoveRequest,
    BlacklistRemoveResponse,
    BotFollowRequest,
    BotFollowResponse,
    BotUnfollowRequest,
    BotUnfollowResponse,
    ChannelBroadcastCreateRequest,
    ChannelBroadcastCreateResponse,
    ChannelBroadcastListRequest,
    ChannelBroadcastListResponse,
    ChannelBroadcastSubscribeRequest,
    ChannelBroadcastSubscribeResponse,
    ChannelContentListRequest,
    ChannelContentListResponse,
    ChannelContentPublishRequest,
    ChannelContentPublishResponse,
    ChannelHideRequest,
    ChannelHideResponse,
    ChannelMuteRequest,
    ChannelMuteResponse,
    ChannelPinRequest,
    ChannelPinResponse,
    ClientSubmitRequest,
    ClientSubmitResponse,
    DevicePushStatusRequest,
    DevicePushStatusResponse,
    DevicePushUpdateRequest,
    DevicePushUpdateResponse,
    FileGetUrlRequest,
    FileGetUrlResponse,
    FileRequestUploadTokenRequest,
    FileRequestUploadTokenResponse,
    FileUploadCallbackRequest,
    FileUploadCallbackResponse,
    FriendAcceptRequest,
    FriendAcceptResponse,
    FriendApplyRequest,
    FriendApplyResponse,
    FriendCheckRequest,
    FriendCheckResponse,
    FriendPendingRequest,
    FriendPendingResponse,
    FriendRecallRequest,
    FriendRecallResponse,
    FriendRejectRequest,
    FriendRejectResponse,
    FriendRemoveRequest,
    FriendRemoveResponse,
    GetChannelPtsRequest,
    GetChannelPtsResponse,
    GetDifferenceRequest,
    GetDifferenceResponse,
    GetOrCreateDirectChannelRequest,
    GetOrCreateDirectChannelResponse,
    GroupApprovalHandleRequest,
    GroupApprovalHandleResponse,
    GroupApprovalListRequest,
    GroupApprovalListResponse,
    GroupCreateRequest,
    GroupCreateResponse,
    GroupInfoRequest,
    GroupInfoResponse,
    GroupMemberAddRequest,
    GroupMemberAddResponse,
    GroupMemberLeaveRequest,
    GroupMemberLeaveResponse,
    GroupMemberListRequest,
    GroupMemberListResponse,
    GroupMemberMuteRequest,
    GroupMemberMuteResponse,
    GroupMemberRemoveRequest,
    GroupMemberRemoveResponse,
    GroupMemberUnmuteRequest,
    GroupMemberUnmuteResponse,
    GroupMuteAllRequest,
    // QR_CODE_SPEC v1.3 — group qrcode 新类型
    GroupQRCodeGetRequest,
    GroupQRCodeGetResponse,
    GroupQRCodeJoinRequest,
    GroupQRCodeJoinResponse,
    GroupQRCodeRefreshRequest,
    GroupQRCodeRefreshResponse,
    GroupRoleSetRequest,
    GroupRoleSetResponse,
    GroupSettingsGetRequest,
    GroupSettingsGetResponse,
    GroupSettingsUpdateRequest,
    GroupSettingsUpdateResponse,
    GroupTransferOwnerRequest,
    GroupTransferOwnerResponse,
    MessageReactionAddRequest,
    MessageReactionAddResponse,
    MessageReactionListRequest,
    MessageReactionListResponse,
    MessageReactionRemoveRequest,
    MessageReactionRemoveResponse,
    MessageReactionStatsRequest,
    MessageReactionStatsResponse,
    MessageReadListRequest,
    MessageReadListResponse,
    MessageReadStatsRequest,
    MessageReadStatsResponse,
    MessageRevokeRequest,
    MessageRevokeResponse,
    MessageStatusCountRequest,
    MessageStatusCountResponse,
    MessageStatusReadPtsRequest,
    MessageStatusReadPtsResponse,
    QRCodeGenerateRequest,
    QRCodeGenerateResponse,
    QRCodeListRequest,
    QRCodeListResponse,
    QRCodeRefreshRequest,
    QRCodeRefreshResponse,
    QRCodeResolveRequest,
    QRCodeResolveResponse,
    QRCodeRevokeRequest,
    QRCodeRevokeResponse,
    StickerPackageDetailRequest,
    StickerPackageDetailResponse,
    StickerPackageListRequest,
    StickerPackageListResponse,
    SyncEntitiesRequest,
    SyncEntitiesResponse,
    // QR_CODE_SPEC v1.3 — user qrcode 路径下的类型 (顶层 glob 再导出)
    UserQRCodeGetRequest,
    UserQRCodeGetResponse,
    UserQRCodeRefreshRequest,
    UserQRCodeRefreshResponse,
    UserQRCodeResolveRequest,
    UserQRCodeResolveResponse,
};
use privchat_sdk::{
    ConnectionState as SdkConnectionState, ContactCardMessageInput as SdkContactCardMessageInput,
    Error as SdkError, FileQueueRef as SdkFileQueueRef, LinkMessageInput as SdkLinkMessageInput,
    LocalAccountSummary as SdkLocalAccountSummary, LocationMessageInput as SdkLocationMessageInput,
    LoginResult as SdkLoginResult, MediaProcessOp as SdkMediaProcessOp,
    MentionInput as SdkMentionInput, NetworkHint as SdkNetworkHint, NewMessage as SdkNewMessage,
    PresenceStatus as SdkPresenceStatus, PrivchatConfig as SdkConfig, PrivchatSdk as InnerSdk,
    QueueMessage as SdkQueueMessage, SequencedSdkEvent as SdkSequencedSdkEvent,
    ServerEndpoint as SdkServerEndpoint, SessionSnapshot as SdkSessionSnapshot,
    StoredBlacklistEntry as SdkStoredBlacklistEntry, StoredChannel as SdkStoredChannel,
    StoredChannelExtra as SdkStoredChannelExtra, StoredChannelMember as SdkStoredChannelMember,
    StoredFriend as SdkStoredFriend, StoredGroup as SdkStoredGroup,
    StoredGroupMember as SdkStoredGroupMember, StoredMessage as SdkStoredMessage,
    StoredMessageExtra as SdkStoredMessageExtra, StoredMessageReaction as SdkStoredMessageReaction,
    StoredReminder as SdkStoredReminder, StoredUser as SdkStoredUser,
    StructuredSendOptions as SdkStructuredSendOptions, TerminalReason as SdkTerminalReason,
    TransportProtocol as SdkProtocol, TypingActionType as SdkTypingActionType,
    UnreadMentionCount as SdkUnreadMentionCount, UpsertBlacklistInput as SdkUpsertBlacklistInput,
    UpsertChannelExtraInput as SdkUpsertChannelExtraInput,
    UpsertChannelInput as SdkUpsertChannelInput,
    UpsertChannelMemberInput as SdkUpsertChannelMemberInput,
    UpsertFriendInput as SdkUpsertFriendInput, UpsertGroupInput as SdkUpsertGroupInput,
    UpsertGroupMemberInput as SdkUpsertGroupMemberInput,
    UpsertMessageReactionInput as SdkUpsertMessageReactionInput,
    UpsertReminderInput as SdkUpsertReminderInput, UpsertUserInput as SdkUpsertUserInput,
    UserStoragePaths as SdkUserStoragePaths, VideoProcessHook as SdkVideoProcessHook,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast::error::TryRecvError, Mutex as AsyncMutex};

async fn poll_wait(remain: std::time::Duration) {
    let wait = std::cmp::min(std::time::Duration::from_millis(10), remain);
    if wait.is_zero() {
        return;
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::time::sleep(wait).await;
    } else {
        std::thread::sleep(wait);
    }
}

#[derive(Debug, Clone, Serialize)]
struct AuthLogoutRequest {}

type AuthLogoutResponse = bool;

// AuthRefresh request/response types come from privchat_protocol::rpc::auth.
// Used by `refresh_access_token` FFI as a thin RPC wrapper around the
// `account/auth/refresh` endpoint (see TOKEN_REFRESH_SPEC v1.0 §5).

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct EventConfigView {
    broadcast_capacity: u32,
    polling_api: String,
    polling_envelope_api: String,
    event_poll_count: u64,
    sequence_cursor: u64,
    replay_api: String,
    history_limit: u64,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct QueueConfigView {
    normal_queue: String,
    file_queue: String,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct RetryConfigView {
    message_retry: bool,
    strategy: String,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct HttpClientConfigView {
    connection_timeout_secs: u64,
    tls: bool,
    scheme: String,
}

/// Channel Transfer client→app reply (decoded from wire `TransferResponse`).
/// See `02-server/CHANNEL_TRANSFER_SPEC.md` v2.0.
#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct TransferReplyView {
    pub request_id: String,
    pub channel_id: u64,
    pub code: i32,
    pub message: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct PresenceStatsView {
    online: u64,
    offline: u64,
    total: u64,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct TypingChannelView {
    channel_id: u64,
    channel_type: i32,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct TypingStatsView {
    typing: u64,
    active_channels: Vec<TypingChannelView>,
    started_count: u64,
    stopped_count: u64,
}

#[derive(Debug, Clone, Serialize, uniffi::Record)]
pub struct DeviceInfoView {
    device_id: String,
    device_name: String,
    is_current: bool,
    app_in_background: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ConnectionSummary {
    pub state: String,
    pub user_id: u64,
    pub bootstrap_completed: bool,
    pub app_in_background: bool,
    pub supervised_sync_running: bool,
    pub send_queue_enabled: bool,
    pub event_poll_count: u64,
    pub lifecycle_hook_registered: bool,
    pub transport_disconnect_listener_started: bool,
    pub on_connection_state_changed_registered: bool,
    pub on_message_received_registered: bool,
    pub on_reaction_changed_registered: bool,
    pub on_typing_indicator_registered: bool,
    pub video_process_hook_registered: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct BlacklistCheckResult {
    pub is_blocked: bool,
}

/// 显式头像 re-cache 结果（CLIENT_GLOBAL_STATE §4.3 P2）。
/// `avatar_local_path` 是本地展示主字段；`avatar_cached_url` = 本地文件对应的远程版本（freshness 判据）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct AvatarCacheResult {
    pub user_id: u64,
    pub avatar_local_path: String,
    pub avatar_cached_url: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SeenByEntry {
    pub user_id: u64,
    pub read_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaProcessOp {
    Thumbnail,
    Compress,
}

#[uniffi::export(callback_interface)]
pub trait VideoProcessHook: Send + Sync {
    fn process(
        &self,
        op: MediaProcessOp,
        source_path: String,
        meta_path: String,
        output_path: String,
    ) -> Result<bool, PrivchatFfiError>;
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ProfileView {
    pub status: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ProfileUpdateInput {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PrivacySettingsView {
    pub user_id: u64,
    pub allow_add_by_group: bool,
    pub allow_search_by_phone: bool,
    pub allow_search_by_username: bool,
    pub allow_search_by_email: bool,
    pub allow_search_by_qrcode: bool,
    pub allow_view_by_non_friend: bool,
    pub allow_receive_message_from_non_friend: bool,
    pub updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountUserDetailView {
    pub user_id: u64,
    pub username: String,
    pub nickname: String,
    pub avatar_url: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub user_type: i16,
    pub is_friend: bool,
    pub can_send_message: bool,
    pub source_type: String,
    pub source_id: String,
    /// 是否已关注（仅 user_type=2 Bot 有意义；非 bot 永远 false）
    pub is_follow: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeListView {
    pub qr_keys: Vec<QrCodeEntryView>,
    pub total: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeEntryView {
    pub qr_key: String,
    pub qr_code: String,
    pub qr_type: String,
    pub target_id: String,
    pub created_at: u64,
    pub expire_at: Option<u64>,
    pub used_count: u32,
    pub max_usage: Option<u32>,
    pub revoked: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeRefreshView {
    pub old_qr_key: String,
    pub new_qr_key: String,
    pub new_qr_code: String,
    pub revoked_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeRevokeView {
    pub success: bool,
    pub qr_key: String,
    pub revoked_at: u64,
}

/// QR_CODE_SPEC v1.3 — `user/qrcode/get` 响应。
#[derive(Debug, Clone, uniffi::Record)]
pub struct UserQrCodeGetView {
    pub qr_key: String,
    pub qr_code: String,
    pub user_id: u64,
}

/// QR_CODE_SPEC v1.3 — `user/qrcode/refresh` 响应。
#[derive(Debug, Clone, uniffi::Record)]
pub struct UserQrCodeRefreshView {
    pub old_qr_key: String,
    pub new_qr_key: String,
    pub qr_code: String,
    pub user_id: u64,
}

/// QR_CODE_SPEC v1.3 — `user/qrcode/resolve` 响应（最小用户卡片，**不含** qr_key）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct UserQrCodeResolveView {
    pub user_id: u64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub user_type: i16,
    pub is_friend: bool,
    pub is_self: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupSettingsView {
    pub group_id: u64,
    pub join_need_approval: bool,
    pub member_can_invite: bool,
    /// 群成员之间是否允许私自加好友（P0-5）
    pub allow_member_add_friend: bool,
    /// 群是否允许被搜索发现（P0-4）
    pub allow_search: bool,
    /// 加入策略：0=不允许申请 1=允许申请需审核 2=允许直接加入（P0-4）
    pub join_policy: u8,
    pub all_muted: bool,
    pub max_members: u64,
    pub announcement: Option<String>,
    pub description: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupMuteAllView {
    pub success: bool,
    pub group_id: String,
    pub all_muted: bool,
    pub message: String,
    pub operator_id: String,
    pub updated_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupApprovalListView {
    pub group_id: String,
    pub approvals: Vec<GroupApprovalItemView>,
    pub total: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupApprovalItemView {
    pub request_id: String,
    pub user_id: u64,
    pub inviter_id: Option<String>,
    pub qr_code_id: Option<String>,
    pub message: Option<String>,
    pub created_at: u64,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct KeyValueEntry {
    pub key: String,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountPrivacyUpdateInput {
    pub allow_add_by_group: Option<bool>,
    pub allow_search_by_phone: Option<bool>,
    pub allow_search_by_username: Option<bool>,
    pub allow_search_by_email: Option<bool>,
    pub allow_search_by_qrcode: Option<bool>,
    pub allow_view_by_non_friend: Option<bool>,
    pub allow_receive_message_from_non_friend: Option<bool>,
}

/// Input for `refresh_access_token` FFI (TOKEN_REFRESH_SPEC v1.0 §5).
///
/// Both fields are required: caller already knows the device_id (passed at login),
/// and refresh_token is owned by the caller (SDK doesn't store it).
#[derive(Debug, Clone, uniffi::Record)]
pub struct RefreshAccessTokenInput {
    pub refresh_token: String,
    pub device_id: String,
}

/// Result of `refresh_access_token` FFI.
///
/// `refresh_token` is `None` when the server doesn't rotate (B1 default).
/// `refresh_expires_at` is reserved for B2 rotation; B1 may not return it.
#[derive(Debug, Clone, uniffi::Record)]
pub struct RefreshAccessTokenResult {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
    pub refresh_expires_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountUserUpdateInput {
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupSettingsUpdateInput {
    pub group_id: u64,
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    /// 加群是否需审批（可选）
    pub join_need_approval: Option<bool>,
    /// 成员是否可邀请他人入群（可选）
    pub member_can_invite: Option<bool>,
    /// 全员禁言（可选）
    pub all_muted: Option<bool>,
    /// 群成员之间是否允许私自加好友（可选，P0-5）
    pub allow_member_add_friend: Option<bool>,
    /// 群是否允许被搜索发现（可选，P0-4）
    pub allow_search: Option<bool>,
    /// 加入策略（可选）：0=不允许申请 1=允许申请需审核 2=允许直接加入
    pub join_policy: Option<u8>,
    /// 成员上限（可选）
    pub max_members: Option<u32>,
    /// 群公告（可选）
    pub announcement: Option<String>,
}

/// 群消息置顶/取消置顶结果
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupPinMessageView {
    pub success: bool,
    pub group_id: u64,
    pub message_id: u64,
    pub pinned: bool,
    pub pinned_at: Option<u64>,
    pub pinned_by: Option<u64>,
}

/// 单条群置顶消息
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupPinnedMessageView {
    pub message_id: u64,
    pub channel_id: u64,
    pub pinned_by: u64,
    pub pinned_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileRequestUploadTokenInput {
    pub filename: Option<String>,
    pub file_size: i64,
    pub mime_type: String,
    pub file_type: String,
    pub business_type: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileRequestUploadTokenView {
    pub token: String,
    pub upload_url: String,
    pub file_id: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FileUploadCallbackInput {
    pub file_id: String,
    pub status: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncSubmitInput {
    pub local_message_id: u64,
    pub channel_id: u64,
    pub channel_type: u8,
    pub last_pts: u64,
    pub command_type: String,
    pub payload_entries: Vec<SyncPayloadEntry>,
    pub client_timestamp: i64,
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncPayloadEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncSubmitView {
    pub decision: String,
    pub decision_reason: Option<String>,
    pub pts: Option<u64>,
    pub server_msg_id: Option<u64>,
    pub server_timestamp: i64,
    pub local_message_id: u64,
    pub has_gap: bool,
    pub current_pts: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncEntitiesInput {
    pub entity_type: String,
    pub since_version: Option<u64>,
    pub scope: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncEntityItemView {
    pub entity_id: String,
    pub version: u64,
    pub deleted: bool,
    pub payload_json: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncEntitiesView {
    pub items: Vec<SyncEntityItemView>,
    pub next_version: u64,
    pub has_more: bool,
    pub min_version: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GetDifferenceInput {
    pub channel_id: u64,
    pub channel_type: u8,
    pub last_pts: u64,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GetDifferenceCommitView {
    pub pts: u64,
    pub server_msg_id: u64,
    pub local_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: u8,
    pub message_type: String,
    pub content_json: String,
    pub server_timestamp: i64,
    pub sender_id: u64,
    pub sender_info_json: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GetDifferenceView {
    pub commits: Vec<GetDifferenceCommitView>,
    pub current_pts: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GetChannelPtsInput {
    pub channel_id: u64,
    pub channel_type: u8,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelPtsView {
    pub channel_id: u64,
    pub channel_type: u8,
    pub current_pts: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct BatchGetChannelPtsInput {
    pub channels: Vec<GetChannelPtsInput>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct BatchGetChannelPtsView {
    pub channels: Vec<ChannelPtsView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelBroadcastCreateInput {
    pub name: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelBroadcastCreateView {
    pub status: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelBroadcastSubscribeInput {
    pub channel_id: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelBroadcastListInput {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelBroadcastListView {
    pub status: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelContentPublishInput {
    pub channel_id: u64,
    pub content: String,
    pub title: Option<String>,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelContentPublishView {
    pub status: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelContentListInput {
    pub channel_id: u64,
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelContentListView {
    pub status: String,
    pub action: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerPackageListInput {}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerPackageListView {
    pub packages: Vec<StickerPackageInfoView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerPackageDetailInput {
    pub package_id: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerPackageDetailView {
    pub package: StickerPackageInfoView,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerPackageInfoView {
    pub package_id: String,
    pub name: String,
    pub thumbnail_url: String,
    pub author: String,
    pub description: String,
    pub sticker_count: u64,
    pub stickers: Vec<StickerInfoView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StickerInfoView {
    pub sticker_id: String,
    pub package_id: String,
    pub image_url: String,
    pub alt_text: String,
    pub emoji: Option<String>,
    pub width: u32,
    pub height: u32,
    pub mime_type: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SendMessageOptionsInput {
    pub in_reply_to_message_id: Option<u64>,
    pub mentioned_user_ids: Vec<u64>,
    pub silent: bool,
    pub extra_json: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct LocalAttachmentMetadataInput {
    pub file_name: String,
    pub mime_type: String,
    pub duration: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumbnail_width: Option<u32>,
    pub thumbnail_height: Option<u32>,
    pub extension_json: Option<String>,
}

fn metadata_input_extension(
    raw: Option<&str>,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, PrivchatFfiError> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let value = serde_json::from_str::<serde_json::Value>(raw).map_err(|e| {
        PrivchatFfiError::from(SdkError::Serialization(format!(
            "decode attachment extension_json: {e}"
        )))
    })?;
    value.as_object().cloned().map(Some).ok_or_else(|| {
        PrivchatFfiError::from(SdkError::Serialization(
            "attachment extension_json must be an object".to_string(),
        ))
    })
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct StructuredSendOptionsInput {
    pub in_reply_to_message_id: Option<u64>,
    pub mentioned_user_ids: Vec<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct LinkMessageInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub thumbnail_file_id: Option<u64>,
    pub options: Option<StructuredSendOptionsInput>,
}

#[derive(Debug, Clone, uniffi::Record)]
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
    pub options: Option<StructuredSendOptionsInput>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ContactCardMessageInput {
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub user_id: u64,
    pub options: Option<StructuredSendOptionsInput>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupInfoView {
    pub group_id: u64,
    pub name: String,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub owner_id: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub member_count: u32,
    pub message_count: Option<u32>,
    pub is_archived: Option<bool>,
    pub tags_json: Option<String>,
    pub custom_fields_json: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupMemberRemoteEntry {
    pub user_id: u64,
    pub role: i32,
    pub status: i32,
    pub alias: Option<String>,
    pub is_muted: bool,
    pub joined_at: i64,
    pub updated_at: i64,
    pub raw_json: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupMemberRemoteList {
    pub members: Vec<GroupMemberRemoteEntry>,
    pub total: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReactionListView {
    pub success: bool,
    pub total_count: u64,
    pub reactions: Vec<MessageReactionEmojiUsersView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReactionStatsView {
    pub success: bool,
    pub total_count: u64,
    pub reactions: Vec<MessageReactionEmojiUsersView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ReactionsBatchItemView {
    pub server_message_id: u64,
    pub success: bool,
    pub total_count: u64,
    pub reactions: Vec<MessageReactionEmojiUsersView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReactionEmojiUsersView {
    pub emoji: String,
    pub user_ids: Vec<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ReactionsBatchView {
    pub items: Vec<ReactionsBatchItemView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DevicePushInfoView {
    pub device_id: String,
    pub apns_armed: bool,
    pub connected: bool,
    pub platform: String,
    pub vendor: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DevicePushStatusView {
    pub devices: Vec<DevicePushInfoView>,
    pub user_push_enabled: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DevicePushUpdateView {
    pub device_id: String,
    pub apns_armed: bool,
    pub user_push_enabled: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageHistoryView {
    pub messages: Vec<MessageHistoryItemView>,
    pub has_more: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageHistoryItemView {
    pub message_id: u64,
    pub channel_id: u64,
    pub sender_id: u64,
    pub content: String,
    pub message_type: String,
    pub timestamp: u64,
    /// per-channel pts（server message_seq）；本地排序权威 = (pts, server_message_id)
    pub message_seq: Option<i64>,
    pub reply_to_message_id: Option<u64>,
    pub metadata_json: Option<String>,
    pub revoked: bool,
    pub revoked_at: Option<i64>,
    pub revoked_by: Option<u64>,
}

/// 搜索命中高亮区间（相对 snippet 的字符偏移 [start, end)）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchHighlightRangeView {
    pub start: u32,
    pub end: u32,
}

/// 云端历史搜索命中（snippet 投影——UI 展示用，**不落本地 message 表**；
/// 点击后调 get_messages_around 拿完整上下文，spec §4/§5/§6 边界）
#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchHistoryHitView {
    pub channel_id: u64,
    pub message_id: u64,
    pub sender_user_id: u64,
    /// 毫秒时间戳
    pub created_at: i64,
    pub message_type: String,
    pub snippet: String,
    pub highlight_ranges: Vec<SearchHighlightRangeView>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SearchHistoryView {
    pub hits: Vec<SearchHistoryHitView>,
    /// keyset 游标；None = 到底。原样回传给下一页请求
    pub next_cursor: Option<String>,
}

/// jump-to-message 上下文（完整消息，SDK 已回填本地库；UI 应从本地重查渲染并定位 anchor）
#[derive(Debug, Clone, uniffi::Record)]
pub struct MessagesAroundView {
    pub before_messages: Vec<MessageHistoryItemView>,
    pub anchor_message: MessageHistoryItemView,
    pub after_messages: Vec<MessageHistoryItemView>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

/// 协议 MessageHistoryItem → FFI view（get/around 共用，防字段漂移）
fn history_item_view(
    m: privchat_protocol::rpc::message::history::MessageHistoryItem,
) -> MessageHistoryItemView {
    MessageHistoryItemView {
        message_id: m.message_id,
        channel_id: m.channel_id,
        sender_id: m.sender_id,
        content: m.content,
        message_type: m.message_type,
        timestamp: m.timestamp,
        message_seq: m.message_seq,
        reply_to_message_id: m.reply_to_message_id,
        metadata_json: m.metadata.map(|v| serde_json::Value::Object(v).to_string()),
        revoked: m.revoked,
        revoked_at: m.revoked_at,
        revoked_by: m.revoked_by,
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReadListView {
    pub readers: Vec<MessageReadUserView>,
    pub total: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReadUserView {
    pub user_id: u64,
    pub username: Option<String>,
    pub nickname: Option<String>,
    pub avatar_url: Option<String>,
    pub read_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageReadStatsView {
    pub read_count: u32,
    pub total_count: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeGenerateView {
    pub qr_key: String,
    pub qr_code: String,
    pub qr_type: String,
    pub target_id: u64,
    pub created_at: u64,
    pub expire_at: Option<u64>,
    pub max_usage: Option<u32>,
    pub used_count: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct QrCodeResolveView {
    pub qr_type: String,
    pub target_id: u64,
    pub action: String,
    pub data_json: Option<String>,
    pub used_count: u32,
    pub max_usage: Option<u32>,
    pub expire_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountSearchResultView {
    pub users: Vec<SearchUserEntry>,
    pub total: u64,
    pub query: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct AccountUserShareCardView {
    pub share_key: String,
    pub share_url: String,
    pub expire_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupTransferOwnerView {
    pub group_id: u64,
    pub new_owner_id: u64,
    pub transferred_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupRoleSetView {
    pub group_id: u64,
    pub user_id: u64,
    pub role: String,
    pub updated_at: Option<u64>,
}

/// QR_CODE_SPEC v1.3 — `group/qrcode/get` 响应。
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupQrCodeGetView {
    pub qr_key: String,
    pub qr_code: String,
    pub group_id: u64,
}

/// QR_CODE_SPEC v1.3 — `group/qrcode/refresh` 响应。
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupQrCodeRefreshView {
    pub old_qr_key: String,
    pub new_qr_key: String,
    pub qr_code: String,
    pub group_id: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageUnreadCountView {
    pub unread_count: i32,
    pub channel_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RetryMessagePayloadView {
    message_id: u64,
    channel_id: u64,
    channel_type: i32,
    from_uid: u64,
    message_type: i32,
    content: String,
    extra: String,
}

fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}

fn json_encode<T: Serialize>(value: &T, what: &str) -> Result<String, PrivchatFfiError> {
    serde_json::to_string(value).map_err(|e| PrivchatFfiError::SdkError {
        code: privchat_protocol::ErrorCode::InvalidJson as u32,
        detail: format!("serialize {what} failed: {e}"),
    })
}

fn spawn_background_future<F>(label: &'static str, fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    let name = format!("privchat-ffi-{label}");
    let _ = std::thread::Builder::new().name(name).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(err) => {
                eprintln!("[FFI] {label}: build background runtime failed: {err}");
                return;
            }
        };
        runtime.block_on(fut);
    });
}

/// FFI typed RPC 入口——**统一委托到 [`PrivchatSdk::rpc_call_typed`]**。
///
/// 历史 bug：FFI 曾经自己实现了一套 encode → rpc_call → decode 的简化链路，绕过了
/// SDK 主路径上的 [`PrivchatSdk::apply_rpc_side_effects`]，导致 `friend::ACCEPT`
/// 这种"成功后必须同步 friend / user / channel 实体"的 RPC 在本地实体表没刷新——
/// UI 同意好友后联系人列表不增量更新，是同一个根因。
///
/// **架构不变式（FFI RPC Convergence Rule）**：
/// FFI 层不得自行重新实现 typed RPC 调用链。所有带业务语义的 RPC 必须经过
/// [`PrivchatSdk::rpc_call_typed`]——它内部 encode → rpc_call → 统一 side-effect
/// 编排（[`PrivchatSdk::apply_rpc_side_effects`]）→ decode。
///
/// 这个不变式跟"Global Service Convergence Rule"同构：业务语义只有一条主路径。
/// FFI 只做类型映射与平台桥接，不绕开 SDK 主路径。
async fn rpc_call_typed<Req, Resp>(
    sdk: &InnerSdk,
    route: &str,
    request: &Req,
) -> Result<Resp, PrivchatFfiError>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    sdk.rpc_call_typed::<Req, Resp>(route, request)
        .await
        .map_err(PrivchatFfiError::from)
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
            code: value.sdk_code(),
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
    pub data_dir: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct LoginResult {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub refresh_token: Option<String>,
    pub expires_at: u64,
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
    /// 服务端判定本次登录态不可自愈（token 过期/撤销/设备不匹配）。
    /// SDK 已断开并停止自动重连；UI 必须重新登录才能回到 New。
    Terminated,
    Shutdown,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum NetworkHint {
    Unknown,
    Offline,
    Wifi,
    Cellular,
    Ethernet,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum ResumeFailureClass {
    RetryableTemporaryError,
    ChannelResyncRequired,
    EntityResyncRequired,
    FullRebuildRequired,
    FatalProtocolError,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum ResumeEscalationScope {
    Retry,
    ChannelScopedResync,
    EntityScopedResync,
    FullRebuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SyncPhase {
    Idle,
    Syncing,
    Synced,
    Retrying,
    FailedTerminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SyncRunKind {
    Bootstrap,
    Resume,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SyncStateSnapshot {
    pub phase: SyncPhase,
    pub run_kind: Option<SyncRunKind>,
    pub attempt: u32,
    pub error_code: Option<u32>,
    pub message: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum MediaDownloadState {
    Idle,
    Downloading { bytes: u64, total: Option<u64> },
    Paused { bytes: u64, total: Option<u64> },
    Done { path: String },
    Failed { code: u32, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ForcedLogoutSource {
    ConnectAuth,
    RpcAuth,
    Manual,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct TerminalReason {
    /// `privchat_protocol::ErrorCode` 对应的 u32 码；未携带时为 0。
    pub code: u32,
    pub message: String,
    pub source: ForcedLogoutSource,
    pub at_ms: i64,
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
    SyncStateChanged {
        state: SyncStateSnapshot,
    },
    ResumeSyncStarted,
    ResumeSyncCompleted {
        entity_types_synced: u64,
        channels_scanned: u64,
        channels_applied: u64,
        channel_failures: u64,
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
        applied: u64,
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
        queued: u64,
        applied: u64,
        dropped_duplicates: u64,
    },
    SyncEntityChanged {
        entity_type: String,
        entity_id: String,
        deleted: bool,
    },
    SyncChannelApplied {
        channel_id: u64,
        channel_type: i32,
        applied: u64,
    },
    SyncAllChannelsApplied {
        applied: u64,
    },
    NetworkHintChanged {
        from: NetworkHint,
        to: NetworkHint,
    },
    OutboundQueueUpdated {
        kind: String,
        action: String,
        message_id: Option<u64>,
        queue_index: Option<u64>,
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
        message_id: u64,
        server_message_id: u64,
        delivered_at: u64,
    },
    MediaDownloadStateChanged {
        message_id: u64,
        state: MediaDownloadState,
    },
    MediaJobRequested {
        job_id: String,
        job_kind: String,
        source_path: String,
        output_path: String,
        mime_type: String,
        message_id: u64,
        timeout_ms: u64,
    },
    /// Access token 已由 SDK 自动续期成功。不携带 token 内容——宿主如需使用，
    /// 请调用 `PrivchatSdk::get_current_access_token()` 主动拉取。
    TokenRefreshed {
        /// 新 access_token 过期时间（Unix 毫秒，服务端下发）。
        expires_at: u64,
    },
    /// auto-reconnect 握手撞到 Recoverable auth 错（典型 10002）；SDK 已暂停 auto-reconnect。
    /// 业务层应调用自家 mode-aware refresh 入口（详见 TOKEN_REFRESH_SPEC §3.1）。
    AccessTokenRefreshNeeded {
        /// 服务端原始错误码，典型 10002。
        code: u32,
        /// 服务端原始 message，仅作日志/审计。
        message: String,
    },
    ForcedLogout {
        /// `privchat_protocol::ErrorCode` 对应的 u32 码；未携带时为 0。
        code: u32,
        message: String,
        source: ForcedLogoutSource,
    },
    ShutdownStarted,
    ShutdownCompleted,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MediaJobResult {
    pub ok: bool,
    pub output_path: Option<String>,
    pub error: Option<String>,
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
pub struct SearchUserEntry {
    pub user_id: u64,
    pub username: String,
    pub nickname: String,
    pub avatar_url: Option<String>,
    pub user_type: i32,
    pub search_session_id: u64,
    pub is_friend: bool,
    pub can_send_message: bool,
    /// 是否已关注（仅 user_type=2 Bot 有意义；非 bot 永远 false）
    pub is_follow: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FriendPendingEntry {
    pub from_user_id: u64,
    pub user: SearchUserEntry,
    pub message: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FriendRequestResult {
    pub user_id: u64,
    pub username: String,
    pub status: String,
    pub added_at: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct DirectChannelResult {
    pub channel_id: u64,
    pub created: bool,
}

/// Bot follow 结果（spec SERVICE_ACCOUNT_FOLLOW_SPEC §2.2）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct BotFollowResult {
    pub bot_user_id: u64,
    /// 与该 bot 之间的 direct channel id；后续 Subscribe / Transfer / SendMessage 都用它。
    pub channel_id: u64,
    /// v1.0 固定 2 (Bot)；保留以兼容未来扩展。
    pub account_user_type: i32,
    pub followed: bool,
    /// `true` = 新建关系或从 unfollowed 复活；`false` = 已 followed 幂等复用。
    pub created: bool,
}

/// Bot unfollow 结果。
#[derive(Debug, Clone, uniffi::Record)]
pub struct BotUnfollowResult {
    pub bot_user_id: u64,
    /// 已存在的 direct channel id（保留，**不**删除）；`0` = 原本就没关注过。
    pub channel_id: u64,
    /// `true` = 已取消关注；`false` = 原本就没关注，no-op。
    pub unfollowed: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupCreateResult {
    pub group_id: u64,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u32,
    pub created_at: u64,
    pub creator_id: u64,
}

/// QR_CODE_SPEC v1.3 — `group/join/qrcode` 响应。
/// 注意：v1.3 删除了 `expires_at` 字段（永久二维码无过期概念）。
#[derive(Debug, Clone, uniffi::Record)]
pub struct GroupQrCodeJoinResult {
    pub status: String,
    pub group_id: u64,
    pub request_id: Option<String>,
    pub message: Option<String>,
    pub user_id: Option<u64>,
    pub joined_at: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct ChannelSyncState {
    pub channel_id: u64,
    pub channel_type: i32,
    pub unread: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PresenceStatus {
    pub user_id: u64,
    pub is_online: bool,
    pub last_seen_at: i64,
    pub device_count: u32,
    pub version: u64,
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
    pub mime_type: Option<String>,
    pub media_downloaded: bool,
    pub thumb_status: i32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageTextEntity {
    pub kind: String,
    pub start: u32,
    pub end: u32,
    pub text: String,
    pub value: String,
    pub user_id: Option<u64>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageContentRef {
    pub kind: String,
    pub target_id: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MessageContentBody {
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

#[derive(Debug, Clone, uniffi::Record)]
pub struct StoredMessage {
    pub message_id: u64,
    pub server_message_id: Option<u64>,
    pub local_message_id: Option<u64>,
    pub channel_id: u64,
    pub channel_type: i32,
    pub from_uid: u64,
    pub message_type: i32,
    pub content: String,
    /// SDK-owned typed projection. Application/UI code consumes this field.
    pub body: MessageContentBody,
    pub status: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub extra: String,
    pub revoked: bool,
    pub revoked_by: Option<u64>,
    pub mime_type: Option<String>,
    pub media_downloaded: bool,
    pub thumb_status: i32,
    pub delivered: bool,
    pub pts: Option<u64>,
    /// 引用消息的 server_message_id（envelope.reply_to_message_id）
    pub reply_to_message_id: Option<String>,
    /// @ 提及的用户 ID 列表（envelope.mentioned_user_ids）
    pub mentioned_user_ids: Vec<u64>,
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
    /// 最后一条消息的原始 content（TEXT = 纯文本，其他类型 = 结构化 JSON）。
    /// UI 层基于 `last_message_type` + content + i18n 自行渲染预览，**SDK 不做改写**。
    pub last_msg_content: String,
    pub last_message_body: Option<MessageContentBody>,
    pub updated_at: i64,
    pub peer_user_id: Option<u64>,
    /// 群成员数（群会话有意义，来自 group 实体缓存；DM/未知为 0）。供群标题「(N)」。
    pub member_count: u32,
    /// 最后一条消息的协议 message_type（ContentMessageType 整型值）。
    pub last_message_type: Option<i32>,
    /// 最后一条消息是否已撤回。
    pub last_message_is_revoked: bool,
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
    pub delivered: bool,
    pub delivered_at: u64,
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
    /// AVATAR_CACHE_SPEC P1: 头像本地缓存文件绝对路径（已验证文件存在）；
    /// 空串 = 未缓存，回落 avatar（网络 URL）。
    pub avatar_local_path: String,
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
    /// 用户名（来自 LEFT JOIN user，可能为空——例如 user profile 还没同步）。
    pub username: Option<String>,
    /// 昵称（同上）。
    pub nickname: Option<String>,
    /// 备注名（仅 accepted 行有意义；request 态 server 不填）。
    pub alias: Option<String>,
    /// 头像 URL。空串表示无头像。
    pub avatar: String,
    /// AVATAR_CACHE_SPEC P1: 头像本地缓存文件绝对路径（已验证文件存在）；
    /// 空串 = 未缓存，回落 avatar（网络 URL）。
    pub avatar_local_path: String,
    pub tags: Option<String>,
    pub is_pinned: bool,
    pub created_at: i64,
    pub updated_at: i64,
    /// F-sync.2: 0=pending / 1=accepted / 2=blocked / 3=rejected / 4=recalled / 5=expired.
    /// `list_friends` 只返 status=1；其它态从 `list_friend_requests` 拿。
    pub status: i16,
    /// 申请态下 viewer 是不是 requester：true=我发出的，false=我收到的；accepted=null。
    pub is_outgoing: Option<bool>,
    pub request_message: Option<String>,
    pub request_source: Option<String>,
    pub request_source_id: Option<String>,
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

#[derive(Debug, Clone, uniffi::Record)]
pub struct LocalAccountSummary {
    pub uid: String,
    pub created_at: i64,
    pub last_login_at: i64,
    pub is_active: bool,
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
        data_dir: c.data_dir,
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
        SdkConnectionState::Terminated => ConnectionState::Terminated,
        SdkConnectionState::Shutdown => ConnectionState::Shutdown,
    }
}

fn map_network_hint(v: NetworkHint) -> SdkNetworkHint {
    match v {
        NetworkHint::Unknown => SdkNetworkHint::Unknown,
        NetworkHint::Offline => SdkNetworkHint::Offline,
        NetworkHint::Wifi => SdkNetworkHint::Wifi,
        NetworkHint::Cellular => SdkNetworkHint::Cellular,
        NetworkHint::Ethernet => SdkNetworkHint::Ethernet,
    }
}

fn map_sdk_network_hint(v: SdkNetworkHint) -> NetworkHint {
    match v {
        SdkNetworkHint::Unknown => NetworkHint::Unknown,
        SdkNetworkHint::Offline => NetworkHint::Offline,
        SdkNetworkHint::Wifi => NetworkHint::Wifi,
        SdkNetworkHint::Cellular => NetworkHint::Cellular,
        SdkNetworkHint::Ethernet => NetworkHint::Ethernet,
    }
}

fn map_sdk_resume_failure_class(v: privchat_sdk::ResumeFailureClass) -> ResumeFailureClass {
    match v {
        privchat_sdk::ResumeFailureClass::RetryableTemporaryError => {
            ResumeFailureClass::RetryableTemporaryError
        }
        privchat_sdk::ResumeFailureClass::ChannelResyncRequired => {
            ResumeFailureClass::ChannelResyncRequired
        }
        privchat_sdk::ResumeFailureClass::EntityResyncRequired => {
            ResumeFailureClass::EntityResyncRequired
        }
        privchat_sdk::ResumeFailureClass::FullRebuildRequired => {
            ResumeFailureClass::FullRebuildRequired
        }
        privchat_sdk::ResumeFailureClass::FatalProtocolError => {
            ResumeFailureClass::FatalProtocolError
        }
    }
}

fn map_sdk_resume_escalation_scope(
    v: privchat_sdk::ResumeEscalationScope,
) -> ResumeEscalationScope {
    match v {
        privchat_sdk::ResumeEscalationScope::Retry => ResumeEscalationScope::Retry,
        privchat_sdk::ResumeEscalationScope::ChannelScopedResync => {
            ResumeEscalationScope::ChannelScopedResync
        }
        privchat_sdk::ResumeEscalationScope::EntityScopedResync => {
            ResumeEscalationScope::EntityScopedResync
        }
        privchat_sdk::ResumeEscalationScope::FullRebuild => ResumeEscalationScope::FullRebuild,
    }
}

fn map_sync_state(v: privchat_sdk::SyncStateSnapshot) -> SyncStateSnapshot {
    let phase = match v.phase {
        privchat_sdk::SyncPhase::Idle => SyncPhase::Idle,
        privchat_sdk::SyncPhase::Syncing => SyncPhase::Syncing,
        privchat_sdk::SyncPhase::Synced => SyncPhase::Synced,
        privchat_sdk::SyncPhase::Retrying => SyncPhase::Retrying,
        privchat_sdk::SyncPhase::FailedTerminal => SyncPhase::FailedTerminal,
    };
    let run_kind = v.run_kind.map(|kind| match kind {
        privchat_sdk::SyncRunKind::Bootstrap => SyncRunKind::Bootstrap,
        privchat_sdk::SyncRunKind::Resume => SyncRunKind::Resume,
    });
    SyncStateSnapshot {
        phase,
        run_kind,
        attempt: v.attempt,
        error_code: v.error_code,
        message: v.message,
        updated_at_ms: v.updated_at_ms,
    }
}

#[cfg(test)]
fn parse_read_list_entries(raw: &str) -> Vec<serde_json::Value> {
    fn to_values(
        entries: Vec<privchat_protocol::rpc::MessageReadUserEntry>,
    ) -> Vec<serde_json::Value> {
        entries
            .into_iter()
            .filter_map(|entry| serde_json::to_value(entry).ok())
            .collect()
    }

    if let Ok(resp) = serde_json::from_str::<MessageReadListResponse>(raw) {
        if !resp.readers.is_empty() {
            return to_values(resp.readers);
        }
    }

    #[derive(Deserialize)]
    struct DataEnvelope {
        data: Option<MessageReadListResponse>,
        items: Option<Vec<serde_json::Value>>,
    }

    if let Ok(env) = serde_json::from_str::<DataEnvelope>(raw) {
        if let Some(data) = env.data {
            return to_values(data.readers);
        }
        if let Some(items) = env.items {
            return items;
        }
    }

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
        privchat_sdk::SdkEvent::SyncStateChanged { state } => SdkEvent::SyncStateChanged {
            state: map_sync_state(state),
        },
        privchat_sdk::SdkEvent::ResumeSyncStarted => SdkEvent::ResumeSyncStarted,
        privchat_sdk::SdkEvent::ResumeSyncCompleted {
            entity_types_synced,
            channels_scanned,
            channels_applied,
            channel_failures,
        } => SdkEvent::ResumeSyncCompleted {
            entity_types_synced: entity_types_synced as u64,
            channels_scanned: channels_scanned as u64,
            channels_applied: channels_applied as u64,
            channel_failures: channel_failures as u64,
        },
        privchat_sdk::SdkEvent::ResumeSyncFailed {
            classification,
            scope,
            error_code,
            message,
        } => SdkEvent::ResumeSyncFailed {
            classification: map_sdk_resume_failure_class(classification),
            scope: map_sdk_resume_escalation_scope(scope),
            error_code,
            message,
        },
        privchat_sdk::SdkEvent::ResumeSyncEscalated {
            classification,
            scope,
            reason,
            entity_type,
            channel_id,
            channel_type,
        } => SdkEvent::ResumeSyncEscalated {
            classification: map_sdk_resume_failure_class(classification),
            scope: map_sdk_resume_escalation_scope(scope),
            reason,
            entity_type,
            channel_id,
            channel_type,
        },
        privchat_sdk::SdkEvent::ResumeSyncChannelStarted {
            channel_id,
            channel_type,
        } => SdkEvent::ResumeSyncChannelStarted {
            channel_id,
            channel_type,
        },
        privchat_sdk::SdkEvent::ResumeSyncChannelCompleted {
            channel_id,
            channel_type,
            applied,
        } => SdkEvent::ResumeSyncChannelCompleted {
            channel_id,
            channel_type,
            applied: applied as u64,
        },
        privchat_sdk::SdkEvent::ResumeSyncChannelFailed {
            channel_id,
            channel_type,
            classification,
            scope,
            error_code,
            message,
        } => SdkEvent::ResumeSyncChannelFailed {
            channel_id,
            channel_type,
            classification: map_sdk_resume_failure_class(classification),
            scope: map_sdk_resume_escalation_scope(scope),
            error_code,
            message,
        },
        privchat_sdk::SdkEvent::SyncEntitiesApplied {
            entity_type,
            scope,
            queued,
            applied,
            dropped_duplicates,
        } => SdkEvent::SyncEntitiesApplied {
            entity_type,
            scope,
            queued: queued as u64,
            applied: applied as u64,
            dropped_duplicates: dropped_duplicates as u64,
        },
        privchat_sdk::SdkEvent::SyncEntityChanged {
            entity_type,
            entity_id,
            deleted,
        } => SdkEvent::SyncEntityChanged {
            entity_type,
            entity_id,
            deleted,
        },
        privchat_sdk::SdkEvent::SyncChannelApplied {
            channel_id,
            channel_type,
            applied,
        } => SdkEvent::SyncChannelApplied {
            channel_id,
            channel_type,
            applied: applied as u64,
        },
        privchat_sdk::SdkEvent::SyncAllChannelsApplied { applied } => {
            SdkEvent::SyncAllChannelsApplied {
                applied: applied as u64,
            }
        }
        privchat_sdk::SdkEvent::NetworkHintChanged { from, to } => SdkEvent::NetworkHintChanged {
            from: map_sdk_network_hint(from),
            to: map_sdk_network_hint(to),
        },
        privchat_sdk::SdkEvent::OutboundQueueUpdated {
            kind,
            action,
            message_id,
            queue_index,
        } => SdkEvent::OutboundQueueUpdated {
            kind,
            action,
            message_id,
            queue_index: queue_index.map(|v| v as u64),
        },
        privchat_sdk::SdkEvent::TimelineUpdated {
            channel_id,
            channel_type,
            message_id,
            reason,
        } => SdkEvent::TimelineUpdated {
            channel_id,
            channel_type,
            message_id,
            reason,
        },
        privchat_sdk::SdkEvent::MessageSendStatusChanged {
            message_id,
            status,
            server_message_id,
        } => SdkEvent::MessageSendStatusChanged {
            message_id,
            status,
            server_message_id,
        },
        privchat_sdk::SdkEvent::TypingSent {
            channel_id,
            channel_type,
            is_typing,
        } => SdkEvent::TypingSent {
            channel_id,
            channel_type,
            is_typing,
        },
        privchat_sdk::SdkEvent::SubscriptionMessageReceived {
            channel_id,
            topic,
            payload,
            publisher,
            server_message_id,
            timestamp,
        } => SdkEvent::SubscriptionMessageReceived {
            channel_id,
            topic,
            payload,
            publisher,
            server_message_id,
            timestamp,
        },
        privchat_sdk::SdkEvent::PeerReadPtsAdvanced {
            channel_id,
            channel_type,
            reader_id,
            read_pts,
        } => SdkEvent::PeerReadPtsAdvanced {
            channel_id,
            channel_type,
            reader_id,
            read_pts,
        },
        privchat_sdk::SdkEvent::MessageDelivered {
            channel_id,
            channel_type,
            message_id,
            server_message_id,
            delivered_at,
        } => SdkEvent::MessageDelivered {
            channel_id,
            channel_type,
            message_id,
            server_message_id,
            delivered_at,
        },
        privchat_sdk::SdkEvent::MediaDownloadStateChanged { message_id, state } => {
            SdkEvent::MediaDownloadStateChanged {
                message_id,
                state: map_media_download_state(state),
            }
        }
        privchat_sdk::SdkEvent::MediaJobRequested {
            job_id,
            job_kind,
            source_path,
            output_path,
            mime_type,
            message_id,
            timeout_ms,
        } => SdkEvent::MediaJobRequested {
            job_id,
            job_kind,
            source_path,
            output_path,
            mime_type,
            message_id,
            timeout_ms,
        },
        privchat_sdk::SdkEvent::TokenRefreshed { expires_at } => {
            SdkEvent::TokenRefreshed { expires_at }
        }
        privchat_sdk::SdkEvent::AccessTokenRefreshNeeded { code, message } => {
            SdkEvent::AccessTokenRefreshNeeded { code, message }
        }
        privchat_sdk::SdkEvent::ForcedLogout {
            code,
            message,
            source,
        } => SdkEvent::ForcedLogout {
            code,
            message,
            source: map_forced_logout_source(source),
        },
        privchat_sdk::SdkEvent::ShutdownStarted => SdkEvent::ShutdownStarted,
        privchat_sdk::SdkEvent::ShutdownCompleted => SdkEvent::ShutdownCompleted,
    }
}

fn map_forced_logout_source(v: privchat_sdk::ForcedLogoutSource) -> ForcedLogoutSource {
    match v {
        privchat_sdk::ForcedLogoutSource::ConnectAuth => ForcedLogoutSource::ConnectAuth,
        privchat_sdk::ForcedLogoutSource::RpcAuth => ForcedLogoutSource::RpcAuth,
        privchat_sdk::ForcedLogoutSource::Manual => ForcedLogoutSource::Manual,
    }
}

fn map_terminal_reason(v: SdkTerminalReason) -> TerminalReason {
    TerminalReason {
        code: v.code,
        message: v.message,
        source: map_forced_logout_source(v.source),
        at_ms: v.at_ms,
    }
}

fn map_media_download_state(v: privchat_sdk::MediaDownloadState) -> MediaDownloadState {
    match v {
        privchat_sdk::MediaDownloadState::Idle => MediaDownloadState::Idle,
        privchat_sdk::MediaDownloadState::Downloading { bytes, total } => {
            MediaDownloadState::Downloading { bytes, total }
        }
        privchat_sdk::MediaDownloadState::Paused { bytes, total } => {
            MediaDownloadState::Paused { bytes, total }
        }
        privchat_sdk::MediaDownloadState::Done { path } => MediaDownloadState::Done { path },
        privchat_sdk::MediaDownloadState::Failed { code, message } => {
            MediaDownloadState::Failed { code, message }
        }
    }
}

fn map_sequenced_sdk_event(v: SdkSequencedSdkEvent) -> SequencedSdkEvent {
    SequencedSdkEvent {
        sequence_id: v.sequence_id,
        timestamp_ms: v.timestamp_ms,
        event: map_sdk_event(v.event),
    }
}

fn sdk_event_to_json_value(event: &SdkEvent) -> serde_json::Value {
    use serde_json::json;
    match event {
        SdkEvent::ConnectionStateChanged { from, to } => json!({
            "type": "connection_state_changed",
            "from_state": format!("{from:?}"),
            "to_state": format!("{to:?}")
        }),
        SdkEvent::BootstrapCompleted { user_id } => json!({
            "type": "bootstrap_completed",
            "user_id": user_id
        }),
        SdkEvent::SyncStateChanged { state } => json!({
            "type": "sync_state_changed",
            "phase": format!("{:?}", state.phase),
            "run_kind": state.run_kind.map(|kind| format!("{kind:?}")),
            "attempt": state.attempt,
            "error_code": state.error_code,
            "message": state.message,
            "updated_at_ms": state.updated_at_ms
        }),
        SdkEvent::ResumeSyncStarted => json!({
            "type": "resume_sync_started"
        }),
        SdkEvent::ResumeSyncCompleted {
            entity_types_synced,
            channels_scanned,
            channels_applied,
            channel_failures,
        } => json!({
            "type": "resume_sync_completed",
            "entity_types_synced": entity_types_synced,
            "channels_scanned": channels_scanned,
            "channels_applied": channels_applied,
            "channel_failures": channel_failures
        }),
        SdkEvent::ResumeSyncFailed {
            classification,
            scope,
            error_code,
            message,
        } => json!({
            "type": "resume_sync_failed",
            "classification": format!("{classification:?}"),
            "scope": format!("{scope:?}"),
            "error_code": error_code,
            "message": message
        }),
        SdkEvent::ResumeSyncEscalated {
            classification,
            scope,
            reason,
            entity_type,
            channel_id,
            channel_type,
        } => json!({
            "type": "resume_sync_escalated",
            "classification": format!("{classification:?}"),
            "scope": format!("{scope:?}"),
            "reason": reason,
            "entity_type": entity_type,
            "channel_id": channel_id,
            "channel_type": channel_type
        }),
        SdkEvent::ResumeSyncChannelStarted {
            channel_id,
            channel_type,
        } => json!({
            "type": "resume_sync_channel_started",
            "channel_id": channel_id,
            "channel_type": channel_type
        }),
        SdkEvent::ResumeSyncChannelCompleted {
            channel_id,
            channel_type,
            applied,
        } => json!({
            "type": "resume_sync_channel_completed",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "applied": applied
        }),
        SdkEvent::ResumeSyncChannelFailed {
            channel_id,
            channel_type,
            classification,
            scope,
            error_code,
            message,
        } => json!({
            "type": "resume_sync_channel_failed",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "classification": format!("{classification:?}"),
            "scope": format!("{scope:?}"),
            "error_code": error_code,
            "message": message
        }),
        SdkEvent::SyncEntitiesApplied {
            entity_type,
            scope,
            queued,
            applied,
            dropped_duplicates,
        } => json!({
            "type": "sync_entities_applied",
            "entity_type": entity_type,
            "scope": scope,
            "queued": queued,
            "applied": applied,
            "dropped_duplicates": dropped_duplicates
        }),
        SdkEvent::SyncEntityChanged {
            entity_type,
            entity_id,
            deleted,
        } => json!({
            "type": "sync_entity_changed",
            "entity_type": entity_type,
            "entity_id": entity_id,
            "deleted": deleted
        }),
        SdkEvent::SyncChannelApplied {
            channel_id,
            channel_type,
            applied,
        } => json!({
            "type": "sync_channel_applied",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "applied": applied
        }),
        SdkEvent::SyncAllChannelsApplied { applied } => json!({
            "type": "sync_all_channels_applied",
            "applied": applied
        }),
        SdkEvent::NetworkHintChanged { from, to } => json!({
            "type": "network_hint_changed",
            "from_network_hint": format!("{from:?}"),
            "to_network_hint": format!("{to:?}")
        }),
        SdkEvent::OutboundQueueUpdated {
            kind,
            action,
            message_id,
            queue_index,
        } => json!({
            "type": "outbound_queue_updated",
            "kind": kind,
            "action": action,
            "message_id": message_id,
            "queue_index": queue_index
        }),
        SdkEvent::TimelineUpdated {
            channel_id,
            channel_type,
            message_id,
            reason,
        } => json!({
            "type": "timeline_updated",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "message_id": message_id,
            "reason": reason
        }),
        SdkEvent::MessageSendStatusChanged {
            message_id,
            status,
            server_message_id,
        } => json!({
            "type": "message_send_status_changed",
            "message_id": message_id,
            "status": status,
            "server_message_id": server_message_id
        }),
        SdkEvent::TypingSent {
            channel_id,
            channel_type,
            is_typing,
        } => json!({
            "type": "typing_sent",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "is_typing": is_typing
        }),
        SdkEvent::SubscriptionMessageReceived {
            channel_id,
            topic,
            payload,
            publisher,
            server_message_id,
            timestamp,
        } => json!({
            "type": "subscription_message_received",
            "channel_id": channel_id,
            "topic": topic,
            "payload_len": payload.len(),
            "publisher": publisher,
            "server_message_id": server_message_id,
            "timestamp": timestamp
        }),
        SdkEvent::PeerReadPtsAdvanced {
            channel_id,
            channel_type,
            reader_id,
            read_pts,
        } => json!({
            "type": "peer_read_pts_advanced",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "reader_id": reader_id,
            "read_pts": read_pts
        }),
        SdkEvent::MessageDelivered {
            channel_id,
            channel_type,
            message_id,
            server_message_id,
            delivered_at,
        } => json!({
            "type": "message_delivered",
            "channel_id": channel_id,
            "channel_type": channel_type,
            "message_id": message_id,
            "server_message_id": server_message_id,
            "delivered_at": delivered_at
        }),
        SdkEvent::MediaDownloadStateChanged { message_id, state } => json!({
            "type": "media_download_state_changed",
            "message_id": message_id,
            "state": media_download_state_to_json(state)
        }),
        SdkEvent::MediaJobRequested {
            job_id,
            job_kind,
            source_path,
            output_path,
            mime_type,
            message_id,
            timeout_ms,
        } => json!({
            "type": "media_job_requested",
            "job_id": job_id,
            "job_kind": job_kind,
            "source_path": source_path,
            "output_path": output_path,
            "mime_type": mime_type,
            "message_id": message_id,
            "timeout_ms": timeout_ms
        }),
        SdkEvent::TokenRefreshed { expires_at } => json!({
            "type": "token_refreshed",
            "expires_at": expires_at
        }),
        SdkEvent::AccessTokenRefreshNeeded { code, message } => json!({
            "type": "access_token_refresh_needed",
            "code": code,
            "message": message
        }),
        SdkEvent::ForcedLogout {
            code,
            message,
            source,
        } => json!({
            "type": "forced_logout",
            "code": code,
            "message": message,
            "source": forced_logout_source_to_json(source)
        }),
        SdkEvent::ShutdownStarted => json!({ "type": "shutdown_started" }),
        SdkEvent::ShutdownCompleted => json!({ "type": "shutdown_completed" }),
    }
}

fn forced_logout_source_to_json(source: &ForcedLogoutSource) -> &'static str {
    match source {
        ForcedLogoutSource::ConnectAuth => "connect_auth",
        ForcedLogoutSource::RpcAuth => "rpc_auth",
        ForcedLogoutSource::Manual => "manual",
    }
}

fn media_download_state_to_json(state: &MediaDownloadState) -> serde_json::Value {
    use serde_json::json;
    match state {
        MediaDownloadState::Idle => json!({ "kind": "idle" }),
        MediaDownloadState::Downloading { bytes, total } => json!({
            "kind": "downloading",
            "bytes": bytes,
            "total": total
        }),
        MediaDownloadState::Paused { bytes, total } => json!({
            "kind": "paused",
            "bytes": bytes,
            "total": total
        }),
        MediaDownloadState::Done { path } => json!({
            "kind": "done",
            "path": path
        }),
        MediaDownloadState::Failed { code, message } => json!({
            "kind": "failed",
            "code": code,
            "message": message
        }),
    }
}

fn sequenced_event_to_json_value(event: &SequencedSdkEvent) -> serde_json::Value {
    serde_json::json!({
        "sequence_id": event.sequence_id,
        "timestamp_ms": event.timestamp_ms,
        "event": sdk_event_to_json_value(&event.event)
    })
}

fn is_timeline_event(evt: &SdkEvent) -> bool {
    matches!(
        evt,
        SdkEvent::TimelineUpdated { .. }
            | SdkEvent::MessageSendStatusChanged { .. }
            | SdkEvent::SyncEntityChanged { .. }
            | SdkEvent::SyncChannelApplied { .. }
            | SdkEvent::SyncAllChannelsApplied { .. }
            | SdkEvent::TypingSent { .. }
    )
}

fn is_network_event(evt: &SdkEvent) -> bool {
    matches!(
        evt,
        SdkEvent::ConnectionStateChanged { .. }
            | SdkEvent::NetworkHintChanged { .. }
            | SdkEvent::ResumeSyncStarted
            | SdkEvent::ResumeSyncCompleted { .. }
            | SdkEvent::ResumeSyncFailed { .. }
            | SdkEvent::ResumeSyncEscalated { .. }
            | SdkEvent::TokenRefreshed { .. }
            | SdkEvent::AccessTokenRefreshNeeded { .. }
            | SdkEvent::ForcedLogout { .. }
    )
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
        is_online: v.is_online,
        last_seen_at: v.last_seen_at,
        device_count: v.device_count,
        version: v.version,
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
        mime_type: v.mime_type,
        media_downloaded: v.media_downloaded,
        thumb_status: v.thumb_status,
    }
}

fn map_structured_options(v: Option<StructuredSendOptionsInput>) -> SdkStructuredSendOptions {
    v.map(|options| SdkStructuredSendOptions {
        in_reply_to_message_id: options.in_reply_to_message_id,
        mentioned_user_ids: options.mentioned_user_ids,
    })
    .unwrap_or_default()
}

fn map_link_message_input(v: LinkMessageInput) -> SdkLinkMessageInput {
    SdkLinkMessageInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        url: v.url,
        title: v.title,
        description: v.description,
        thumbnail_file_id: v.thumbnail_file_id,
        options: map_structured_options(v.options),
    }
}

fn map_location_message_input(v: LocationMessageInput) -> SdkLocationMessageInput {
    SdkLocationMessageInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        latitude: v.latitude,
        longitude: v.longitude,
        coordinate_system: v.coordinate_system,
        name: v.name,
        address: v.address,
        poi_id: v.poi_id,
        poi_source: v.poi_source,
        thumbnail_file_id: v.thumbnail_file_id,
        options: map_structured_options(v.options),
    }
}

fn map_contact_card_message_input(v: ContactCardMessageInput) -> SdkContactCardMessageInput {
    SdkContactCardMessageInput {
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        user_id: v.user_id,
        options: map_structured_options(v.options),
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
        version: v.last_msg_timestamp.max(0),
        // FFI 直接 upsert（本地建会话等）不是 peer 的权威来源；交给 channel 同步填充。
        // upsert SQL 用 COALESCE，None 不会覆盖已存的 peer_user_id。
        peer_user_id: None,
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
    let body = map_message_content(privchat_sdk::message_content::project_stored_message(&v));
    let (display_content, mut reply_to_message_id, mut mentioned_user_ids) =
        extract_envelope_fields(&v.content);
    // Inbound sync pipeline stores the plain text in `content` and the full envelope JSON
    // in `extra`. Fall back to parsing `extra` when envelope fields are absent from content,
    // so reply/mention metadata survives the push → sync → storage round trip.
    if reply_to_message_id.is_none() && mentioned_user_ids.is_empty() && !v.extra.is_empty() {
        let (_, extra_reply, extra_mentions) = extract_envelope_fields(&v.extra);
        reply_to_message_id = extra_reply;
        mentioned_user_ids = extra_mentions;
    }
    StoredMessage {
        message_id: v.message_id,
        server_message_id: v.server_message_id,
        local_message_id: v.local_message_id,
        channel_id: v.channel_id,
        channel_type: v.channel_type,
        from_uid: v.from_uid,
        message_type: v.message_type,
        content: display_content,
        body,
        status: v.status,
        created_at: v.created_at,
        updated_at: v.updated_at,
        extra: v.extra,
        revoked: v.revoked,
        revoked_by: v.revoked_by,
        mime_type: v.mime_type,
        media_downloaded: v.media_downloaded,
        thumb_status: v.thumb_status,
        delivered: v.delivered,
        pts: v.pts,
        reply_to_message_id,
        mentioned_user_ids,
    }
}

fn map_message_content(
    v: privchat_sdk::message_content::MessageContentProjection,
) -> MessageContentBody {
    MessageContentBody {
        kind: v.kind,
        text: v.text,
        entities: v
            .entities
            .into_iter()
            .map(|e| MessageTextEntity {
                kind: e.kind,
                start: e.start,
                end: e.end,
                text: e.text,
                value: e.value,
                user_id: e.user_id,
            })
            .collect(),
        reply_to_message_id: v.reply_to_message_id,
        mentioned_user_ids: v.mentioned_user_ids,
        attachment_url: v.attachment_url,
        attachment_file_id: v.attachment_file_id,
        thumbnail_url: v.thumbnail_url,
        thumbnail_file_id: v.thumbnail_file_id,
        file_name: v.file_name,
        file_size: v.file_size,
        duration: v.duration,
        width: v.width,
        height: v.height,
        latitude: v.latitude,
        longitude: v.longitude,
        coordinate_system: v.coordinate_system,
        location_name: v.location_name,
        address: v.address,
        poi_id: v.poi_id,
        poi_source: v.poi_source,
        link_url: v.link_url,
        link_title: v.link_title,
        link_description: v.link_description,
        contact_user_id: v.contact_user_id,
        contact_name: v.contact_name,
        contact_avatar_url: v.contact_avatar_url,
        system_template: v.system_template,
        system_refs: v
            .system_refs
            .into_iter()
            .map(|r| MessageContentRef {
                kind: r.kind,
                target_id: r.target_id,
                text: r.text,
            })
            .collect(),
        money_ref_id: v.money_ref_id,
        money_title: v.money_title,
        money_summary: v.money_summary,
        money_status: v.money_status,
        money_amount_text: v.money_amount_text,
        money_scene: v.money_scene,
        money_type: v.money_type,
    }
}

/// 将存储层原始 content 字符串解析成 (显示文本, reply_to, mentions)。
///
/// - 若 content 是 `MessagePayloadEnvelope` JSON（带 `content`/`reply_to_message_id`/
///   `mentioned_user_ids` 等键），提取 envelope.content 作为显示文本，并透出引用 / mention。
/// - 否则原样返回 content（纯文本或旧消息）。
fn extract_envelope_fields(raw: &str) -> (String, Option<String>, Vec<u64>) {
    // Local-content path: parses the SDK's stored JSON envelope (legacy
    // shape, opaque metadata Value). The wire-canonical FB envelope is
    // only used when going through the transport.
    use privchat_protocol::message::LocalMessagePayloadEnvelope;
    if raw.is_empty() {
        return (String::new(), None, Vec::new());
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => {
            let looks_like_envelope = value
                .as_object()
                .map(|obj| {
                    obj.contains_key("content")
                        && (obj.contains_key("metadata")
                            || obj.contains_key("reply_to_message_id")
                            || obj.contains_key("mentioned_user_ids")
                            || obj.contains_key("message_source"))
                })
                .unwrap_or(false);
            if !looks_like_envelope {
                return (raw.to_string(), None, Vec::new());
            }
            match serde_json::from_value::<LocalMessagePayloadEnvelope>(value) {
                Ok(env) => (
                    env.content,
                    env.reply_to_message_id,
                    env.mentioned_user_ids.unwrap_or_default(),
                ),
                Err(_) => (raw.to_string(), None, Vec::new()),
            }
        }
        Err(_) => (raw.to_string(), None, Vec::new()),
    }
}

fn map_stored_channel(v: SdkStoredChannel) -> StoredChannel {
    let last_message_body = v.last_message_type.map(|message_type| {
        let synthetic = SdkStoredMessage {
            message_id: v.last_local_message_id,
            server_message_id: None,
            local_message_id: None,
            channel_id: v.channel_id,
            channel_type: v.channel_type,
            from_uid: 0,
            message_type,
            content: v.last_msg_content.clone(),
            status: 2,
            created_at: v.last_msg_timestamp,
            updated_at: v.updated_at,
            extra: String::new(),
            revoked: v.last_message_is_revoked,
            revoked_by: None,
            mime_type: None,
            media_downloaded: false,
            thumb_status: 0,
            delivered: false,
            pts: None,
        };
        map_message_content(privchat_sdk::message_content::project_stored_message(
            &synthetic,
        ))
    });
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
        last_message_body,
        updated_at: v.updated_at,
        peer_user_id: v.peer_user_id,
        member_count: v.member_count.max(0) as u32,
        last_message_type: v.last_message_type,
        last_message_is_revoked: v.last_message_is_revoked,
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
        delivered: v.delivered,
        delivered_at: v.delivered_at,
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
        version: v.updated_at.max(0),
        updated_at: v.updated_at,
    }
}

/// AVATAR_CACHE_SPEC P1 §3.5 local-first 解析：本地缓存路径只有在文件确实
/// 存在时才透传给宿主（文件被外部清理时静默回落网络 URL）。
fn resolve_avatar_local_path(path: String) -> String {
    if !path.is_empty() && std::path::Path::new(&path).exists() {
        path
    } else {
        String::new()
    }
}

fn map_stored_user(v: SdkStoredUser) -> StoredUser {
    StoredUser {
        user_id: v.user_id,
        username: v.username,
        nickname: v.nickname,
        alias: v.alias,
        avatar: v.avatar,
        avatar_local_path: resolve_avatar_local_path(v.avatar_local_path),
        user_type: v.user_type,
        is_deleted: v.is_deleted,
        channel_id: v.channel_id,
        updated_at: v.updated_at,
    }
}

fn map_upsert_friend(v: UpsertFriendInput) -> SdkUpsertFriendInput {
    // FFI 直写路径默认按 accepted (status=1) 处理——request 态由 server 的
    // entity sync 链路灌入 friend 表，host 一侧不会直接构造请求态行。
    SdkUpsertFriendInput {
        user_id: v.user_id,
        tags: v.tags,
        is_pinned: v.is_pinned,
        created_at: v.created_at,
        version: v.updated_at.max(0),
        updated_at: v.updated_at,
        status: 1,
        is_outgoing: None,
        request_message: None,
        request_source: None,
        request_source_id: None,
    }
}

fn map_stored_friend(v: SdkStoredFriend) -> StoredFriend {
    StoredFriend {
        user_id: v.user_id,
        username: v.username,
        nickname: v.nickname,
        alias: v.alias,
        avatar: v.avatar,
        avatar_local_path: resolve_avatar_local_path(v.avatar_local_path),
        tags: v.tags,
        is_pinned: v.is_pinned,
        created_at: v.created_at,
        updated_at: v.updated_at,
        status: v.status,
        is_outgoing: v.is_outgoing,
        request_message: v.request_message,
        request_source: v.request_source,
        request_source_id: v.request_source_id,
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
        member_count: None,
        created_at: v.created_at,
        version: v.updated_at.max(0),
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
        version: v.updated_at.max(0),
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

fn map_local_account_summary(v: SdkLocalAccountSummary) -> LocalAccountSummary {
    LocalAccountSummary {
        uid: v.uid,
        created_at: v.created_at,
        last_login_at: v.last_login_at,
        is_active: v.is_active,
    }
}

fn map_reactions_map(reactions: HashMap<String, Vec<u64>>) -> Vec<MessageReactionEmojiUsersView> {
    reactions
        .into_iter()
        .map(|(emoji, user_ids)| MessageReactionEmojiUsersView { emoji, user_ids })
        .collect()
}

fn map_sticker_package_info(
    v: privchat_protocol::rpc::StickerPackageInfo,
) -> StickerPackageInfoView {
    StickerPackageInfoView {
        package_id: v.package_id,
        name: v.name,
        thumbnail_url: v.thumbnail_url,
        author: v.author,
        description: v.description,
        sticker_count: v.sticker_count as u64,
        stickers: v
            .stickers
            .unwrap_or_default()
            .into_iter()
            .map(|s| StickerInfoView {
                sticker_id: s.sticker_id,
                package_id: s.package_id,
                image_url: s.image_url,
                alt_text: s.alt_text,
                emoji: s.emoji,
                width: s.width,
                height: s.height,
                mime_type: s.mime_type,
            })
            .collect(),
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
    event_envelope_cursor: Arc<AtomicU64>,
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
        let event_envelope_cursor = Arc::new(AtomicU64::new(inner.last_event_sequence_id()));
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
            event_envelope_cursor,
        })
    }

    async fn require_current_user_id(&self) -> Result<u64, PrivchatFfiError> {
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
            let messages = self
                .list_messages(ch.channel_id, ch.channel_type, 1, 200)
                .await?;
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

    /// 读取最近一次 Terminal 认证错误快照。
    /// `None` 表示当前没有未清的 ForcedLogout 记录（例如已成功 Connect 一次）。
    pub async fn last_terminal_reason(&self) -> Result<Option<TerminalReason>, PrivchatFfiError> {
        let value = self
            .inner
            .last_terminal_reason()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(value.map(map_terminal_reason))
    }

    /// 读取当前会话的 access token（只读拉取模式）。
    /// SDK 权威地管理 token；app 层通常无需直接使用，仅在需要透传给外部服务时调用。
    pub async fn get_current_access_token(&self) -> Result<Option<String>, PrivchatFfiError> {
        self.inner
            .get_current_access_token()
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn set_network_hint(&self, hint: NetworkHint) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_network_hint(map_network_hint(hint))
            .await
            .map_err(PrivchatFfiError::from)
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
                data_dir: String::new(),
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
        let mut cfg = self.config.lock().map_err(|_| PrivchatFfiError::SdkError {
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
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match rx.try_recv() {
                Ok(evt) => return Ok(Some(map_sdk_event(evt))),
                Err(TryRecvError::Lagged(_)) => return Ok(None),
                Err(TryRecvError::Closed) => return Ok(None),
                Err(TryRecvError::Empty) => {
                    let remain = deadline.saturating_duration_since(std::time::Instant::now());
                    if remain.is_zero() {
                        return Ok(None);
                    }
                    poll_wait(remain).await;
                }
            }
        }
    }

    pub async fn next_event_envelope(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SequencedSdkEvent>, PrivchatFfiError> {
        self.event_poll_count.fetch_add(1, Ordering::Relaxed);
        let timeout = std::time::Duration::from_millis(timeout_ms.max(1));
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let cursor = self.event_envelope_cursor.load(Ordering::Acquire);
            if let Some(evt) = self.inner.events_since(cursor, 1).into_iter().next() {
                self.event_envelope_cursor
                    .store(evt.sequence_id, Ordering::Release);
                return Ok(Some(map_sequenced_sdk_event(evt)));
            }
            let remain = deadline.saturating_duration_since(std::time::Instant::now());
            if remain.is_zero() {
                return Ok(None);
            }
            poll_wait(remain).await;
        }
    }

    pub async fn next_timeline_event_envelope(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SequencedSdkEvent>, PrivchatFfiError> {
        self.event_poll_count.fetch_add(1, Ordering::Relaxed);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(1));
        let mut cursor = self.inner.last_event_sequence_id();
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let mut rx = self.event_rx.lock().await;
            match rx.try_recv() {
                Ok(_) | Err(TryRecvError::Lagged(_)) => {
                    let replay = self
                        .inner
                        .timeline_events_since(cursor, self.inner.event_history_limit());
                    if let Some(evt) = replay.last().cloned() {
                        return Ok(Some(map_sequenced_sdk_event(evt)));
                    }
                    cursor = self.inner.last_event_sequence_id();
                }
                Err(TryRecvError::Closed) => return Ok(None),
                Err(TryRecvError::Empty) => {
                    let remain = deadline.saturating_duration_since(std::time::Instant::now());
                    if remain.is_zero() {
                        return Ok(None);
                    }
                    poll_wait(remain).await;
                }
            }
        }
    }

    pub async fn next_timeline_event(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SdkEvent>, PrivchatFfiError> {
        Ok(self
            .next_timeline_event_envelope(timeout_ms)
            .await?
            .map(|v| v.event))
    }

    pub async fn next_network_event_envelope(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SequencedSdkEvent>, PrivchatFfiError> {
        self.event_poll_count.fetch_add(1, Ordering::Relaxed);
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms.max(1));
        let mut cursor = self.inner.last_event_sequence_id();
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let mut rx = self.event_rx.lock().await;
            match rx.try_recv() {
                Ok(_) | Err(TryRecvError::Lagged(_)) => {
                    let replay = self
                        .inner
                        .network_events_since(cursor, self.inner.event_history_limit());
                    if let Some(evt) = replay.last().cloned() {
                        return Ok(Some(map_sequenced_sdk_event(evt)));
                    }
                    cursor = self.inner.last_event_sequence_id();
                }
                Err(TryRecvError::Closed) => return Ok(None),
                Err(TryRecvError::Empty) => {
                    let remain = deadline.saturating_duration_since(std::time::Instant::now());
                    if remain.is_zero() {
                        return Ok(None);
                    }
                    poll_wait(remain).await;
                }
            }
        }
    }

    pub async fn next_network_event(
        &self,
        timeout_ms: u64,
    ) -> Result<Option<SdkEvent>, PrivchatFfiError> {
        Ok(self
            .next_network_event_envelope(timeout_ms)
            .await?
            .map(|v| v.event))
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

    pub fn recent_timeline_events(&self, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .recent_timeline_events(cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub fn timeline_events_since(&self, sequence_id: u64, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .timeline_events_since(sequence_id, cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub fn recent_network_events(&self, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .recent_network_events(cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub fn network_events_since(&self, sequence_id: u64, limit: u64) -> Vec<SequencedSdkEvent> {
        let cap = if limit == 0 {
            0usize
        } else {
            limit.min(self.inner.event_history_limit() as u64) as usize
        };
        self.inner
            .network_events_since(sequence_id, cap)
            .into_iter()
            .map(map_sequenced_sdk_event)
            .collect()
    }

    pub fn recent_timeline_plain_events(&self, limit: u64) -> Vec<SdkEvent> {
        self.recent_timeline_events(limit)
            .into_iter()
            .map(|e| e.event)
            .filter(is_timeline_event)
            .collect()
    }

    pub fn recent_network_plain_events(&self, limit: u64) -> Vec<SdkEvent> {
        self.recent_network_events(limit)
            .into_iter()
            .map(|e| e.event)
            .filter(is_network_event)
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
        eprintln!("[FFI] run_bootstrap_sync: enter");
        let out = self
            .inner
            .run_bootstrap_sync()
            .await
            .map_err(PrivchatFfiError::from);
        eprintln!("[FFI] run_bootstrap_sync: done ok={}", out.is_ok());
        out
    }

    pub async fn ensure_synced(&self) -> Result<(), PrivchatFfiError> {
        self.inner
            .ensure_synced()
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn sync_state(&self) -> Result<SyncStateSnapshot, PrivchatFfiError> {
        self.inner
            .sync_state()
            .await
            .map(map_sync_state)
            .map_err(PrivchatFfiError::from)
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

    pub async fn get_connection_summary(&self) -> Result<ConnectionSummary, PrivchatFfiError> {
        let state = self.connection_state().await?;
        let snapshot = self.session_snapshot().await?;
        let state_text = match state {
            ConnectionState::New => "new",
            ConnectionState::Connected => "connected",
            ConnectionState::LoggedIn => "logged_in",
            ConnectionState::Authenticated => "authenticated",
            ConnectionState::Terminated => "terminated",
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
        let on_message_received_registered =
            self.on_message_received_registered.load(Ordering::Relaxed);
        let on_reaction_changed_registered =
            self.on_reaction_changed_registered.load(Ordering::Relaxed);
        let on_typing_indicator_registered =
            self.on_typing_indicator_registered.load(Ordering::Relaxed);
        let video_process_hook_registered =
            self.video_process_hook_registered.load(Ordering::Relaxed);
        Ok(ConnectionSummary {
            state: state_text.to_string(),
            user_id,
            bootstrap_completed,
            app_in_background,
            supervised_sync_running,
            send_queue_enabled,
            event_poll_count,
            lifecycle_hook_registered,
            transport_disconnect_listener_started,
            on_connection_state_changed_registered,
            on_message_received_registered,
            on_reaction_changed_registered,
            on_typing_indicator_registered,
            video_process_hook_registered,
        })
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
        self.lifecycle_hook_registered
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] lifecycle hook registered");
    }

    pub fn start_transport_disconnect_listener(&self) {
        self.transport_disconnect_listener_started
            .store(true, Ordering::Relaxed);
        eprintln!("[FFI] transport disconnect listener started (event-poll mode)");
    }

    pub async fn log_connection_state(&self) -> Result<ConnectionSummary, PrivchatFfiError> {
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
        spawn_background_future("sync-entities", async move {
            let _ = inner.sync_entities(entity_type, scope).await;
        });
        Ok(())
    }

    pub async fn sync_messages(&self) -> Result<u64, PrivchatFfiError> {
        self.sync_all_channels().await
    }

    pub async fn sync_messages_in_background(&self) -> Result<(), PrivchatFfiError> {
        let inner = self.inner.clone();
        spawn_background_future("sync-messages", async move {
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

    pub fn image_send_max_edge(&self) -> u32 {
        4096
    }

    pub fn event_config(&self) -> EventConfigView {
        EventConfigView {
            broadcast_capacity: 256,
            polling_api: "next_event(timeout_ms)".to_string(),
            polling_envelope_api: "next_event_envelope(timeout_ms)".to_string(),
            event_poll_count: self.event_poll_count.load(Ordering::Relaxed),
            sequence_cursor: self.event_stream_cursor(),
            replay_api: "events_since(sequence_id, limit)".to_string(),
            history_limit: self.inner.event_history_limit() as u64,
        }
    }

    pub fn queue_config(&self) -> QueueConfigView {
        QueueConfigView {
            normal_queue: "single".to_string(),
            file_queue: "multi".to_string(),
        }
    }

    pub fn retry_config(&self) -> RetryConfigView {
        RetryConfigView {
            message_retry: true,
            strategy: "manual+queue".to_string(),
        }
    }

    pub fn http_client_config(&self) -> HttpClientConfigView {
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
        HttpClientConfigView {
            connection_timeout_secs: self.connection_timeout(),
            tls,
            scheme: scheme.to_string(),
        }
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

    /// 订阅频道事件（进入聊天页面时调用，接收 typing / presence 等状态事件）
    /// channel_type: 0=Private, 1=Group, 2=Room
    /// token: 可选，Room 类型订阅时传入业务 API 签发的 ticket（JWT）
    pub async fn subscribe_channel(
        &self,
        channel_id: u64,
        channel_type: u8,
        token: Option<String>,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .subscribe_channel(channel_id, channel_type, token)
            .await
            .map_err(PrivchatFfiError::from)
    }

    /// 取消订阅频道事件（离开聊天页面时调用）
    /// channel_type: 0=Private, 1=Group, 2=Room
    pub async fn unsubscribe_channel(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .unsubscribe_channel(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    /// Channel Transfer client→app RPC. Sends a wire `TransferRequest`
    /// (biz_type=19) and awaits the matching `TransferResponse` (biz_type=20).
    /// `timeout_ms = 0` falls back to the SDK default (5000 ms).
    /// See `02-server/CHANNEL_TRANSFER_SPEC.md` v2.0 and
    /// `07-application/BOT_INTERACTION_SPEC.md` for typical routes.
    pub async fn transfer(
        &self,
        channel_id: u64,
        route: String,
        body: Vec<u8>,
        timeout_ms: u64,
    ) -> Result<TransferReplyView, PrivchatFfiError> {
        let reply = self
            .inner
            .transfer(channel_id, route, body, timeout_ms)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(TransferReplyView {
            request_id: reply.request_id,
            channel_id: reply.channel_id,
            code: reply.code,
            message: reply.message,
            data: reply.data,
        })
    }

    pub async fn batch_get_presence(
        &self,
        user_ids: Vec<u64>,
    ) -> Result<Vec<PresenceStatus>, PrivchatFfiError> {
        let out = self
            .inner
            .batch_get_presence(user_ids)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_presence_status).collect())
    }

    pub async fn get_presence(
        &self,
        user_id: u64,
    ) -> Result<Option<PresenceStatus>, PrivchatFfiError> {
        Ok(self
            .inner
            .get_cached_presence(user_id)
            .map(map_presence_status))
    }

    pub async fn clear_presence_cache(&self) -> Result<(), PrivchatFfiError> {
        self.inner.clear_presence_cache();
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
        #[derive(Deserialize)]
        struct EventPollReq {
            timeout_ms: Option<u64>,
            limit: Option<u64>,
        }

        if route == "__sdk.next_event_json" {
            let req = serde_json::from_str::<EventPollReq>(&body_json).unwrap_or(EventPollReq {
                timeout_ms: Some(1000),
                limit: None,
            });
            let timeout_ms = req.timeout_ms.unwrap_or(1000);
            let event = self.next_event_envelope(timeout_ms).await?;
            return Ok(match event {
                Some(v) => sequenced_event_to_json_value(&v).to_string(),
                None => "null".to_string(),
            });
        }

        if route == "__sdk.recent_events_json" {
            let req = serde_json::from_str::<EventPollReq>(&body_json).unwrap_or(EventPollReq {
                timeout_ms: None,
                limit: Some(100),
            });
            let limit = req.limit.unwrap_or(100).min(2048);
            let events: Vec<serde_json::Value> = self
                .inner
                .recent_events(limit as usize)
                .into_iter()
                .map(map_sequenced_sdk_event)
                .map(|evt| sequenced_event_to_json_value(&evt))
                .collect();
            return Ok(serde_json::Value::Array(events).to_string());
        }

        self.inner
            .rpc_call(route, body_json)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn search_users(
        &self,
        query: String,
    ) -> Result<Vec<SearchUserEntry>, PrivchatFfiError> {
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
        Ok(resp
            .users
            .into_iter()
            .map(|u| SearchUserEntry {
                user_id: u.user_id,
                username: u.username,
                nickname: u.nickname,
                avatar_url: u.avatar_url,
                user_type: i32::from(u.user_type),
                search_session_id: u.search_session_id,
                is_friend: u.is_friend,
                can_send_message: u.can_send_message,
                is_follow: u.is_follow,
            })
            .collect())
    }

    /// 关注一个 Bot（user_type=2）；server 写 `privchat_bot_follow` + 通知 application
    /// 写 `privchat_business_channel` binding。返回 channel_id 后即可 Subscribe + Transfer。
    ///
    /// Spec: `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §3.1。
    pub async fn follow_bot(&self, bot_user_id: u64) -> Result<BotFollowResult, PrivchatFfiError> {
        let resp: BotFollowResponse = rpc_call_typed(
            &self.inner,
            routes::account_bot::FOLLOW,
            &BotFollowRequest { bot_user_id },
        )
        .await?;
        // 让 SDK 本地 store 同步新 channel（与 accept_friend_request 处的 sync_channel 一致）。
        self.inner
            .sync_channel(resp.channel_id, 1)
            .await
            .map_err(PrivchatFfiError::from)?;
        // 立刻拉 bot 详情写入本地 users 表，避免会话头没昵称（spec BOT_INTERACTION_SPEC §3.0.4）。
        // 失败只忽略——不阻塞 follow 主路径。
        let _ = self.persist_user_profile_local(bot_user_id).await;
        Ok(BotFollowResult {
            bot_user_id: resp.bot_user_id,
            channel_id: resp.channel_id,
            account_user_type: i32::from(resp.account_user_type),
            followed: resp.followed,
            created: resp.created,
        })
    }

    /// 拉一次 `account/user/detail` 并把对端用户写入本地 users 表。
    /// 用于 follow 后让会话头显示昵称/头像，spec BOT_INTERACTION_SPEC §3.0。
    async fn persist_user_profile_local(
        &self,
        target_user_id: u64,
    ) -> Result<(), PrivchatFfiError> {
        let detail: AccountUserDetailResponse = rpc_call_typed(
            &self.inner,
            routes::account_user::DETAIL,
            &AccountUserDetailRequest {
                target_user_id,
                source: DetailSourceType::Friend.as_str().to_string(),
                source_id: target_user_id.to_string(),
                user_id: 0,
            },
        )
        .await?;
        let now_ms = now_millis();
        self.inner
            .upsert_user(SdkUpsertUserInput {
                user_id: detail.user_id,
                username: Some(detail.username),
                nickname: Some(detail.nickname),
                alias: None,
                avatar: detail.avatar_url.unwrap_or_default(),
                user_type: i32::from(detail.user_type),
                is_deleted: false,
                channel_id: String::new(),
                version: 0,
                updated_at: now_ms,
            })
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(())
    }

    /// 取消关注 Bot；server 切 status=0 但**不**删 channel / 历史 / application 业务行。
    ///
    /// Spec: `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §3.2。
    pub async fn unfollow_bot(
        &self,
        bot_user_id: u64,
    ) -> Result<BotUnfollowResult, PrivchatFfiError> {
        let resp: BotUnfollowResponse = rpc_call_typed(
            &self.inner,
            routes::account_bot::UNFOLLOW,
            &BotUnfollowRequest { bot_user_id },
        )
        .await?;
        Ok(BotUnfollowResult {
            bot_user_id: resp.bot_user_id,
            channel_id: resp.channel_id,
            unfollowed: resp.unfollowed,
        })
    }

    pub async fn send_friend_request(
        &self,
        target_user_id: u64,
        message: Option<String>,
        source: Option<String>,
        source_id: Option<String>,
    ) -> Result<FriendRequestResult, PrivchatFfiError> {
        let resp: FriendApplyResponse = rpc_call_typed(
            &self.inner,
            routes::friend::APPLY,
            &FriendApplyRequest {
                target_user_id,
                message,
                source,
                source_id,
                    grant_id: None,
                from_user_id: 0,
            },
        )
        .await?;
        Ok(FriendRequestResult {
            user_id: resp.user_id,
            username: resp.username,
            status: resp.status,
            added_at: resp.added_at,
            message: resp.message,
        })
    }

    pub async fn get_friend_pending_requests(
        &self,
    ) -> Result<Vec<FriendPendingEntry>, PrivchatFfiError> {
        let resp: FriendPendingResponse = rpc_call_typed(
            &self.inner,
            routes::friend::PENDING,
            &FriendPendingRequest { user_id: 0 },
        )
        .await?;
        Ok(resp
            .requests
            .into_iter()
            .map(|item| {
                let u = item.user;
                FriendPendingEntry {
                    from_user_id: item.from_user_id,
                    user: SearchUserEntry {
                        user_id: u.user_id,
                        username: u.username,
                        nickname: u.nickname,
                        avatar_url: u.avatar_url,
                        user_type: u.user_type as i32,
                        search_session_id: u.search_session_id,
                        is_friend: u.is_friend,
                        can_send_message: u.can_send_message,
                        is_follow: u.is_follow,
                    },
                    message: item.message,
                    created_at: item.created_at,
                }
            })
            .collect())
    }

    pub async fn accept_friend_request(
        &self,
        from_user_id: u64,
        message: Option<String>,
    ) -> Result<u64, PrivchatFfiError> {
        // 同步 channel/friend/user 实体的 side-effects 由 PrivchatSdk::apply_rpc_side_effects
        // 在 rpc_call_typed 内部统一处理（friend::ACCEPT 分支）。FFI 不再手写，参见
        // 顶部 `rpc_call_typed` 文档中的 "FFI RPC Convergence Rule"。
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
        Ok(resp)
    }

    pub async fn reject_friend_request(
        &self,
        from_user_id: u64,
        message: Option<String>,
    ) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    /// F-sync.2: 撤回自己发出的、尚未处理的好友申请。
    ///
    /// server 把 friendships.(user_id=me, friend_id=target, status=0) 改成
    /// Recalled(4)，并通过 push + entity sync 广播给双方所有设备。本地状态由
    /// entity sync 拉到 friend 表（status=4），UI Sent tab 据此显示"已撤回"。
    pub async fn recall_friend_request(
        &self,
        target_user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        let resp: FriendRecallResponse = rpc_call_typed(
            &self.inner,
            routes::friend::RECALL,
            &FriendRecallRequest {
                target_user_id,
                from_user_id: 0, // server 端按 session 填
            },
        )
        .await?;
        Ok(resp)
    }

    pub async fn get_or_create_direct_channel(
        &self,
        peer_user_id: u64,
    ) -> Result<DirectChannelResult, PrivchatFfiError> {
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
        self.inner
            .sync_channel(resp.channel_id, 1)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(DirectChannelResult {
            channel_id: resp.channel_id,
            created: resp.created,
        })
    }

    pub async fn create_group(
        &self,
        name: String,
        description: Option<String>,
        member_ids: Option<Vec<u64>>,
    ) -> Result<GroupCreateResult, PrivchatFfiError> {
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
        // 立即落本地 group 行:建群后 entity sync 到达前,channel 名解析的
        // group 表回退才有数据(否则时间窗内群名只能落到兜底)。
        let now = now_millis();
        let _ = self
            .inner
            .upsert_group(SdkUpsertGroupInput {
                group_id: resp.group_id,
                name: Some(resp.name.clone()),
                avatar: String::new(),
                owner_id: Some(resp.creator_id),
                is_dismissed: false,
                member_count: Some(resp.member_count as i64),
                created_at: now,
                version: now.max(0),
                updated_at: now,
            })
            .await;
        Ok(GroupCreateResult {
            group_id: resp.group_id,
            name: resp.name,
            description: resp.description,
            member_count: resp.member_count,
            created_at: resp.created_at,
            creator_id: resp.creator_id,
        })
    }

    pub async fn get_group_info(&self, group_id: u64) -> Result<GroupInfoView, PrivchatFfiError> {
        let resp: GroupInfoResponse = rpc_call_typed(
            &self.inner,
            routes::group::INFO,
            &GroupInfoRequest {
                group_id,
                user_id: 0,
            },
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
                member_count: Some(resp.member_count as i64),
                created_at,
                version: updated_at.max(0),
                updated_at,
            })
            .await;
        Ok(GroupInfoView {
            group_id: resp.group_id,
            name: resp.name,
            description: resp.description,
            avatar_url: resp.avatar_url,
            owner_id: resp.owner_id,
            created_at: resp.created_at,
            updated_at: resp.updated_at,
            member_count: resp.member_count,
            message_count: resp.message_count,
            is_archived: resp.is_archived,
            tags_json: resp.tags.map(|v| v.to_string()),
            custom_fields_json: resp.custom_fields.map(|v| v.to_string()),
        })
    }

    pub async fn fetch_group_members_remote(
        &self,
        group_id: u64,
        _page: Option<u32>,
        _page_size: Option<u32>,
    ) -> Result<GroupMemberRemoteList, PrivchatFfiError> {
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
        let mut out_members = Vec::with_capacity(resp.members.len());
        for entry in &resp.members {
            let user_id = entry.user_id;
            // 本地 group_member 表与 UI 契约的 role 编码:owner=2 / admin=1 / member=0
            // (与生产 DB privchat_group_members.role 一致)。
            let role = match entry.role.to_ascii_lowercase().as_str() {
                "owner" => 2,
                "admin" => 1,
                _ => 0,
            };
            let status = 0;
            let alias = Some(entry.nickname.clone());
            let is_muted = entry.is_muted;
            let joined_at = now;
            let updated_at = now;
            let _ = self
                .inner
                .upsert_group_member(SdkUpsertGroupMemberInput {
                    group_id,
                    user_id,
                    role,
                    status,
                    alias: alias.clone(),
                    is_muted,
                    joined_at,
                    version: updated_at.max(0),
                    updated_at,
                })
                .await;
            out_members.push(GroupMemberRemoteEntry {
                user_id,
                role,
                status,
                alias,
                is_muted,
                joined_at,
                updated_at,
                raw_json: json_encode(entry, "group member item")?,
            });
        }
        Ok(GroupMemberRemoteList {
            members: out_members,
            total: resp.total as u64,
        })
    }

    /// 底层唯一头像 re-cache 能力（CLIENT_GLOBAL_STATE §4 全局统一）：把 `user_id` 的头像从
    /// `avatar_url` 下载到本地并强制落库（avatar / avatar_local_path / avatar_cached_url 三者对齐）。
    /// **任意头像来源**（当前用户 / 好友 / 群成员 / 会话 peer / 资料页刷新）都走这一个入口——
    /// `avatar_local_path` 是展示主字段，`avatar_url` 只是下载源。下载失败返回 Err，不污染旧缓存。
    pub async fn recache_user_avatar(
        &self,
        user_id: u64,
        avatar_url: String,
    ) -> Result<AvatarCacheResult, PrivchatFfiError> {
        let (avatar_local_path, avatar_cached_url) = self
            .inner
            .recache_user_avatar(user_id, &avatar_url)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(AvatarCacheResult {
            user_id,
            avatar_local_path,
            avatar_cached_url,
        })
    }

    /// [`Self::recache_user_avatar`] 的便捷封装 = recache 当前登录用户头像。
    pub async fn recache_self_avatar(
        &self,
        avatar_url: String,
    ) -> Result<AvatarCacheResult, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        self.recache_user_avatar(user_id, avatar_url).await
    }

    pub async fn delete_friend(&self, friend_id: u64) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        let resp: FriendRemoveResponse = rpc_call_typed(
            &self.inner,
            routes::friend::DELETE,
            &FriendRemoveRequest { friend_id, user_id },
        )
        .await?;
        self.inner
            .delete_friend(friend_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(resp)
    }

    /// 设置用户备注（本地）
    pub async fn update_user_alias(
        &self,
        user_id: u64,
        alias: Option<String>,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .update_user_alias(user_id, alias)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(())
    }

    pub async fn add_to_blacklist(&self, blocked_user_id: u64) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn remove_from_blacklist(
        &self,
        blocked_user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn get_blacklist(&self) -> Result<Vec<StoredBlacklistEntry>, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        let resp: BlacklistListResponse = rpc_call_typed(
            &self.inner,
            routes::blacklist::LIST,
            &BlacklistListRequest { user_id },
        )
        .await?;
        let remote_ids: HashSet<u64> = resp.users.iter().map(|u| u.blocked_user_id).collect();
        let local_entries = self
            .inner
            .list_blacklist_entries(10_000, 0)
            .await
            .map_err(PrivchatFfiError::from)?;
        for item in local_entries {
            if !remote_ids.contains(&item.blocked_user_id) {
                let _ = self
                    .inner
                    .delete_blacklist_entry(item.blocked_user_id)
                    .await;
            }
        }
        let ts = now_millis();
        for blocked_user_id in &remote_ids {
            let _ = self
                .inner
                .upsert_blacklist_entry(SdkUpsertBlacklistInput {
                    blocked_user_id: *blocked_user_id,
                    created_at: ts,
                    updated_at: ts,
                })
                .await;
        }
        self.list_blacklist_entries(10_000, 0).await
    }

    pub async fn check_blacklist(
        &self,
        target_user_id: u64,
    ) -> Result<BlacklistCheckResult, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        let resp: BlacklistCheckResponse = rpc_call_typed(
            &self.inner,
            routes::blacklist::CHECK,
            &BlacklistCheckRequest {
                user_id,
                target_user_id,
            },
        )
        .await?;
        if resp.blocked {
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
        Ok(BlacklistCheckResult {
            is_blocked: resp.blocked,
        })
    }

    pub async fn mark_read_to_pts(
        &self,
        channel_id: u64,
        read_pts: u64,
    ) -> Result<u64, PrivchatFfiError> {
        let resp: MessageStatusReadPtsResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_PTS,
            &MessageStatusReadPtsRequest {
                channel_id,
                read_pts,
                last_read_message_id: None,
                client_visible_pts: None,
            },
        )
        .await?;
        let channel_type = self.resolve_channel_type(channel_id).await;
        let _ = self
            .inner
            .project_channel_read_cursor(channel_id, channel_type, resp.last_read_pts)
            .await;
        Ok(resp.last_read_pts)
    }

    pub async fn mark_read_to_pts_blocking(
        &self,
        channel_id: u64,
        read_pts: u64,
    ) -> Result<u64, PrivchatFfiError> {
        self.mark_read_to_pts(channel_id, read_pts).await
    }

    pub async fn recall_message(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
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
            .resolve_local_message_id_by_server_message_id(
                channel_id,
                channel_type,
                server_message_id,
            )
            .await?
        {
            let revoker = self.require_current_user_id().await.ok();
            let _ = self
                .set_message_revoke(local_message_id, true, revoker)
                .await;
        }
        Ok(resp)
    }

    pub async fn recall_message_blocking(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        self.recall_message(server_message_id, channel_id).await
    }

    /// 本地删除消息：删 DB 行 + 清附件目录。不触达服务端。
    /// 返回 true 表示确实删到了行；false 表示消息不存在（幂等）。
    pub async fn delete_message_local(&self, message_id: u64) -> Result<bool, PrivchatFfiError> {
        let stored = self
            .inner
            .delete_message_local(message_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        let Some(stored) = stored else {
            return Ok(false);
        };
        if let Ok(uid) = self.require_current_user_id().await {
            let data_dir = self.config.lock().unwrap().data_dir.clone();
            let root = std::path::Path::new(&data_dir);
            let canonical = privchat_sdk::media_store::get_canonical_message_dir(
                root,
                uid,
                stored.message_id as i64,
                stored.created_at,
            );
            let _ = std::fs::remove_dir_all(&canonical);
            let legacy = root
                .join("users")
                .join(uid.to_string())
                .join("files")
                .join(stored.message_id.to_string());
            if legacy != canonical {
                let _ = std::fs::remove_dir_all(&legacy);
            }
        }
        Ok(true)
    }

    /// 把指定本地消息转发到目标频道。
    ///
    /// 内部做两件事：
    /// 1. 克隆源消息的 `content / message_type / mime_type / extra`，用当前登录用户作为 `from_uid`，
    ///    通过 `enqueue_local_message` 创建新本地行并加入出站队列（走正常发送链路）。
    /// 2. 若源消息带附件（`mime_type` 非空），则把源消息目录下的所有文件整体复制到新消息目录，
    ///    并把 `media_downloaded` 置为 true，让 UI 立即看到本地缩略图 / 文件。
    ///
    /// 调用方负责限制不可转发的类型（比如 VOICE / 撤回消息）——SDK 会拒绝撤回消息但不做类型过滤。
    /// 可选的 note 文本由调用方自行追加 `send_message` 调用，本接口不负责。
    ///
    /// 返回新消息的 `message_id`（本地 rowid）。
    pub async fn forward_message(
        &self,
        src_message_id: u64,
        target_channel_id: u64,
        target_channel_type: i32,
    ) -> Result<u64, PrivchatFfiError> {
        let src = self
            .inner
            .get_message_by_id(src_message_id)
            .await
            .map_err(PrivchatFfiError::from)?
            .ok_or_else(|| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: format!("source message not found: {}", src_message_id),
            })?;

        if src.revoked {
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: "cannot forward a revoked message".to_string(),
            });
        }

        let from_uid = self.require_current_user_id().await?;

        let input = NewMessage {
            channel_id: target_channel_id,
            channel_type: target_channel_type,
            from_uid,
            message_type: src.message_type,
            content: src.content.clone(),
            searchable_word: String::new(),
            setting: 0,
            extra: src.extra.clone(),
            mime_type: src.mime_type.clone(),
            media_downloaded: false,
            thumb_status: src.thumb_status,
        };
        let new_message_id = self.enqueue_local_message(input).await?;

        // 复制附件文件（若有）。任意一步失败都不回滚已创建的消息，仅让 UI 缺少本地缓存。
        if src.mime_type.is_some() {
            let data_dir = self.config.lock().unwrap().data_dir.clone();
            let root = std::path::Path::new(&data_dir);
            let src_dir = privchat_sdk::media_store::get_canonical_message_dir(
                root,
                from_uid,
                src.message_id as i64,
                src.created_at,
            );
            let new_created_at = self
                .inner
                .get_message_by_id(new_message_id)
                .await
                .map_err(PrivchatFfiError::from)?
                .map(|m| m.created_at)
                .unwrap_or(src.created_at);
            if let Ok(dst_dir) = privchat_sdk::media_store::ensure_attachment_dir(
                root,
                from_uid,
                new_message_id as i64,
                new_created_at,
            ) {
                let mut copied_any = false;
                if src_dir.exists() {
                    if let Ok(entries) = std::fs::read_dir(&src_dir) {
                        for entry in entries.flatten() {
                            let src_path = entry.path();
                            if src_path.is_file() {
                                if let Some(name) = src_path.file_name() {
                                    let dst_path = dst_dir.join(name);
                                    if std::fs::copy(&src_path, &dst_path).is_ok() {
                                        copied_any = true;
                                    }
                                }
                            }
                        }
                    }
                }
                if copied_any {
                    let _ = self
                        .inner
                        .update_media_downloaded(new_message_id, true)
                        .await;
                }
            }
        }

        Ok(new_message_id)
    }

    pub async fn add_reaction(
        &self,
        server_message_id: u64,
        channel_id: Option<u64>,
        emoji: String,
    ) -> Result<bool, PrivchatFfiError> {
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
                let uid = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn add_reaction_blocking(
        &self,
        server_message_id: u64,
        channel_id: Option<u64>,
        emoji: String,
    ) -> Result<bool, PrivchatFfiError> {
        self.add_reaction(server_message_id, channel_id, emoji)
            .await
    }

    pub async fn remove_reaction(
        &self,
        server_message_id: u64,
        emoji: String,
    ) -> Result<bool, PrivchatFfiError> {
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
        let uid = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn list_reactions(
        &self,
        server_message_id: u64,
    ) -> Result<MessageReactionListView, PrivchatFfiError> {
        let resp: MessageReactionListResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::LIST,
            &MessageReactionListRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await?;
        Ok(MessageReactionListView {
            success: resp.success,
            total_count: resp.total_count as u64,
            reactions: map_reactions_map(resp.reactions),
        })
    }

    pub async fn reaction_stats(
        &self,
        server_message_id: u64,
    ) -> Result<MessageReactionStatsView, PrivchatFfiError> {
        let resp: MessageReactionStatsResponse = rpc_call_typed(
            &self.inner,
            routes::message_reaction::STATS,
            &MessageReactionStatsRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await?;
        Ok(MessageReactionStatsView {
            success: resp.success,
            total_count: resp.stats.total_count as u64,
            reactions: map_reactions_map(resp.stats.reactions),
        })
    }

    pub async fn reactions(
        &self,
        server_message_id: u64,
    ) -> Result<MessageReactionListView, PrivchatFfiError> {
        self.list_reactions(server_message_id).await
    }

    pub async fn reactions_batch(
        &self,
        server_message_ids: Vec<u64>,
    ) -> Result<ReactionsBatchView, PrivchatFfiError> {
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
            items.push(ReactionsBatchItemView {
                server_message_id: id,
                success: value.success,
                total_count: value.total_count as u64,
                reactions: map_reactions_map(value.reactions),
            });
        }
        Ok(ReactionsBatchView { items })
    }

    pub async fn pin_channel(
        &self,
        channel_id: u64,
        pinned: bool,
    ) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn hide_channel(&self, channel_id: u64) -> Result<bool, PrivchatFfiError> {
        let resp: ChannelHideResponse = rpc_call_typed(
            &self.inner,
            routes::channel::HIDE,
            &ChannelHideRequest {
                user_id: 0,
                channel_id,
            },
        )
        .await?;
        Ok(resp)
    }

    /// 设置本地 channel 隐藏标记（不触达服务端）。
    /// 隐藏后会话列表不再显示该 channel，收到新消息时自动取消隐藏。
    pub async fn set_channel_hidden_local(
        &self,
        channel_id: u64,
        hidden: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_channel_hidden(channel_id, hidden)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(())
    }

    /// 本地删除 channel：标记隐藏 + 清空所有关联消息与附件文件。不触达服务端。
    /// 返回 true 表示 channel 原本存在；false 表示 channel 不存在或没有消息（幂等）。
    pub async fn delete_channel_local(&self, channel_id: u64) -> Result<bool, PrivchatFfiError> {
        let messages = self
            .inner
            .delete_channel_local(channel_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        if let Ok(uid) = self.require_current_user_id().await {
            let data_dir = self.config.lock().unwrap().data_dir.clone();
            let root = std::path::Path::new(&data_dir);
            for stored in &messages {
                let canonical = privchat_sdk::media_store::get_canonical_message_dir(
                    root,
                    uid,
                    stored.message_id as i64,
                    stored.created_at,
                );
                let _ = std::fs::remove_dir_all(&canonical);
                let legacy = root
                    .join("users")
                    .join(uid.to_string())
                    .join("files")
                    .join(stored.message_id.to_string());
                if legacy != canonical {
                    let _ = std::fs::remove_dir_all(&legacy);
                }
            }
        }
        Ok(!messages.is_empty())
    }

    pub async fn mute_channel(
        &self,
        channel_id: u64,
        muted: bool,
    ) -> Result<bool, PrivchatFfiError> {
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
        Ok(resp)
    }

    pub async fn update_device_push_state(
        &self,
        device_id: String,
        apns_armed: bool,
        push_token: Option<String>,
    ) -> Result<DevicePushUpdateView, PrivchatFfiError> {
        let resp: DevicePushUpdateResponse = rpc_call_typed(
            &self.inner,
            routes::device::PUSH_UPDATE,
            &DevicePushUpdateRequest {
                device_id,
                apns_armed,
                push_token,
                vendor: None,
            },
        )
        .await?;
        Ok(DevicePushUpdateView {
            device_id: resp.device_id,
            apns_armed: resp.apns_armed,
            user_push_enabled: resp.user_push_enabled,
        })
    }

    pub async fn get_device_push_status(
        &self,
        device_id: Option<String>,
    ) -> Result<DevicePushStatusView, PrivchatFfiError> {
        let resp: DevicePushStatusResponse = rpc_call_typed(
            &self.inner,
            routes::device::PUSH_STATUS,
            &DevicePushStatusRequest { device_id },
        )
        .await?;
        Ok(DevicePushStatusView {
            devices: resp
                .devices
                .into_iter()
                .map(|d| DevicePushInfoView {
                    device_id: d.device_id,
                    apns_armed: d.apns_armed,
                    connected: d.connected,
                    platform: d.platform,
                    vendor: d.vendor,
                })
                .collect(),
            user_push_enabled: resp.user_push_enabled,
        })
    }

    pub async fn get_messages_remote(
        &self,
        channel_id: u64,
        channel_type: i32,
        before_server_message_id: Option<u64>,
        limit: Option<u32>,
    ) -> Result<MessageHistoryView, PrivchatFfiError> {
        // SDK-HISTORY-2（spec §6）：get 拉回的完整消息必须回填本地库（带真实 pts），
        // 此前这里远程直取不落库。回填后调用方应从本地库重查渲染。
        let resp = self
            .inner
            .fetch_channel_history(channel_id, channel_type, before_server_message_id, limit)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(MessageHistoryView {
            messages: resp.messages.into_iter().map(history_item_view).collect(),
            has_more: resp.has_more,
        })
    }

    /// 云端历史搜索（spec §4）。channel_id: Some=CHANNEL scope / None=GLOBAL。
    /// 命中是 snippet 投影不落本地库；服务端限频 300ms/user——UI 必须 debounce
    /// 300–500ms、忽略过期 in-flight 结果、query<2 字符不发起远程。
    pub async fn search_message_history(
        &self,
        query: String,
        channel_id: Option<u64>,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<SearchHistoryView, PrivchatFfiError> {
        let resp = self
            .inner
            .search_message_history(&query, channel_id, cursor, limit)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(SearchHistoryView {
            hits: resp
                .hits
                .into_iter()
                .map(|h| SearchHistoryHitView {
                    channel_id: h.channel_id,
                    message_id: h.message_id,
                    sender_user_id: h.sender_user_id,
                    created_at: h.created_at,
                    message_type: h.message_type,
                    snippet: h.snippet,
                    highlight_ranges: h
                        .highlight_ranges
                        .into_iter()
                        .map(|(start, end)| SearchHighlightRangeView { start, end })
                        .collect(),
                })
                .collect(),
            next_cursor: resp.next_cursor,
        })
    }

    /// jump-to-message 上下文（spec §5）：before/anchor/after 已回填本地库，
    /// UI 从本地重查渲染并定位/高亮 anchor。anchor 不可见（不存在/撤回/删除/无权限）
    /// 时服务端统一 not_found。
    pub async fn get_messages_around(
        &self,
        channel_id: u64,
        channel_type: i32,
        message_id: u64,
        before_limit: Option<u32>,
        after_limit: Option<u32>,
    ) -> Result<MessagesAroundView, PrivchatFfiError> {
        let resp = self
            .inner
            .fetch_messages_around(
                channel_id,
                channel_type,
                message_id,
                before_limit,
                after_limit,
            )
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(MessagesAroundView {
            before_messages: resp
                .before_messages
                .into_iter()
                .map(history_item_view)
                .collect(),
            anchor_message: history_item_view(resp.anchor_message),
            after_messages: resp
                .after_messages
                .into_iter()
                .map(history_item_view)
                .collect(),
            has_more_before: resp.has_more_before,
            has_more_after: resp.has_more_after,
        })
    }

    pub async fn message_read_list(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<MessageReadListView, PrivchatFfiError> {
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        Ok(MessageReadListView {
            readers: resp
                .readers
                .into_iter()
                .map(|r| MessageReadUserView {
                    user_id: r.user_id,
                    username: r.username,
                    nickname: r.nickname,
                    avatar_url: r.avatar_url,
                    read_at: r.read_at,
                })
                .collect(),
            total: resp.total as u64,
        })
    }

    pub async fn message_read_stats(
        &self,
        server_message_id: u64,
        channel_id: u64,
    ) -> Result<MessageReadStatsView, PrivchatFfiError> {
        let resp: MessageReadStatsResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_STATS,
            &MessageReadStatsRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        Ok(MessageReadStatsView {
            read_count: resp.read_count,
            total_count: resp.total_count,
        })
    }

    pub async fn check_friend(&self, friend_id: u64) -> Result<bool, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
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
                    version: ts.max(0),
                    updated_at: ts,
                    status: 1, // friend/check 命中 → 一定是 accepted
                    is_outgoing: None,
                    request_message: None,
                    request_source: None,
                    request_source_id: None,
                })
                .await;
        } else {
            let _ = self.inner.delete_friend(friend_id).await;
        }
        Ok(resp.is_friend)
    }

    pub async fn get_profile(&self) -> Result<ProfileView, PrivchatFfiError> {
        let resp: AccountProfileGetResponse = rpc_call_typed(
            &self.inner,
            routes::account_profile::GET,
            &AccountProfileGetRequest { user_id: 0 },
        )
        .await?;
        Ok(ProfileView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn update_profile(
        &self,
        payload: ProfileUpdateInput,
    ) -> Result<ProfileView, PrivchatFfiError> {
        let req = AccountProfileUpdateRequest {
            display_name: payload.display_name,
            avatar_url: payload.avatar_url,
            bio: payload.bio,
            extra_fields: HashMap::new(),
            user_id: 0,
        };
        let resp: AccountProfileUpdateResponse =
            rpc_call_typed(&self.inner, routes::account_profile::UPDATE, &req).await?;
        Ok(ProfileView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn get_privacy_settings(&self) -> Result<PrivacySettingsView, PrivchatFfiError> {
        let resp: AccountPrivacyGetResponse = rpc_call_typed(
            &self.inner,
            routes::privacy::GET,
            &AccountPrivacyGetRequest { user_id: 0 },
        )
        .await?;
        Ok(PrivacySettingsView {
            user_id: resp.user_id,
            allow_add_by_group: resp.allow_add_by_group,
            allow_search_by_phone: resp.allow_search_by_phone,
            allow_search_by_username: resp.allow_search_by_username,
            allow_search_by_email: resp.allow_search_by_email,
            allow_search_by_qrcode: resp.allow_search_by_qrcode,
            allow_view_by_non_friend: resp.allow_view_by_non_friend,
            allow_receive_message_from_non_friend: resp.allow_receive_message_from_non_friend,
            updated_at: resp.updated_at,
        })
    }

    pub async fn update_privacy_settings(
        &self,
        payload: AccountPrivacyUpdateInput,
    ) -> Result<bool, PrivchatFfiError> {
        let mut req = AccountPrivacyUpdateRequest {
            allow_add_by_group: payload.allow_add_by_group,
            allow_search_by_phone: payload.allow_search_by_phone,
            allow_search_by_username: payload.allow_search_by_username,
            allow_search_by_email: payload.allow_search_by_email,
            allow_search_by_qrcode: payload.allow_search_by_qrcode,
            allow_view_by_non_friend: payload.allow_view_by_non_friend,
            allow_receive_message_from_non_friend: payload.allow_receive_message_from_non_friend,
            user_id: 0,
        };
        req.user_id = 0;
        let resp: AccountPrivacyUpdateResponse =
            rpc_call_typed(&self.inner, routes::privacy::UPDATE, &req).await?;
        Ok(resp.success)
    }

    pub async fn qrcode_generate(
        &self,
        qr_type: String,
        payload: String,
        expire_seconds: Option<u64>,
    ) -> Result<QrCodeGenerateView, PrivchatFfiError> {
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
        Ok(QrCodeGenerateView {
            qr_key: resp.qr_key,
            qr_code: resp.qr_code,
            qr_type: resp.qr_type,
            target_id: resp.target_id,
            created_at: resp.created_at,
            expire_at: resp.expire_at,
            max_usage: resp.max_usage,
            used_count: resp.used_count,
        })
    }

    pub async fn qrcode_resolve(
        &self,
        qr_key: String,
        token: Option<String>,
    ) -> Result<QrCodeResolveView, PrivchatFfiError> {
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
        Ok(QrCodeResolveView {
            qr_type: resp.qr_type,
            target_id: resp.target_id,
            action: resp.action,
            data_json: resp.data.map(|v| v.to_string()),
            used_count: resp.used_count,
            max_usage: resp.max_usage,
            expire_at: resp.expire_at,
        })
    }

    pub async fn qrcode_list(
        &self,
        include_revoked: Option<bool>,
    ) -> Result<QrCodeListView, PrivchatFfiError> {
        let creator_id = self.require_current_user_id().await?.to_string();
        let resp: QRCodeListResponse = rpc_call_typed(
            &self.inner,
            routes::qrcode::LIST,
            &QRCodeListRequest {
                creator_id,
                include_revoked: include_revoked.unwrap_or(false),
            },
        )
        .await?;
        Ok(QrCodeListView {
            qr_keys: resp
                .qr_keys
                .into_iter()
                .map(|q| QrCodeEntryView {
                    qr_key: q.qr_key,
                    qr_code: q.qr_code,
                    qr_type: q.qr_type,
                    target_id: q.target_id,
                    created_at: q.created_at,
                    expire_at: q.expire_at,
                    used_count: q.used_count,
                    max_usage: q.max_usage,
                    revoked: q.revoked,
                })
                .collect(),
            total: resp.total as u64,
        })
    }

    /// QR_CODE_SPEC v1.3 — `user/qrcode/get`：读自己的永久 qr_key + URL。
    pub async fn user_qrcode_get(&self) -> Result<UserQrCodeGetView, PrivchatFfiError> {
        let resp: UserQRCodeGetResponse = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::GET,
            &UserQRCodeGetRequest::default(),
        )
        .await?;
        Ok(UserQrCodeGetView {
            qr_key: resp.qr_key,
            qr_code: resp.qr_code,
            user_id: resp.user_id,
        })
    }

    /// QR_CODE_SPEC v1.3 — `user/qrcode/refresh`：旋转自己的 qr_key。
    pub async fn user_qrcode_refresh(&self) -> Result<UserQrCodeRefreshView, PrivchatFfiError> {
        let resp: UserQRCodeRefreshResponse = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::REFRESH,
            &UserQRCodeRefreshRequest::default(),
        )
        .await?;
        Ok(UserQrCodeRefreshView {
            old_qr_key: resp.old_qr_key,
            new_qr_key: resp.new_qr_key,
            qr_code: resp.qr_code,
            user_id: resp.user_id,
        })
    }

    /// QR_CODE_SPEC v1.3 — `user/qrcode/resolve`：把对端 qrkey 翻译成最小用户卡片。
    /// 响应**不含** qr_key（避免二次扩散）。
    pub async fn user_qrcode_resolve(
        &self,
        qr_key: String,
    ) -> Result<UserQrCodeResolveView, PrivchatFfiError> {
        let resp: UserQRCodeResolveResponse = rpc_call_typed(
            &self.inner,
            routes::user_qrcode::RESOLVE,
            &UserQRCodeResolveRequest {
                qr_key,
                operator_id: 0,
            },
        )
        .await?;
        Ok(UserQrCodeResolveView {
            user_id: resp.user_id,
            username: resp.username,
            display_name: resp.display_name,
            avatar_url: resp.avatar_url,
            user_type: resp.user_type,
            is_friend: resp.is_friend,
            is_self: resp.is_self,
        })
    }

    pub async fn search_user_by_qrcode(
        &self,
        qr_key: String,
        token: Option<String>,
    ) -> Result<AccountSearchResultView, PrivchatFfiError> {
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
        Ok(AccountSearchResultView {
            users: resp
                .users
                .into_iter()
                .map(|u| SearchUserEntry {
                    user_id: u.user_id,
                    username: u.username,
                    nickname: u.nickname,
                    avatar_url: u.avatar_url,
                    user_type: i32::from(u.user_type),
                    search_session_id: u.search_session_id,
                    is_friend: u.is_friend,
                    can_send_message: u.can_send_message,
                    is_follow: u.is_follow,
                })
                .collect(),
            total: resp.total as u64,
            query: resp.query,
        })
    }

    pub async fn account_user_detail_remote(
        &self,
        user_id: u64,
    ) -> Result<AccountUserDetailView, PrivchatFfiError> {
        let resp: AccountUserDetailResponse = rpc_call_typed(
            &self.inner,
            routes::account_user::DETAIL,
            &AccountUserDetailRequest {
                target_user_id: user_id,
                source: DetailSourceType::Friend.as_str().to_string(),
                source_id: user_id.to_string(),
                user_id: 0,
            },
        )
        .await?;
        Ok(AccountUserDetailView {
            user_id: resp.user_id,
            username: resp.username,
            nickname: resp.nickname,
            avatar_url: resp.avatar_url,
            phone: resp.phone,
            email: resp.email,
            user_type: resp.user_type,
            is_friend: resp.is_friend,
            can_send_message: resp.can_send_message,
            source_type: resp.source_type,
            source_id: resp.source_id,
            is_follow: resp.is_follow,
        })
    }

    pub async fn account_user_share_card_remote(
        &self,
        user_id: u64,
    ) -> Result<AccountUserShareCardView, PrivchatFfiError> {
        let receiver_id = self.require_current_user_id().await?;
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
        Ok(AccountUserShareCardView {
            share_key: resp.share_key.unwrap_or_default(),
            share_url: resp.share_url.unwrap_or_default(),
            expire_at: resp.expire_at,
        })
    }

    pub async fn account_user_update_remote(
        &self,
        payload: AccountUserUpdateInput,
    ) -> Result<bool, PrivchatFfiError> {
        let mut req = AccountUserUpdateRequest {
            display_name: payload.display_name,
            avatar_url: payload.avatar_url,
            bio: payload.bio,
            user_id: 0,
        };
        req.user_id = 0;
        let resp: AccountUserUpdateResponse =
            rpc_call_typed(&self.inner, routes::account_user::UPDATE, &req).await?;
        Ok(resp.status.eq_ignore_ascii_case("success"))
    }

    pub async fn auth_logout_remote(&self) -> Result<bool, PrivchatFfiError> {
        let resp: AuthLogoutResponse =
            rpc_call_typed(&self.inner, routes::auth::LOGOUT, &AuthLogoutRequest {}).await?;
        Ok(resp)
    }

    /// Refresh access token via privchat-server `account/auth/refresh` RPC.
    ///
    /// Pure RPC wrapper. **MUST NOT** read/write SDK store, modify state, or
    /// auto-call authenticate. Caller must:
    /// 1) provide `refresh_token` (read from caller's own secure storage);
    /// 2) handle errors (10009/10010 → user re-login; transport → retry);
    /// 3) call `authenticate(uid, result.access_token, device_id)` to apply.
    ///
    /// 详见 TOKEN_REFRESH_SPEC v1.0 §5。
    pub async fn refresh_access_token(
        &self,
        input: RefreshAccessTokenInput,
    ) -> Result<RefreshAccessTokenResult, PrivchatFfiError> {
        let req = privchat_protocol::rpc::auth::AuthRefreshRequest {
            refresh_token: input.refresh_token,
            device_id: input.device_id,
        };
        let resp: privchat_protocol::rpc::auth::AuthRefreshResponse =
            rpc_call_typed(&self.inner, routes::auth::REFRESH, &req).await?;
        Ok(RefreshAccessTokenResult {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at: resp.expires_at,
            refresh_expires_at: None,
        })
    }

    pub async fn qrcode_refresh(
        &self,
        qr_type: String,
        target_id: String,
    ) -> Result<QrCodeRefreshView, PrivchatFfiError> {
        let creator_id = self.require_current_user_id().await?.to_string();
        let resp: QRCodeRefreshResponse = rpc_call_typed(
            &self.inner,
            routes::qrcode::REFRESH,
            &QRCodeRefreshRequest {
                qr_type,
                target_id,
                creator_id,
            },
        )
        .await?;
        Ok(QrCodeRefreshView {
            old_qr_key: resp.old_qr_key,
            new_qr_key: resp.new_qr_key,
            new_qr_code: resp.new_qr_code,
            revoked_at: resp.revoked_at,
        })
    }

    pub async fn qrcode_revoke(
        &self,
        qr_key: String,
    ) -> Result<QrCodeRevokeView, PrivchatFfiError> {
        let resp: QRCodeRevokeResponse = rpc_call_typed(
            &self.inner,
            routes::qrcode::REVOKE,
            &QRCodeRevokeRequest { qr_key, user_id: 0 },
        )
        .await?;
        Ok(QrCodeRevokeView {
            success: resp.success,
            qr_key: resp.qr_key,
            revoked_at: resp.revoked_at,
        })
    }

    // QR_CODE_SPEC v1.3：legacy `user_qrcode_generate` / 重复的 `user_qrcode_refresh`
    // 已删除——`user/qrcode/generate` server 已停止注册；`refresh` 的 v1.3 版本
    // 见上方（返回 UserQrCodeRefreshView, 含 user_id 而非 revoked_at）。

    pub async fn message_unread_count_remote(
        &self,
        channel_id: u64,
    ) -> Result<MessageUnreadCountView, PrivchatFfiError> {
        let resp: MessageStatusCountResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::COUNT,
            &MessageStatusCountRequest {
                channel_id: Some(channel_id),
            },
        )
        .await?;
        Ok(MessageUnreadCountView {
            unread_count: resp.unread_count,
            channel_id: resp.channel_id,
        })
    }

    pub async fn group_add_members_remote(
        &self,
        group_id: u64,
        user_ids: Vec<u64>,
    ) -> Result<bool, PrivchatFfiError> {
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
                    version: now.max(0),
                    updated_at: now,
                })
                .await;
        }
        Ok(last_resp.unwrap_or(true))
    }

    pub async fn group_remove_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
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
        Ok(resp)
    }

    pub async fn remove_group_member(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        self.group_remove_member_remote(group_id, user_id).await
    }

    pub async fn group_leave_remote(&self, group_id: u64) -> Result<bool, PrivchatFfiError> {
        let resp: GroupMemberLeaveResponse = rpc_call_typed(
            &self.inner,
            routes::group_member::LEAVE,
            &GroupMemberLeaveRequest {
                group_id,
                user_id: 0,
            },
        )
        .await?;
        if let Ok(Some(current_uid)) = self.user_id().await {
            let _ = self.inner.delete_group_member(group_id, current_uid).await;
        }
        Ok(resp)
    }

    pub async fn leave_group(&self, group_id: u64) -> Result<bool, PrivchatFfiError> {
        self.group_leave_remote(group_id).await
    }

    pub async fn invite_to_group(
        &self,
        group_id: u64,
        member_ids: Vec<u64>,
    ) -> Result<bool, PrivchatFfiError> {
        self.group_add_members_remote(group_id, member_ids).await
    }

    pub async fn group_mute_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
        duration_seconds: Option<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
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
                version: now.max(0),
                updated_at: now,
            })
            .await;
        Ok(resp)
    }

    pub async fn group_unmute_member_remote(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<bool, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
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
                version: now.max(0),
                updated_at: now,
            })
            .await;
        Ok(resp)
    }

    pub async fn group_transfer_owner_remote(
        &self,
        group_id: u64,
        target_user_id: u64,
    ) -> Result<GroupTransferOwnerView, PrivchatFfiError> {
        let current_owner_id = self.require_current_user_id().await?;
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
                member_count: None,
                created_at: existing.as_ref().map(|g| g.created_at).unwrap_or(now),
                version: now.max(0),
                updated_at: now,
            })
            .await;
        Ok(GroupTransferOwnerView {
            group_id: resp.group_id,
            new_owner_id: resp.new_owner_id,
            transferred_at: resp.transferred_at,
        })
    }

    pub async fn group_set_role_remote(
        &self,
        group_id: u64,
        user_id: u64,
        role: String,
    ) -> Result<GroupRoleSetView, PrivchatFfiError> {
        let role_code = parse_group_role_to_code(&role);
        let operator_id = self.require_current_user_id().await?;
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
                version: now.max(0),
                updated_at: now,
            })
            .await;
        Ok(GroupRoleSetView {
            group_id: resp.group_id,
            user_id: resp.user_id,
            role: resp.role,
            updated_at: resp.updated_at,
        })
    }

    pub async fn group_get_settings_remote(
        &self,
        group_id: u64,
    ) -> Result<GroupSettingsView, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        let resp: GroupSettingsGetResponse = rpc_call_typed(
            &self.inner,
            routes::group_settings::GET,
            &GroupSettingsGetRequest { group_id, user_id },
        )
        .await?;
        let _ = self
            .inner
            .cache_group_settings_json(group_id, json_encode(&resp, "group_get_settings cache")?)
            .await;
        Ok(GroupSettingsView {
            group_id: resp.group_id,
            join_need_approval: resp.settings.join_need_approval,
            member_can_invite: resp.settings.member_can_invite,
            allow_member_add_friend: resp.settings.allow_member_add_friend,
            allow_search: resp.settings.allow_search,
            join_policy: resp.settings.join_policy,
            all_muted: resp.settings.all_muted,
            max_members: resp.settings.max_members as u64,
            announcement: resp.settings.announcement,
            description: resp.settings.description,
            created_at: resp.settings.created_at,
            updated_at: resp.settings.updated_at,
        })
    }

    pub async fn group_update_settings_remote(
        &self,
        payload: GroupSettingsUpdateInput,
    ) -> Result<bool, PrivchatFfiError> {
        let mut req = GroupSettingsUpdateRequest {
            group_id: payload.group_id,
            operator_id: 0,
            settings: privchat_protocol::rpc::GroupSettingsPatch {
                join_need_approval: payload.join_need_approval,
                member_can_invite: payload.member_can_invite,
                allow_member_add_friend: payload.allow_member_add_friend,
                allow_search: payload.allow_search,
                join_policy: payload.join_policy,
                all_muted: payload.all_muted,
                max_members: payload.max_members,
                announcement: payload.announcement,
                description: payload.description,
            },
        };
        req.operator_id = self.require_current_user_id().await?;
        let resp: GroupSettingsUpdateResponse =
            rpc_call_typed(&self.inner, routes::group_settings::UPDATE, &req).await?;
        if req.group_id > 0 {
            let settings_json = json_encode(&req, "group_update_settings cache")?;
            let _ = self
                .inner
                .cache_group_settings_json(req.group_id, settings_json)
                .await;
        }
        Ok(resp.success)
    }

    pub async fn group_mute_all_remote(
        &self,
        group_id: u64,
        enabled: bool,
    ) -> Result<GroupMuteAllView, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
        let resp: privchat_protocol::rpc::GroupMuteAllResponse = rpc_call_typed(
            &self.inner,
            routes::group_settings::MUTE_ALL,
            &GroupMuteAllRequest {
                group_id,
                operator_id,
                muted: enabled,
            },
        )
        .await?;
        let _ = self
            .inner
            .update_group_mute_all_cache(group_id, enabled)
            .await;
        Ok(GroupMuteAllView {
            success: resp.success,
            group_id: resp.group_id,
            all_muted: resp.all_muted,
            message: resp.message,
            operator_id: resp.operator_id,
            updated_at: resp.updated_at,
        })
    }

    /// 群消息置顶 / 取消置顶（仅群主/管理员；`pinned=false` 为取消置顶）。
    /// `channel_id` 为消息所在通信频道（群聊场景等于 group_id），服务端会三方校验一致性。
    pub async fn group_pin_message_remote(
        &self,
        group_id: u64,
        channel_id: u64,
        message_id: u64,
        pinned: bool,
    ) -> Result<GroupPinMessageView, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
        let resp: privchat_protocol::rpc::message::pin::MessagePinResponse = rpc_call_typed(
            &self.inner,
            routes::message::PIN,
            &privchat_protocol::rpc::message::pin::MessagePinRequest {
                group_id,
                channel_id,
                message_id,
                pinned,
                operator_id,
            },
        )
        .await?;
        Ok(GroupPinMessageView {
            success: resp.success,
            group_id: resp.group_id,
            message_id: resp.message_id,
            pinned: resp.pinned,
            pinned_at: resp.pinned_at,
            pinned_by: resp.pinned_by,
        })
    }

    /// 获取群置顶消息列表（群成员可读，按置顶时间倒序）。
    pub async fn group_pinned_messages_remote(
        &self,
        group_id: u64,
    ) -> Result<Vec<GroupPinnedMessageView>, PrivchatFfiError> {
        let user_id = self.require_current_user_id().await?;
        let resp: privchat_protocol::rpc::message::pin::MessagePinListResponse = rpc_call_typed(
            &self.inner,
            routes::message::PIN_LIST,
            &privchat_protocol::rpc::message::pin::MessagePinListRequest { group_id, user_id },
        )
        .await?;
        Ok(resp
            .items
            .into_iter()
            .map(|it| GroupPinnedMessageView {
                message_id: it.message_id,
                channel_id: it.channel_id,
                pinned_by: it.pinned_by,
                pinned_at: it.pinned_at,
            })
            .collect())
    }

    pub async fn group_approval_list_remote(
        &self,
        group_id: u64,
        page: Option<u32>,
        page_size: Option<u32>,
    ) -> Result<GroupApprovalListView, PrivchatFfiError> {
        let _ = page;
        let _ = page_size;
        let operator_id = self.require_current_user_id().await?;
        let resp: GroupApprovalListResponse = rpc_call_typed(
            &self.inner,
            routes::group_approval::LIST,
            &GroupApprovalListRequest {
                group_id,
                operator_id,
            },
        )
        .await?;
        Ok(GroupApprovalListView {
            group_id: resp.group_id,
            approvals: resp
                .requests
                .into_iter()
                .map(|it| GroupApprovalItemView {
                    request_id: it.request_id,
                    user_id: it.user_id,
                    inviter_id: it.method.member_invite.map(|m| m.inviter_id),
                    qr_code_id: it.method.qr_code.map(|q| q.qr_code_id),
                    message: it.message,
                    created_at: it.created_at,
                    expires_at: it.expires_at,
                })
                .collect(),
            total: resp.total as u64,
        })
    }

    pub async fn group_approval_handle_remote(
        &self,
        request_id: String,
        approved: bool,
        reason: Option<String>,
    ) -> Result<bool, PrivchatFfiError> {
        let operator_id = self.require_current_user_id().await?;
        let resp: GroupApprovalHandleResponse = rpc_call_typed(
            &self.inner,
            routes::group_approval::HANDLE,
            &GroupApprovalHandleRequest {
                request_id,
                operator_id,
                action: if approved {
                    "approve".to_string()
                } else {
                    "reject".to_string()
                },
                reject_reason: reason,
            },
        )
        .await?;
        Ok(resp.success)
    }

    /// QR_CODE_SPEC v1.3 — `group/qrcode/get`：读群当前永久二维码。
    /// Member 及以上可见（server 鉴权）。
    pub async fn group_qrcode_get_remote(
        &self,
        group_id: u64,
    ) -> Result<GroupQrCodeGetView, PrivchatFfiError> {
        let resp: GroupQRCodeGetResponse = rpc_call_typed(
            &self.inner,
            routes::group_qrcode::GET,
            &GroupQRCodeGetRequest {
                group_id,
                operator_id: 0,
            },
        )
        .await?;
        Ok(GroupQrCodeGetView {
            qr_key: resp.qr_key,
            qr_code: resp.qr_code,
            group_id: resp.group_id,
        })
    }

    /// QR_CODE_SPEC v1.3 — `group/qrcode/refresh`：旋转群二维码。
    /// Owner/Admin only（server 鉴权）。
    pub async fn group_qrcode_refresh_remote(
        &self,
        group_id: u64,
    ) -> Result<GroupQrCodeRefreshView, PrivchatFfiError> {
        let resp: GroupQRCodeRefreshResponse = rpc_call_typed(
            &self.inner,
            routes::group_qrcode::REFRESH,
            &GroupQRCodeRefreshRequest {
                group_id,
                operator_id: 0,
            },
        )
        .await?;
        Ok(GroupQrCodeRefreshView {
            old_qr_key: resp.old_qr_key,
            new_qr_key: resp.new_qr_key,
            qr_code: resp.qr_code,
            group_id: resp.group_id,
        })
    }

    /// QR_CODE_SPEC v1.3 — `group/join/qrcode`：扫码加群。
    /// Server 用 `qr_key` 反查 `group_id` 后走与邀请相同的 join_need_approval 流程。
    /// v1.3 删除了 token 参数（UNIQUE qr_key 本身就是不可枚举凭证）。
    pub async fn group_qrcode_join_remote(
        &self,
        qr_key: String,
        message: Option<String>,
    ) -> Result<GroupQrCodeJoinResult, PrivchatFfiError> {
        let resp: GroupQRCodeJoinResponse = rpc_call_typed(
            &self.inner,
            routes::group_qrcode::JOIN,
            &GroupQRCodeJoinRequest {
                qr_key,
                message,
                user_id: 0,
            },
        )
        .await?;
        Ok(GroupQrCodeJoinResult {
            status: resp.status,
            group_id: resp.group_id,
            request_id: resp.request_id,
            message: resp.message,
            user_id: resp.user_id,
            joined_at: resp.joined_at,
        })
    }

    pub async fn channel_broadcast_create_remote(
        &self,
        payload: ChannelBroadcastCreateInput,
    ) -> Result<ChannelBroadcastCreateView, PrivchatFfiError> {
        let req = ChannelBroadcastCreateRequest {
            name: payload.name,
            description: payload.description,
            avatar_url: payload.avatar_url,
        };
        let resp: ChannelBroadcastCreateResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::CREATE, &req).await?;
        Ok(ChannelBroadcastCreateView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn channel_broadcast_subscribe_remote(
        &self,
        payload: ChannelBroadcastSubscribeInput,
    ) -> Result<bool, PrivchatFfiError> {
        let req = ChannelBroadcastSubscribeRequest {
            user_id: 0,
            channel_id: payload.channel_id,
        };
        let resp: ChannelBroadcastSubscribeResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::SUBSCRIBE, &req).await?;
        Ok(resp.status.eq_ignore_ascii_case("success"))
    }

    pub async fn channel_broadcast_list_remote(
        &self,
        payload: ChannelBroadcastListInput,
    ) -> Result<ChannelBroadcastListView, PrivchatFfiError> {
        let req = ChannelBroadcastListRequest {
            page: payload.page,
            page_size: payload.page_size,
        };
        let resp: ChannelBroadcastListResponse =
            rpc_call_typed(&self.inner, routes::channel_broadcast::LIST, &req).await?;
        Ok(ChannelBroadcastListView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn channel_content_publish_remote(
        &self,
        payload: ChannelContentPublishInput,
    ) -> Result<ChannelContentPublishView, PrivchatFfiError> {
        let req = ChannelContentPublishRequest {
            channel_id: payload.channel_id,
            content: payload.content,
            title: payload.title,
            content_type: payload.content_type,
        };
        let resp: ChannelContentPublishResponse =
            rpc_call_typed(&self.inner, routes::channel_content::PUBLISH, &req).await?;
        Ok(ChannelContentPublishView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn channel_content_list_remote(
        &self,
        payload: ChannelContentListInput,
    ) -> Result<ChannelContentListView, PrivchatFfiError> {
        let req = ChannelContentListRequest {
            channel_id: payload.channel_id,
            page: payload.page,
            page_size: payload.page_size,
        };
        let resp: ChannelContentListResponse =
            rpc_call_typed(&self.inner, routes::channel_content::LIST, &req).await?;
        Ok(ChannelContentListView {
            status: resp.status,
            action: resp.action,
            timestamp: resp.timestamp,
        })
    }

    pub async fn sticker_package_list_remote(
        &self,
        payload: StickerPackageListInput,
    ) -> Result<StickerPackageListView, PrivchatFfiError> {
        let _ = payload;
        let req = StickerPackageListRequest {};
        let resp: StickerPackageListResponse =
            rpc_call_typed(&self.inner, routes::sticker::PACKAGE_LIST, &req).await?;
        Ok(StickerPackageListView {
            packages: resp
                .packages
                .into_iter()
                .map(map_sticker_package_info)
                .collect(),
        })
    }

    pub async fn sticker_package_detail_remote(
        &self,
        payload: StickerPackageDetailInput,
    ) -> Result<StickerPackageDetailView, PrivchatFfiError> {
        let req = StickerPackageDetailRequest {
            package_id: payload.package_id,
        };
        let resp: StickerPackageDetailResponse =
            rpc_call_typed(&self.inner, routes::sticker::PACKAGE_DETAIL, &req).await?;
        Ok(StickerPackageDetailView {
            package: map_sticker_package_info(resp.package),
        })
    }

    pub async fn sync_submit_remote(
        &self,
        payload: SyncSubmitInput,
    ) -> Result<SyncSubmitView, PrivchatFfiError> {
        let mut payload_obj = serde_json::Map::new();
        for entry in payload.payload_entries {
            payload_obj.insert(entry.key, serde_json::Value::String(entry.value));
        }
        let req = ClientSubmitRequest {
            local_message_id: payload.local_message_id,
            channel_id: payload.channel_id,
            channel_type: payload.channel_type,
            last_pts: payload.last_pts,
            command_type: payload.command_type,
            payload: serde_json::Value::Object(payload_obj),
            client_timestamp: payload.client_timestamp,
            device_id: payload.device_id,
        };
        let resp: ClientSubmitResponse =
            rpc_call_typed(&self.inner, routes::sync::SUBMIT, &req).await?;
        let (decision, decision_reason) = match resp.decision {
            privchat_protocol::rpc::ServerDecision::Accepted => ("accepted".to_string(), None),
            privchat_protocol::rpc::ServerDecision::Transformed { reason } => {
                ("transformed".to_string(), Some(reason))
            }
            privchat_protocol::rpc::ServerDecision::Rejected { reason } => {
                ("rejected".to_string(), Some(reason))
            }
        };
        Ok(SyncSubmitView {
            decision,
            decision_reason,
            pts: resp.pts,
            server_msg_id: resp.server_msg_id,
            server_timestamp: resp.server_timestamp,
            local_message_id: resp.local_message_id,
            has_gap: resp.has_gap,
            current_pts: resp.current_pts,
        })
    }

    pub async fn entity_sync_remote(
        &self,
        payload: SyncEntitiesInput,
    ) -> Result<SyncEntitiesView, PrivchatFfiError> {
        let req = SyncEntitiesRequest {
            entity_type: payload.entity_type,
            since_version: payload.since_version,
            scope: payload.scope,
            limit: payload.limit,
        };
        let resp: SyncEntitiesResponse =
            rpc_call_typed(&self.inner, routes::entity::SYNC_ENTITIES, &req).await?;
        Ok(SyncEntitiesView {
            items: resp
                .items
                .into_iter()
                .map(|it| SyncEntityItemView {
                    entity_id: it.entity_id,
                    version: it.version,
                    deleted: it.deleted,
                    payload_json: it.payload.map(|v| v.to_string()),
                })
                .collect(),
            next_version: resp.next_version,
            has_more: resp.has_more,
            min_version: resp.min_version,
        })
    }

    pub async fn sync_get_difference_remote(
        &self,
        payload: GetDifferenceInput,
    ) -> Result<GetDifferenceView, PrivchatFfiError> {
        let req = GetDifferenceRequest {
            channel_id: payload.channel_id,
            channel_type: payload.channel_type,
            last_pts: payload.last_pts,
            limit: payload.limit,
        };
        let resp: GetDifferenceResponse =
            rpc_call_typed(&self.inner, routes::sync::GET_DIFFERENCE, &req).await?;
        Ok(GetDifferenceView {
            commits: resp
                .commits
                .into_iter()
                .map(|c| GetDifferenceCommitView {
                    pts: c.pts,
                    server_msg_id: c.server_msg_id,
                    local_message_id: c.local_message_id,
                    channel_id: c.channel_id,
                    channel_type: c.channel_type,
                    message_type: c.message_type,
                    content_json: c.content.to_string(),
                    server_timestamp: c.server_timestamp,
                    sender_id: c.sender_id,
                    sender_info_json: c
                        .sender_info
                        .map(|v| serde_json::to_string(&v).unwrap_or_default()),
                })
                .collect(),
            current_pts: resp.current_pts,
            has_more: resp.has_more,
        })
    }

    pub async fn sync_get_channel_pts_remote(
        &self,
        payload: GetChannelPtsInput,
    ) -> Result<ChannelPtsView, PrivchatFfiError> {
        let req = GetChannelPtsRequest {
            channel_id: payload.channel_id,
            channel_type: payload.channel_type,
        };
        let resp: GetChannelPtsResponse =
            rpc_call_typed(&self.inner, routes::sync::GET_CHANNEL_PTS, &req).await?;
        Ok(ChannelPtsView {
            channel_id: payload.channel_id,
            channel_type: payload.channel_type,
            current_pts: resp.current_pts,
        })
    }

    pub async fn sync_batch_get_channel_pts_remote(
        &self,
        payload: BatchGetChannelPtsInput,
    ) -> Result<BatchGetChannelPtsView, PrivchatFfiError> {
        let req = BatchGetChannelPtsRequest {
            channels: payload
                .channels
                .into_iter()
                .map(|c| privchat_protocol::rpc::ChannelIdentifier {
                    channel_id: c.channel_id,
                    channel_type: c.channel_type,
                })
                .collect(),
        };
        let resp: BatchGetChannelPtsResponse =
            rpc_call_typed(&self.inner, routes::sync::BATCH_GET_CHANNEL_PTS, &req).await?;
        Ok(BatchGetChannelPtsView {
            channels: resp
                .channel_pts_map
                .into_iter()
                .map(|c| ChannelPtsView {
                    channel_id: c.channel_id,
                    channel_type: c.channel_type,
                    current_pts: c.current_pts,
                })
                .collect(),
        })
    }

    pub async fn file_request_upload_token_remote(
        &self,
        payload: FileRequestUploadTokenInput,
    ) -> Result<FileRequestUploadTokenView, PrivchatFfiError> {
        let mut req = FileRequestUploadTokenRequest {
            user_id: 0,
            filename: payload.filename,
            file_size: payload.file_size,
            mime_type: payload.mime_type,
            file_type: payload.file_type,
            business_type: payload.business_type,
        };
        req.user_id = 0;
        let resp: FileRequestUploadTokenResponse =
            rpc_call_typed(&self.inner, routes::file::REQUEST_UPLOAD_TOKEN, &req).await?;
        Ok(FileRequestUploadTokenView {
            token: resp.token,
            upload_url: resp.upload_url,
            file_id: resp.file_id,
        })
    }

    pub async fn file_upload_callback_remote(
        &self,
        payload: FileUploadCallbackInput,
    ) -> Result<bool, PrivchatFfiError> {
        let mut req = FileUploadCallbackRequest {
            file_id: payload.file_id,
            user_id: 0,
            status: payload.status,
        };
        req.user_id = 0;
        let resp: FileUploadCallbackResponse =
            rpc_call_typed(&self.inner, routes::file::UPLOAD_CALLBACK, &req).await?;
        Ok(resp)
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

    pub fn generate_local_message_id(&self) -> Result<u64, PrivchatFfiError> {
        self.inner
            .generate_local_message_id()
            .map_err(PrivchatFfiError::from)
    }

    pub async fn enqueue_text(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
    ) -> Result<u64, PrivchatFfiError> {
        self.enqueue_text_with_local_id(channel_id, channel_type, from_uid, content, None)
            .await
    }

    pub async fn enqueue_text_with_local_id(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
        local_message_id: Option<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        let input = map_new_message(NewMessage {
            channel_id,
            channel_type,
            from_uid,
            message_type: 0,
            content,
            searchable_word: String::new(),
            setting: 0,
            extra: String::new(),
            mime_type: None,
            media_downloaded: false,
            thumb_status: 0,
        });
        let message_id = self
            .inner
            .create_local_message_with_id(input, local_message_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        self.enqueue_outbound_message(message_id, Vec::new())
            .await?;
        Ok(message_id)
    }

    async fn enqueue_local_message(&self, input: NewMessage) -> Result<u64, PrivchatFfiError> {
        let message_id = self.create_local_message(input).await?;
        self.enqueue_outbound_message(message_id, Vec::new())
            .await?;
        Ok(message_id)
    }

    pub async fn send_message(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
    ) -> Result<u64, PrivchatFfiError> {
        self.enqueue_local_message(NewMessage {
            channel_id,
            channel_type,
            from_uid,
            message_type: 0,
            content,
            searchable_word: String::new(),
            setting: 0,
            extra: String::new(),
            mime_type: None,
            media_downloaded: false,
            thumb_status: 0,
        })
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

    pub async fn send_message_with_input(
        &self,
        input: NewMessage,
    ) -> Result<u64, PrivchatFfiError> {
        self.enqueue_local_message(input).await
    }

    // Backward-compat symbol for older generated bindings.
    // Semantics stay queue-first: create local message and enqueue it.
    pub async fn send_local_message_now(&self, input: NewMessage) -> Result<u64, PrivchatFfiError> {
        self.send_message_with_input(input).await
    }

    pub async fn send_message_with_options(
        &self,
        mut input: NewMessage,
        options: SendMessageOptionsInput,
    ) -> Result<u64, PrivchatFfiError> {
        let has_reply = options.in_reply_to_message_id.is_some();
        let has_mentions = !options.mentioned_user_ids.is_empty();
        if has_reply || has_mentions {
            // SQLite still stores the legacy envelope, but its shape is built
            // from a protocol type here rather than JSON assembled by callers.
            let envelope = privchat_protocol::message::LocalMessagePayloadEnvelope {
                content: input.content.clone(),
                metadata: None,
                reply_to_message_id: options.in_reply_to_message_id.map(|id| id.to_string()),
                mentioned_user_ids: has_mentions.then_some(options.mentioned_user_ids),
                message_source: None,
            };
            input.content = serde_json::to_string(&envelope).map_err(|e| {
                PrivchatFfiError::from(SdkError::Serialization(format!(
                    "encode local message envelope: {e}"
                )))
            })?;
        }
        if let Some(extra) = options.extra_json.filter(|value| !value.trim().is_empty()) {
            // `extra_json` remains an explicitly opaque application extension;
            // standard send options above are fully typed.
            input.extra = extra;
        }
        let _ = options.silent;
        self.send_message_with_input(input).await
    }

    pub async fn create_local_attachment_placeholder_typed(
        &self,
        mut input: NewMessage,
        local_message_id: u64,
        metadata: LocalAttachmentMetadataInput,
    ) -> Result<u64, PrivchatFfiError> {
        let extension_json = metadata.extension_json.clone();
        let metadata = privchat_protocol::message::LocalAttachmentMetadata {
            file_name: metadata.file_name,
            mime_type: metadata.mime_type,
            duration: metadata.duration,
            width: metadata.width,
            height: metadata.height,
            thumbnail_width: metadata.thumbnail_width,
            thumbnail_height: metadata.thumbnail_height,
        };
        let mut encoded = serde_json::to_value(&metadata).map_err(|e| {
            PrivchatFfiError::from(SdkError::Serialization(format!(
                "encode local attachment metadata: {e}"
            )))
        })?;
        if let (Some(target), Some(extension)) = (
            encoded.as_object_mut(),
            metadata_input_extension(extension_json.as_deref())?,
        ) {
            for (key, value) in extension {
                target.entry(key).or_insert(value);
            }
        }
        input.extra = serde_json::to_string(&encoded).map_err(|e| {
            PrivchatFfiError::from(SdkError::Serialization(format!(
                "serialize local attachment metadata: {e}"
            )))
        })?;
        self.create_local_attachment_placeholder(input, Some(local_message_id))
            .await
    }

    pub async fn send_link_message(
        &self,
        input: LinkMessageInput,
    ) -> Result<u64, PrivchatFfiError> {
        self.inner
            .send_link_message(map_link_message_input(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn send_location_message(
        &self,
        input: LocationMessageInput,
    ) -> Result<u64, PrivchatFfiError> {
        self.inner
            .send_location_message(map_location_message_input(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn send_contact_card_message(
        &self,
        input: ContactCardMessageInput,
    ) -> Result<u64, PrivchatFfiError> {
        self.inner
            .send_contact_card_message(map_contact_card_message_input(input))
            .await
            .map_err(PrivchatFfiError::from)
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

    pub async fn create_local_message_with_id(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        self.inner
            .create_local_message_with_id(map_new_message(input), local_message_id)
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

    /// 以 anchor 为轴的本地上下文窗口（显示排序；spec §5 跳转渲染原语）。
    /// 通常先调 get_messages_around 完成服务端回填，再用本方法从本地读窗口渲染。
    pub async fn list_local_messages_around(
        &self,
        channel_id: u64,
        channel_type: i32,
        anchor_server_message_id: u64,
        before_limit: u64,
        after_limit: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        let out = self
            .inner
            .list_messages_around(
                channel_id,
                channel_type,
                anchor_server_message_id,
                before_limit as usize,
                after_limit as usize,
            )
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

    pub async fn get_peer_read_pts(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<u64>, PrivchatFfiError> {
        self.inner
            .get_peer_read_pts(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn mark_message_sent(
        &self,
        message_id: u64,
        server_message_id: u64,
        message_seq: u32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .mark_message_sent(message_id, server_message_id, message_seq)
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

    pub async fn update_thumb_status(
        &self,
        message_id: u64,
        thumb_status: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .update_thumb_status(message_id, thumb_status)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn update_media_downloaded(
        &self,
        message_id: u64,
        downloaded: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .update_media_downloaded(message_id, downloaded)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn finalize_local_attachment(
        &self,
        message_id: u64,
        content: String,
        thumb_status: i32,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .finalize_local_attachment(message_id, content, thumb_status)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn create_local_attachment_placeholder(
        &self,
        input: NewMessage,
        local_message_id: Option<u64>,
    ) -> Result<u64, PrivchatFfiError> {
        self.inner
            .create_local_attachment_placeholder(map_new_message(input), local_message_id)
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
        self.get_channel_unread_count(channel_id, channel_type)
            .await
    }

    pub async fn get_channel_sync_state(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<ChannelSyncState, PrivchatFfiError> {
        let unread = self
            .get_channel_unread_count(channel_id, channel_type)
            .await?;
        Ok(ChannelSyncState {
            channel_id,
            channel_type,
            unread,
        })
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
        self.inner
            .set_channel_notification_mode_pref(channel_id, channel_type, mode)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn channel_notification_mode(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32, PrivchatFfiError> {
        self.inner
            .channel_notification_mode_pref(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn set_channel_favourite(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_channel_favourite_pref(channel_id, channel_type, enabled)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn set_channel_low_priority(
        &self,
        channel_id: u64,
        channel_type: i32,
        enabled: bool,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_channel_low_priority_pref(channel_id, channel_type, enabled)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn channel_tags(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Vec<String>, PrivchatFfiError> {
        self.inner
            .channel_tags_pref(channel_id, channel_type)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn upsert_user(&self, input: UpsertUserInput) -> Result<(), PrivchatFfiError> {
        self.inner
            .upsert_user(map_upsert_user(input))
            .await
            .map_err(PrivchatFfiError::from)
    }

    /// AVATAR_CACHE_SPEC §8: 头像上传前客户端预处理。
    ///
    /// decode（白名单 jpeg/png/webp，gif/损坏格式直接 Err，不消耗上传流量）→
    /// 中心裁剪正方形 → 边长 >480 缩放到 480x480（≤480 不放大）→ 编码 PNG
    /// 写临时文件。返回处理后文件路径，App 选图后先过它再走上传管道。
    pub async fn prepare_avatar_image(&self, src_path: String) -> Result<String, PrivchatFfiError> {
        self.inner
            .prepare_avatar_image(src_path)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn get_user_by_id(
        &self,
        user_id: u64,
    ) -> Result<Option<StoredUser>, PrivchatFfiError> {
        // 1. 优先本地缓存
        if let Some(user) = self
            .inner
            .get_user_by_id(user_id)
            .await
            .map_err(PrivchatFfiError::from)?
        {
            return Ok(Some(map_stored_user(user)));
        }

        // 2. 本地没有 → RPC 拉远程
        let remote: Option<AccountUserDetailResponse> = rpc_call_typed(
            &self.inner,
            routes::account_user::DETAIL,
            &AccountUserDetailRequest {
                target_user_id: user_id,
                source: "user_cache".to_string(),
                source_id: user_id.to_string(),
                user_id: 0,
            },
        )
        .await
        .ok();

        let Some(remote) = remote else {
            return Ok(None);
        };

        // 3. 写回本地 user 表
        let updated_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.inner
            .upsert_user(SdkUpsertUserInput {
                user_id,
                username: Some(remote.username).filter(|s| !s.is_empty()),
                nickname: Some(remote.nickname).filter(|s| !s.is_empty()),
                alias: None,
                avatar: remote.avatar_url.unwrap_or_default(),
                user_type: remote.user_type as i32,
                is_deleted: false,
                channel_id: String::new(),
                version: 0,
                updated_at,
            })
            .await
            .map_err(PrivchatFfiError::from)?;

        // 4. 返回刚写入的数据
        let user = self
            .inner
            .get_user_by_id(user_id)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(user.map(map_stored_user))
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

    /// F-sync.2: 列出好友申请（非 accepted 行）。
    ///
    /// - `outgoing=true`：我发出的（is_outgoing=true）；`outgoing=false`：我收到的。
    /// - `statuses` 留空 = 全要 pending/rejected/recalled/expired；具体传如
    ///   [0] 只看 pending、[0,3] pending+rejected 等。
    pub async fn list_friend_requests(
        &self,
        outgoing: bool,
        statuses: Vec<i16>,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<StoredFriend>, PrivchatFfiError> {
        let out = self
            .inner
            .list_friend_requests(outgoing, statuses, limit as usize, offset as usize)
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_stored_friend).collect())
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

    pub async fn delete_blacklist_entry(
        &self,
        blocked_user_id: u64,
    ) -> Result<(), PrivchatFfiError> {
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

    pub async fn user_storage_paths(&self) -> Result<UserStoragePaths, PrivchatFfiError> {
        let out = self
            .inner
            .user_storage_paths()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(map_storage_paths(out))
    }

    pub async fn list_local_accounts(&self) -> Result<Vec<LocalAccountSummary>, PrivchatFfiError> {
        let out = self
            .inner
            .list_local_accounts()
            .await
            .map_err(PrivchatFfiError::from)?;
        Ok(out.into_iter().map(map_local_account_summary).collect())
    }

    pub async fn set_current_uid(&self, uid: String) -> Result<(), PrivchatFfiError> {
        self.inner
            .set_current_uid(uid)
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn wipe_current_user_full(&self) -> Result<(), PrivchatFfiError> {
        self.inner
            .wipe_current_user_full()
            .await
            .map_err(PrivchatFfiError::from)
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
        "livestreaming".to_string()
    }

    pub fn build(&self) -> String {
        self.builder()
    }

    pub async fn get_presence_stats(&self) -> Result<PresenceStatsView, PrivchatFfiError> {
        let friends = self.list_friends(500, 0).await?;
        let user_ids: Vec<u64> = friends.into_iter().map(|f| f.user_id).collect();
        if user_ids.is_empty() {
            return Ok(PresenceStatsView {
                online: 0,
                offline: 0,
                total: 0,
            });
        }
        let statuses = self.batch_get_presence(user_ids).await?;
        let total = statuses.len() as u64;
        let online = statuses.iter().filter(|s| s.is_online).count() as u64;
        let offline = total.saturating_sub(online);
        Ok(PresenceStatsView {
            online,
            offline,
            total,
        })
    }

    pub async fn get_typing_stats(&self) -> Result<TypingStatsView, PrivchatFfiError> {
        let active = self.typing_active_channels.lock().await;
        let active_count = active.len() as u64;
        let started_count = self.typing_started_count.load(Ordering::Relaxed);
        let stopped_count = self.typing_stopped_count.load(Ordering::Relaxed);
        let active_channels: Vec<TypingChannelView> = active
            .iter()
            .map(|(channel_id, channel_type)| TypingChannelView {
                channel_id: *channel_id,
                channel_type: *channel_type,
            })
            .collect();
        Ok(TypingStatsView {
            typing: active_count,
            active_channels,
            started_count,
            stopped_count,
        })
    }

    pub async fn join_group_by_qrcode(
        &self,
        qr_key: String,
    ) -> Result<GroupQrCodeJoinResult, PrivchatFfiError> {
        self.group_qrcode_join_remote(qr_key, None).await
    }

    pub async fn leave_channel(&self, channel_id: u64) -> Result<bool, PrivchatFfiError> {
        self.hide_channel(channel_id).await
    }

    pub async fn list_my_devices(&self) -> Result<Vec<DeviceInfoView>, PrivchatFfiError> {
        let snapshot = self.session_snapshot().await?;
        let Some(s) = snapshot else {
            return Ok(Vec::new());
        };
        Ok(vec![DeviceInfoView {
            device_id: s.device_id,
            device_name: "current-device".to_string(),
            is_current: true,
            app_in_background: self.app_in_background.load(Ordering::Relaxed),
        }])
    }

    pub async fn mark_fully_read_at(
        &self,
        channel_id: u64,
        read_pts: u64,
    ) -> Result<u64, PrivchatFfiError> {
        self.mark_read_to_pts(channel_id, read_pts).await
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
        let channel_id = self
            .resolve_channel_id_by_server_message_id(server_message_id)
            .await?;
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        Ok(resp
            .readers
            .into_iter()
            .any(|entry| entry.user_id == user_id))
    }

    pub async fn seen_by_for_event(
        &self,
        server_message_id: u64,
    ) -> Result<Vec<SeenByEntry>, PrivchatFfiError> {
        let channel_id = self
            .resolve_channel_id_by_server_message_id(server_message_id)
            .await?;
        let resp: MessageReadListResponse = rpc_call_typed(
            &self.inner,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await?;
        Ok(resp
            .readers
            .into_iter()
            .filter_map(|entry| {
                let user_id = entry.user_id;
                let read_at = entry.read_at;
                Some(SeenByEntry { user_id, read_at })
            })
            .collect())
    }

    pub async fn paginate_back(
        &self,
        channel_id: u64,
        channel_type: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        self.list_messages(channel_id, channel_type, page, page_size)
            .await
    }

    pub async fn paginate_forward(
        &self,
        channel_id: u64,
        channel_type: i32,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<StoredMessage>, PrivchatFfiError> {
        self.list_messages(channel_id, channel_type, page, page_size)
            .await
    }

    pub async fn retry_message(&self, message_id: u64) -> Result<u64, PrivchatFfiError> {
        let msg = self.get_message_by_id(message_id).await?.ok_or_else(|| {
            PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::OperationNotAllowed as u32,
                detail: format!("message not found: {message_id}"),
            }
        })?;
        let payload = json_encode(
            &RetryMessagePayloadView {
                message_id: msg.message_id,
                channel_id: msg.channel_id,
                channel_type: msg.channel_type,
                from_uid: msg.from_uid,
                message_type: msg.message_type,
                content: msg.content,
                extra: msg.extra,
            },
            "retry_message payload",
        )?
        .into_bytes();
        self.enqueue_outbound_message(message_id, payload).await
    }

    pub async fn search_channel(
        &self,
        keyword: String,
    ) -> Result<Vec<StoredChannel>, PrivchatFfiError> {
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
        let items = self
            .list_messages(channel_id, channel_type, 1, 1000)
            .await?;
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
        self.enqueue_outbound_file(message_id, route_key, payload)
            .await
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
        self.enqueue_outbound_file(message_id, route_key, payload)
            .await
    }

    pub async fn download_attachment_to_path(
        &self,
        source_path: String,
        target_path: String,
    ) -> Result<String, PrivchatFfiError> {
        let data = self.resolve_attachment_bytes(&source_path).await?;
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

    /// 获取附件下载目标目录 (Canonical 路径)
    /// 参数必须传入 message 表的主键和创建时间，禁止使用业务脏字段
    pub fn get_attachment_target_dir(
        &self,
        uid: u64,
        message_id: i64,
        created_at_ms: i64,
    ) -> Result<String, PrivchatFfiError> {
        let data_dir = self.config.lock().unwrap().data_dir.clone();
        let root = std::path::Path::new(&data_dir);
        privchat_sdk::media_store::ensure_attachment_dir(root, uid, message_id, created_at_ms)
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("ensure attachment dir failed: {e}"),
            })
    }

    /// Start a streaming Telegram-style download for a message's primary attachment.
    /// Delegates to [`PrivchatSdk::start_message_media_download`] — the core SDK owns
    /// the state machine, so the Rust iced UI and the FFI Kotlin/iOS layer share it.
    pub async fn start_message_media_download(
        &self,
        message_id: u64,
        download_url: String,
        mime: String,
        filename_hint: Option<String>,
        created_at_ms: i64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .start_message_media_download(
                message_id,
                download_url,
                mime,
                filename_hint,
                created_at_ms,
            )
            .await
            .map_err(PrivchatFfiError::from)
    }

    /// Start a streaming download for an attachment-encrypted (v1) message by
    /// `file_id`. The SDK resolves the signed URL + cek via `file/get_url` and
    /// decrypts the blob on completion. Prefer this over the URL-driven entry for
    /// any message that carries a `file_id`; the legacy URL form is retained only
    /// for plaintext (pre-encryption) attachments.
    pub async fn start_message_media_download_by_file_id(
        &self,
        message_id: u64,
        file_id: u64,
        mime: String,
        filename_hint: Option<String>,
        created_at_ms: i64,
    ) -> Result<(), PrivchatFfiError> {
        self.inner
            .start_message_media_download_by_file_id(
                message_id,
                file_id,
                mime,
                filename_hint,
                created_at_ms,
            )
            .await
            .map_err(PrivchatFfiError::from)
    }

    pub async fn pause_message_media_download(&self, message_id: u64) {
        self.inner.pause_message_media_download(message_id).await;
    }

    pub async fn resume_message_media_download(&self, message_id: u64) {
        self.inner.resume_message_media_download(message_id).await;
    }

    pub async fn cancel_message_media_download(&self, message_id: u64) {
        self.inner.cancel_message_media_download(message_id).await;
    }

    pub async fn get_media_download_state(&self, message_id: u64) -> MediaDownloadState {
        let state = self.inner.get_media_download_state(message_id).await;
        map_media_download_state(state)
    }

    /// 解析本地已存在的附件路径 (含 Legacy 兼容)
    pub fn resolve_attachment_path(
        &self,
        uid: u64,
        message_id: i64,
        created_at_ms: i64,
        filename: Option<String>,
    ) -> Option<String> {
        let data_dir = self.config.lock().unwrap().data_dir.clone();
        let root = std::path::Path::new(&data_dir);
        privchat_sdk::media_store::resolve_attachment_path(
            root,
            uid,
            message_id,
            created_at_ms,
            filename.as_deref(),
        )
        .map(|p| p.to_string_lossy().to_string())
    }

    /// 查找本地缩略图路径：{user_root}/files/{yyyymm}/{message_id}/thumb.webp
    pub fn resolve_thumbnail_path(
        &self,
        uid: u64,
        message_id: i64,
        created_at_ms: i64,
    ) -> Option<String> {
        let data_dir = self.config.lock().unwrap().data_dir.clone();
        let root = std::path::Path::new(&data_dir);
        let dir = privchat_sdk::media_store::get_canonical_message_dir(
            root,
            uid,
            message_id,
            created_at_ms,
        );
        for name in &[
            privchat_sdk::media_store::THUMB_FILENAME,
            privchat_sdk::media_store::THUMB_PNG_FILENAME,
        ] {
            let thumb = dir.join(name);
            if thumb.exists() {
                return Some(thumb.to_string_lossy().to_string());
            }
        }
        // Legacy: check for {id}_thumb.webp or {id}_thumb.jpg in same dir
        for ext in &["webp", "jpg"] {
            let legacy = dir.join(format!("{}_thumb.{}", message_id, ext));
            if legacy.exists() {
                return Some(legacy.to_string_lossy().to_string());
            }
        }
        // Also try legacy dir without yyyymm
        let legacy_dir = root
            .join("users")
            .join(uid.to_string())
            .join("files")
            .join(message_id.to_string());
        if legacy_dir != dir && legacy_dir.exists() {
            for name in &[
                privchat_sdk::media_store::THUMB_FILENAME,
                privchat_sdk::media_store::THUMB_PNG_FILENAME,
            ] {
                let thumb = legacy_dir.join(name);
                if thumb.exists() {
                    return Some(thumb.to_string_lossy().to_string());
                }
            }
        }
        None
    }

    pub async fn set_video_process_hook(
        &self,
        hook: Option<Box<dyn VideoProcessHook>>,
    ) -> Result<(), PrivchatFfiError> {
        let hook_registered = hook.is_some();
        let sdk_hook: Option<SdkVideoProcessHook> = hook.map(|h| {
            let h: Arc<dyn VideoProcessHook> = Arc::from(h);
            Arc::new(
                move |op: SdkMediaProcessOp,
                      source_path: &std::path::Path,
                      meta_path: &std::path::Path,
                      output_path: &std::path::Path| {
                    let op_ffi = match op {
                        SdkMediaProcessOp::Thumbnail => MediaProcessOp::Thumbnail,
                        SdkMediaProcessOp::Compress => MediaProcessOp::Compress,
                    };
                    h.process(
                        op_ffi,
                        source_path.to_string_lossy().to_string(),
                        meta_path.to_string_lossy().to_string(),
                        output_path.to_string_lossy().to_string(),
                    )
                    .map_err(|e| format!("{e}"))
                },
            ) as SdkVideoProcessHook
        });
        self.inner
            .set_video_process_hook(sdk_hook)
            .await
            .map_err(PrivchatFfiError::from)?;
        self.video_process_hook_registered
            .store(hook_registered, Ordering::Relaxed);
        Ok(())
    }

    pub async fn remove_video_process_hook(&self) -> Result<(), PrivchatFfiError> {
        self.set_video_process_hook(None).await
    }

    /// Plan 2：宿主处理完 `SdkEvent::MediaJobRequested` 后回传结果。
    pub fn submit_media_job_result(
        &self,
        job_id: String,
        result: MediaJobResult,
    ) -> Result<(), PrivchatFfiError> {
        let inner = privchat_sdk::MediaJobResult {
            ok: result.ok,
            output_path: result.output_path,
            error: result.error,
        };
        self.inner
            .submit_media_job_result(job_id, inner)
            .map_err(PrivchatFfiError::from)
    }

    async fn resolve_attachment_bytes(
        &self,
        source_path: &str,
    ) -> Result<Vec<u8>, PrivchatFfiError> {
        let source = source_path.trim();
        if source.is_empty() {
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InvalidParams as u32,
                detail: "attachment source is empty".to_string(),
            });
        }

        let path = std::path::Path::new(source);
        if path.exists() {
            return std::fs::read(path).map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InternalError as u32,
                detail: format!("read local attachment failed: {e}"),
            });
        }

        let download_url = if source.starts_with("http://") || source.starts_with("https://") {
            source.to_string()
        } else if let Ok(file_id) = source.parse::<u64>() {
            let req = FileGetUrlRequest {
                file_id,
                user_id: 0,
            };
            let resp: FileGetUrlResponse =
                rpc_call_typed(&self.inner, routes::file::GET_URL, &req).await?;
            resp.file_url
        } else {
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::InvalidParams as u32,
                detail: format!("invalid attachment source: {source}"),
            });
        };

        let response = reqwest::Client::new()
            .get(&download_url)
            .send()
            .await
            .map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::NetworkError as u32,
                detail: format!("download attachment request failed: {e}"),
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::NetworkError as u32,
                detail: format!("download attachment failed: status={status} body={body}"),
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| PrivchatFfiError::SdkError {
                code: privchat_protocol::ErrorCode::NetworkError as u32,
                detail: format!("read attachment bytes failed: {e}"),
            })?;
        Ok(bytes.to_vec())
    }

    pub fn to_client_endpoint(&self) -> Option<String> {
        self.config().endpoints.first().map(|v| {
            let scheme = match v.protocol {
                TransportProtocol::Quic => "quic",
                TransportProtocol::Tcp => "tcp",
                TransportProtocol::WebSocket => {
                    if v.use_tls {
                        "wss"
                    } else {
                        "ws"
                    }
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

#[uniffi::export]
pub fn git_sha() -> String {
    option_env!("GIT_SHA")
        .or(option_env!("VERGEN_GIT_SHA"))
        .unwrap_or("unknown")
        .to_string()
}

#[uniffi::export]
pub fn build_time() -> String {
    option_env!("FFI_BUILD_TIMESTAMP")
        .or(option_env!("BUILD_TIME"))
        .unwrap_or("unknown")
        .to_string()
}

// ───────────────────── R8.6b-rust QR decoder ─────────────────────
//
// Implementation lives in `crate::qr`. The `#[uniffi::export]` /
// `#[derive(uniffi::Error)]` annotations are kept at crate root because
// the KMP uniffi-bindgen embeds the source module path into generated
// C symbol names (`uniffi_<crate>::qr_fn_func_<name>`), which Clang
// then refuses to parse during cinterop. Root-level declarations
// produce flat `uniffi_<crate>_fn_func_<name>` symbols that cinterop
// is happy with.

/// Errors surfaced through UniFFI to Kotlin / Swift callers of
/// [`qr_decode_luma`]. See `crate::qr::QrDecodeError`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum QrDecodeError {
    #[error("invalid luma dimensions: width={width} height={height} luma_len={luma_len}")]
    InvalidDimensions {
        width: u32,
        height: u32,
        luma_len: u32,
    },
    #[error("decoder error: {detail}")]
    DecoderError { detail: String },
}

impl From<qr::QrDecodeError> for QrDecodeError {
    fn from(value: qr::QrDecodeError) -> Self {
        match value {
            qr::QrDecodeError::InvalidDimensions {
                width,
                height,
                luma_len,
            } => Self::InvalidDimensions {
                width,
                height,
                luma_len,
            },
            qr::QrDecodeError::DecoderError { detail } => Self::DecoderError { detail },
        }
    }
}

/// Decode a QR code from an 8-bit grayscale image (Y plane of a YUV
/// camera frame, or `0.299*R + 0.587*G + 0.114*B` of an RGB photo).
///
/// `luma` must be exactly `width * height` bytes, row-major.
///
/// - `Ok(Some(text))` — QR found
/// - `Ok(None)`       — no QR in this frame (steady-state during live scan)
/// - `Err(InvalidDimensions)` — caller-side dimensions / length mismatch
/// - `Err(DecoderError)` — rxing internal failure (rare)
#[uniffi::export]
pub fn qr_decode_luma(
    width: u32,
    height: u32,
    luma: Vec<u8>,
) -> Result<Option<String>, QrDecodeError> {
    qr::decode_luma(width, height, luma).map_err(Into::into)
}

// ───────────────────── QR_CODE_SPEC v1.4 D0 — QR encoder ─────────────────────
//
// Local-only encoder. No network. Kotlin/Swift call this to turn the
// `qr_code` URL string returned by `user/qrcode/get` / `group/qrcode/get`
// into a cell matrix that gearui-kit renders directly as a `Box` grid.

/// Errors surfaced through UniFFI to Kotlin / Swift callers of
/// [`qr_encode_matrix`]. See `crate::qr::QrEncodeError`.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum QrEncodeError {
    #[error("qr text must not be empty")]
    EmptyText,
    #[error("encoder error: {detail}")]
    EncoderError { detail: String },
}

impl From<qr::QrEncodeError> for QrEncodeError {
    fn from(value: qr::QrEncodeError) -> Self {
        match value {
            qr::QrEncodeError::EmptyText => Self::EmptyText,
            qr::QrEncodeError::EncoderError { detail } => Self::EncoderError { detail },
        }
    }
}

/// One encoded QR matrix delivered to Kotlin/Swift.
///
/// Wire shape: `size` modules per side, `cells` is row-major with
/// length `size * size`. Values are `0` (light) or `1` (dark).
/// Quiet zone is NOT included — UI adds the standard 4-module margin
/// as container padding.
#[derive(Debug, Clone, uniffi::Record)]
pub struct QrMatrixView {
    pub size: u32,
    pub cells: Vec<u8>,
}

/// Encode `text` to a QR matrix at error-correction level **M**
/// (~15% redundancy, sensible for permanent name-card / group URLs).
///
/// - `Ok(QrMatrixView)` — success
/// - `Err(EmptyText)`   — caller passed empty / whitespace-only text
/// - `Err(EncoderError)` — payload too long / unsupported charset / etc.
#[uniffi::export]
pub fn qr_encode_matrix(text: String) -> Result<QrMatrixView, QrEncodeError> {
    let m = qr::encode_matrix(&text).map_err(QrEncodeError::from)?;
    Ok(QrMatrixView {
        size: m.size,
        cells: m.cells,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        map_sdk_event, metadata_input_extension, parse_read_list_entries, parse_read_list_user_ids,
        PrivchatClient, PrivchatConfig, SdkEvent, ServerEndpoint, SyncPhase, SyncRunKind,
        TransportProtocol,
    };

    #[test]
    fn attachment_extension_requires_an_object() {
        assert!(metadata_input_extension(Some("[]")).is_err());
        assert!(metadata_input_extension(Some("not-json")).is_err());
        assert_eq!(metadata_input_extension(Some("  ")).unwrap(), None);
    }

    #[test]
    fn attachment_extension_preserves_opaque_fields() {
        let extension = metadata_input_extension(Some(r#"{"vendor":"camera","width":1}"#))
            .expect("valid object")
            .expect("present object");
        assert_eq!(extension["vendor"], "camera");
        assert_eq!(extension["width"], 1);
    }

    #[test]
    fn sync_state_event_preserves_typed_fields() {
        let event = map_sdk_event(privchat_sdk::SdkEvent::SyncStateChanged {
            state: privchat_sdk::SyncStateSnapshot {
                phase: privchat_sdk::SyncPhase::Retrying,
                run_kind: Some(privchat_sdk::SyncRunKind::Resume),
                attempt: 3,
                error_code: Some(9),
                message: Some("transport".to_string()),
                updated_at_ms: 42,
            },
        });
        let SdkEvent::SyncStateChanged { state } = event else {
            panic!("expected typed sync state event");
        };
        assert_eq!(state.phase, SyncPhase::Retrying);
        assert_eq!(state.run_kind, Some(SyncRunKind::Resume));
        assert_eq!(state.attempt, 3);
        assert_eq!(state.error_code, Some(9));
        assert_eq!(state.updated_at_ms, 42);
    }

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
            data_dir: String::new(),
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
            // Actor startup can contend with the rest of the FFI test suite on CI.
            // Keep the event assertion, but use a realistic async scheduling budget.
            .next_event_envelope(2_000)
            .await
            .expect("poll should not fail");
        assert!(
            evt.is_some(),
            "envelope polling should return at least one event during shutdown"
        );
    }
}

uniffi::setup_scaffolding!();

// UniFFI 0.31 kotlin-multiplatform bindings may reference pointer future helpers.
// Our current FFI surface does not produce pointer-based async results, but iOS
// linking still expects these symbols to exist.
#[repr(C)]
struct UniffiRustBufferCompat {
    capacity: i64,
    len: i64,
    data: *mut u8,
}

#[repr(C)]
struct UniffiRustCallStatusCompat {
    code: i8,
    error_buf: UniffiRustBufferCompat,
}

type UniffiRustFutureContinuationCallbackCompat = extern "C" fn(i64, i8);

#[inline]
unsafe fn set_call_status_ok(out_status: *mut UniffiRustCallStatusCompat) {
    if out_status.is_null() {
        return;
    }
    // SAFETY: caller provides a valid pointer when non-null.
    unsafe {
        (*out_status).code = 0;
        (*out_status).error_buf = UniffiRustBufferCompat {
            capacity: 0,
            len: 0,
            data: std::ptr::null_mut(),
        };
    }
}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_poll_pointer(
    _handle: i64,
    callback: Option<UniffiRustFutureContinuationCallbackCompat>,
    callback_data: i64,
) {
    if let Some(cb) = callback {
        cb(callback_data, 1);
    }
}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_cancel_pointer(_handle: i64) {}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_free_pointer(_handle: i64) {}

#[unsafe(no_mangle)]
extern "C" fn ffi_privchat_sdk_ffi_rust_future_complete_pointer(
    _handle: i64,
    out_status: *mut UniffiRustCallStatusCompat,
) -> *mut std::ffi::c_void {
    // SAFETY: output pointer is owned by caller.
    unsafe { set_call_status_ok(out_status) };
    std::ptr::null_mut()
}
