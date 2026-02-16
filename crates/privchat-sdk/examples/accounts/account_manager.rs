use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::{
    AccountPrivacyGetRequest, AccountPrivacyGetResponse, AccountPrivacyUpdateRequest,
    AccountPrivacyUpdateResponse, AccountProfileGetRequest, AccountProfileGetResponse,
    AccountProfileUpdateRequest, AccountProfileUpdateResponse, AccountSearchQueryRequest,
    AccountSearchResponse, BlacklistAddRequest, BlacklistAddResponse, BlacklistCheckRequest,
    BlacklistCheckResponse, BlacklistListRequest, BlacklistRemoveRequest, BlacklistRemoveResponse,
    ChannelHideRequest, ChannelHideResponse, ChannelMuteRequest, ChannelMuteResponse,
    ChannelPinRequest, ChannelPinResponse, ClientSubmitResponse, FileRequestUploadTokenRequest,
    FileRequestUploadTokenResponse, FileUploadCallbackRequest, FileUploadCallbackResponse,
    FriendAcceptRequest, FriendAcceptResponse, FriendApplyRequest, FriendApplyResponse,
    FriendCheckRequest, FriendCheckResponse, FriendPendingRequest, FriendPendingResponse,
    FriendRejectRequest, FriendRejectResponse, FriendRemoveRequest, FriendRemoveResponse,
    GetDifferenceRequest, GetDifferenceResponse, GetOrCreateDirectChannelRequest,
    GetOrCreateDirectChannelResponse, GroupApprovalListRequest, GroupApprovalListResponse,
    GroupCreateRequest, GroupCreateResponse, GroupInfoRequest, GroupInfoResponse,
    GroupMemberAddRequest, GroupMemberAddResponse, GroupMemberLeaveRequest,
    GroupMemberLeaveResponse, GroupMemberListRequest, GroupMemberListResponse,
    GroupMemberMuteRequest, GroupMemberRemoveRequest, GroupMemberRemoveResponse,
    GroupMemberUnmuteRequest, GroupMuteAllRequest, GroupMuteAllResponse,
    GroupQRCodeGenerateRequest, GroupQRCodeGenerateResponse, GroupRoleSetRequest,
    GroupRoleSetResponse, GroupSettingsGetRequest, GroupSettingsGetResponse, GroupSettingsPatch,
    GroupSettingsUpdateRequest, GroupSettingsUpdateResponse, MessageHistoryGetRequest,
    MessageHistoryResponse, MessageReactionAddRequest, MessageReactionAddResponse,
    MessageReactionListRequest, MessageReactionListResponse, MessageReactionRemoveRequest,
    MessageReactionRemoveResponse, MessageReactionStatsRequest, MessageReactionStatsResponse,
    MessageReadListRequest, MessageReadListResponse, MessageReadStatsRequest,
    MessageReadStatsResponse, MessageRevokeRequest, MessageRevokeResponse,
    MessageStatusCountRequest, MessageStatusCountResponse, MessageStatusReadRequest,
    MessageStatusReadResponse, ServerDecision, StickerPackageDetailRequest,
    StickerPackageDetailResponse, StickerPackageListRequest, StickerPackageListResponse,
    UserQRCodeGenerateRequest, UserQRCodeGenerateResponse, UserQRCodeGetRequest,
    UserQRCodeGetResponse, UserQRCodeRefreshRequest, UserQRCodeRefreshResponse,
};
use privchat_sdk::{
    NewMessage, PrivchatConfig, PrivchatSdk, ServerEndpoint, SessionSnapshot, TransportProtocol,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::types::AccountConfig;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

static LOCAL_MSG_SEQ: AtomicU64 = AtomicU64::new(1);

struct ManagedAccount {
    cfg: AccountConfig,
    sdk: Arc<PrivchatSdk>,
}

pub struct MultiAccountManager {
    accounts: HashMap<String, ManagedAccount>,
    direct_channels: HashMap<String, u64>,
    group_channels: HashMap<String, u64>,
    endpoints: Vec<ServerEndpoint>,
    suffix: String,
    pub base_dir: PathBuf,
}

pub const DIRECT_SYNC_CHANNEL_TYPE: u8 = 1;
pub const GROUP_SYNC_CHANNEL_TYPE: u8 = 2;

impl MultiAccountManager {
    pub async fn new() -> BoxResult<Self> {
        let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let tcp_port = parse_env_u16("PRIVCHAT_TCP_PORT", 9001);
        let quic_port = parse_env_u16("PRIVCHAT_QUIC_PORT", 9001);
        let ws_port = parse_env_u16("PRIVCHAT_WS_PORT", 9080);

        let endpoints = vec![
            ServerEndpoint {
                protocol: TransportProtocol::Quic,
                host: host.clone(),
                port: quic_port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::Tcp,
                host: host.clone(),
                port: tcp_port,
                path: None,
                use_tls: false,
            },
            ServerEndpoint {
                protocol: TransportProtocol::WebSocket,
                host,
                port: ws_port,
                path: Some("/".to_string()),
                use_tls: false,
            },
        ];

        let ts = now_millis();
        let base_dir = std::env::temp_dir().join(format!(
            "privchat-rust-accounts-{}-{}",
            ts,
            std::process::id()
        ));
        std::fs::create_dir_all(&base_dir)?;

        let mut manager = Self {
            accounts: HashMap::new(),
            direct_channels: HashMap::new(),
            group_channels: HashMap::new(),
            endpoints,
            suffix: String::new(),
            base_dir,
        };

        let suffix = format!("{}{}", ts % 100000, std::process::id());
        manager.suffix = suffix.clone();
        manager.create_and_auth_account("alice", &suffix).await?;
        manager.create_and_auth_account("bob", &suffix).await?;
        manager.create_and_auth_account("charlie", &suffix).await?;

        Ok(manager)
    }

    async fn create_and_auth_account(&mut self, key: &str, suffix: &str) -> BoxResult<()> {
        let username = format!("{}_{}", key, suffix);
        let password = "password123".to_string();
        let device_id = pseudo_uuid_v4_like();
        let data_dir = self.base_dir.join(key);
        std::fs::create_dir_all(&data_dir)?;

        let sdk = Arc::new(PrivchatSdk::new(PrivchatConfig {
            endpoints: self.endpoints.clone(),
            connection_timeout_secs: 30,
            data_dir: data_dir.to_string_lossy().to_string(),
        }));

        sdk.connect().await?;
        let login = sdk
            .register(username.clone(), password.clone(), device_id.clone())
            .await?;
        sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
            .await?;
        sdk.run_bootstrap_sync().await?;

        let cfg = AccountConfig {
            key: key.to_string(),
            username,
            password,
            user_id: login.user_id,
            token: login.token,
            device_id,
            data_dir: data_dir.to_string_lossy().to_string(),
        };

        self.accounts
            .insert(key.to_string(), ManagedAccount { cfg, sdk });
        Ok(())
    }

    fn account(&self, key: &str) -> BoxResult<&ManagedAccount> {
        self.accounts
            .get(key)
            .ok_or_else(|| boxed_err(format!("account not found: {key}")))
    }

    pub async fn ensure_account(&mut self, key: &str) -> BoxResult<()> {
        if self.accounts.contains_key(key) {
            return Ok(());
        }
        let suffix = self.suffix.clone();
        self.create_and_auth_account(key, &suffix).await
    }

    pub fn account_keys(&self) -> Vec<String> {
        self.accounts.keys().cloned().collect()
    }

    pub fn user_id(&self, key: &str) -> BoxResult<u64> {
        Ok(self.account(key)?.cfg.user_id)
    }

    pub fn username(&self, key: &str) -> BoxResult<String> {
        Ok(self.account(key)?.cfg.username.clone())
    }

    pub fn account_config(&self, key: &str) -> BoxResult<AccountConfig> {
        Ok(self.account(key)?.cfg.clone())
    }

    pub fn sdk(&self, key: &str) -> BoxResult<Arc<PrivchatSdk>> {
        Ok(self.account(key)?.sdk.clone())
    }

    pub async fn verify_all_connected(&self) -> BoxResult<()> {
        for (key, account) in &self.accounts {
            let connected = account.sdk.is_connected().await?;
            if !connected {
                return Err(boxed_err(format!("account {key} not connected")));
            }
            let snap = account.sdk.session_snapshot().await?;
            if snap.is_none() {
                return Err(boxed_err(format!("account {key} has empty session")));
            }
        }
        Ok(())
    }

    pub async fn session_snapshot(&self, key: &str) -> BoxResult<Option<SessionSnapshot>> {
        Ok(self.account(key)?.sdk.session_snapshot().await?)
    }

    pub async fn rpc_typed<Req, Resp>(&self, key: &str, route: &str, req: &Req) -> BoxResult<Resp>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(req)?;
        let raw = sdk.rpc_call(route.to_string(), body).await?;
        let resp: Resp = serde_json::from_str(&raw)
            .map_err(|e| boxed_err(format!("decode {route} failed: {e}; raw={raw}")))?;
        Ok(resp)
    }

    pub async fn search_users(&self, from: &str, query: &str) -> BoxResult<AccountSearchResponse> {
        self.rpc_typed(
            from,
            routes::account_search::QUERY,
            &AccountSearchQueryRequest {
                query: query.to_string(),
                page: Some(1),
                page_size: Some(20),
                from_user_id: 0,
            },
        )
        .await
    }

    pub async fn send_friend_request(
        &self,
        from: &str,
        to_user_id: u64,
    ) -> BoxResult<FriendApplyResponse> {
        let sdk = &self.account(from)?.sdk;
        let body = serde_json::to_string(&FriendApplyRequest {
            target_user_id: to_user_id,
            message: Some("hello from accounts example".to_string()),
            source: Some("friend".to_string()),
            source_id: Some(to_user_id.to_string()),
            from_user_id: 0,
        })?;
        let raw = sdk
            .rpc_call(routes::friend::APPLY.to_string(), body)
            .await?;
        let resp: FriendApplyCompat = serde_json::from_str(&raw).map_err(|e| {
            boxed_err(format!(
                "decode {} failed: {e}; raw={raw}",
                routes::friend::APPLY
            ))
        })?;
        Ok(FriendApplyResponse {
            user_id: resp.user_id,
            username: resp.username,
            status: resp.status,
            added_at: resp.added_at,
            message: resp.message,
        })
    }

    pub async fn pending_friend_requests(&self, key: &str) -> BoxResult<FriendPendingResponse> {
        self.rpc_typed(
            key,
            routes::friend::PENDING,
            &FriendPendingRequest { user_id: 0 },
        )
        .await
    }

    pub async fn accept_friend_request(
        &self,
        to: &str,
        from_user_id: u64,
    ) -> BoxResult<FriendAcceptResponse> {
        self.rpc_typed(
            to,
            routes::friend::ACCEPT,
            &FriendAcceptRequest {
                from_user_id,
                target_user_id: 0,
                message: Some("accepted".to_string()),
            },
        )
        .await
    }

    pub async fn check_friend(&self, key: &str, friend_id: u64) -> BoxResult<FriendCheckResponse> {
        self.rpc_typed(
            key,
            routes::friend::CHECK,
            &FriendCheckRequest {
                friend_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn get_or_create_direct_channel(&mut self, from: &str, to: &str) -> BoxResult<u64> {
        let to_user_id = self.user_id(to)?;
        let resp: GetOrCreateDirectChannelResponse = self
            .rpc_typed(
                from,
                routes::channel::DIRECT_GET_OR_CREATE,
                &GetOrCreateDirectChannelRequest {
                    target_user_id: to_user_id,
                    source: Some("accounts-example".to_string()),
                    source_id: Some("phase3".to_string()),
                    user_id: 0,
                },
            )
            .await?;

        self.direct_channels
            .insert(direct_key(from, to), resp.channel_id);
        Ok(resp.channel_id)
    }

    pub fn cached_direct_channel(&self, a: &str, b: &str) -> Option<u64> {
        self.direct_channels.get(&direct_key(a, b)).copied()
    }

    pub async fn list_local_friends(
        &self,
        key: &str,
    ) -> BoxResult<Vec<privchat_sdk::StoredFriend>> {
        Ok(self.account(key)?.sdk.list_friends(200, 0).await?)
    }

    pub async fn refresh_local_views(&self, key: &str) -> BoxResult<()> {
        let sdk = &self.account(key)?.sdk;
        let _ = sdk.sync_entities("friend".to_string(), None).await?;
        let _ = sdk.sync_entities("group".to_string(), None).await?;
        let _ = sdk.sync_entities("channel".to_string(), None).await?;
        Ok(())
    }

    pub async fn refresh_all_local_views(&self) -> BoxResult<()> {
        for key in ["alice", "bob", "charlie"] {
            self.refresh_local_views(key).await?;
        }
        Ok(())
    }

    pub async fn list_local_channels(
        &self,
        key: &str,
    ) -> BoxResult<Vec<privchat_sdk::StoredChannel>> {
        Ok(self.account(key)?.sdk.list_channels(500, 0).await?)
    }

    pub async fn list_local_messages(
        &self,
        key: &str,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
    ) -> BoxResult<Vec<privchat_sdk::StoredMessage>> {
        Ok(self
            .account(key)?
            .sdk
            .list_messages(channel_id, channel_type, limit, 0)
            .await?)
    }

    pub fn cache_group_channel(&mut self, name: &str, channel_id: u64) {
        self.group_channels.insert(name.to_string(), channel_id);
    }

    pub fn cached_group_channel(&self, name: &str) -> Option<u64> {
        self.group_channels.get(name).copied()
    }

    pub async fn create_group(
        &self,
        owner: &str,
        name: &str,
        members: Vec<u64>,
    ) -> BoxResult<GroupCreateResponse> {
        self.rpc_typed(
            owner,
            routes::group::CREATE,
            &GroupCreateRequest {
                name: name.to_string(),
                description: Some("accounts example group".to_string()),
                member_ids: Some(members),
                creator_id: 0,
            },
        )
        .await
    }

    pub async fn list_group_members(
        &self,
        key: &str,
        group_id: u64,
    ) -> BoxResult<GroupMemberListResponse> {
        self.rpc_typed(
            key,
            routes::group_member::LIST,
            &GroupMemberListRequest {
                group_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn send_text(
        &self,
        key: &str,
        channel_id: u64,
        channel_type: u8,
        text: &str,
    ) -> BoxResult<ClientSubmitResponse> {
        let sdk = self.sdk(key)?;
        let from_uid = self.user_id(key)?;
        let local_message_id = sdk
            .create_local_message(NewMessage {
                channel_id,
                channel_type: channel_type as i32,
                from_uid,
                message_type: 0,
                content: text.to_string(),
                searchable_word: String::new(),
                setting: 0,
                extra: String::new(),
            })
            .await?;
        sdk.enqueue_outbound_message(local_message_id, Vec::new())
            .await?;
        Ok(ClientSubmitResponse {
            decision: ServerDecision::Accepted,
            pts: None,
            server_msg_id: None,
            server_timestamp: now_millis(),
            local_message_id,
            has_gap: false,
            current_pts: 0,
        })
    }

    pub async fn message_history(
        &self,
        key: &str,
        channel_id: u64,
        limit: u32,
    ) -> BoxResult<MessageHistoryResponse> {
        self.rpc_typed(
            key,
            routes::message_history::GET,
            &MessageHistoryGetRequest {
                user_id: 0,
                channel_id,
                before_server_message_id: None,
                limit: Some(limit),
            },
        )
        .await
    }

    pub async fn add_reaction(
        &self,
        key: &str,
        server_message_id: u64,
        emoji: &str,
    ) -> BoxResult<MessageReactionAddResponse> {
        self.rpc_typed(
            key,
            routes::message_reaction::ADD,
            &MessageReactionAddRequest {
                server_message_id,
                channel_id: None,
                emoji: emoji.to_string(),
                user_id: 0,
            },
        )
        .await
    }

    pub async fn remove_reaction(
        &self,
        key: &str,
        server_message_id: u64,
        emoji: &str,
    ) -> BoxResult<MessageReactionRemoveResponse> {
        self.rpc_typed(
            key,
            routes::message_reaction::REMOVE,
            &MessageReactionRemoveRequest {
                server_message_id,
                emoji: emoji.to_string(),
                user_id: 0,
            },
        )
        .await
    }

    pub async fn list_reactions(
        &self,
        key: &str,
        server_message_id: u64,
    ) -> BoxResult<MessageReactionListResponse> {
        self.rpc_typed(
            key,
            routes::message_reaction::LIST,
            &MessageReactionListRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn mark_read(
        &self,
        key: &str,
        channel_id: u64,
        server_message_id: u64,
    ) -> BoxResult<MessageStatusReadResponse> {
        self.rpc_typed(
            key,
            routes::message_status::READ,
            &MessageStatusReadRequest {
                channel_id,
                message_id: server_message_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn read_list(
        &self,
        key: &str,
        channel_id: u64,
        server_message_id: u64,
    ) -> BoxResult<MessageReadListResponse> {
        self.rpc_typed(
            key,
            routes::message_status::READ_LIST,
            &MessageReadListRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await
    }

    pub async fn blacklist_add(
        &self,
        key: &str,
        blocked_user_id: u64,
    ) -> BoxResult<BlacklistAddResponse> {
        self.rpc_typed(
            key,
            routes::blacklist::ADD,
            &BlacklistAddRequest {
                user_id: 0,
                blocked_user_id,
            },
        )
        .await
    }

    pub async fn blacklist_remove(
        &self,
        key: &str,
        blocked_user_id: u64,
    ) -> BoxResult<BlacklistRemoveResponse> {
        self.rpc_typed(
            key,
            routes::blacklist::REMOVE,
            &BlacklistRemoveRequest {
                user_id: 0,
                blocked_user_id,
            },
        )
        .await
    }

    pub async fn blacklist_check(
        &self,
        key: &str,
        target_user_id: u64,
    ) -> BoxResult<BlacklistCheckResponse> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&BlacklistCheckRequest {
            user_id: 0,
            target_user_id,
        })?;
        let raw = sdk
            .rpc_call(routes::blacklist::CHECK.to_string(), body)
            .await?;
        let resp: BlacklistCheckCompat = serde_json::from_str(&raw).map_err(|e| {
            boxed_err(format!(
                "decode {} failed: {e}; raw={raw}",
                routes::blacklist::CHECK
            ))
        })?;
        Ok(BlacklistCheckResponse {
            is_blocked: resp.is_blocked,
        })
    }

    pub async fn blacklist_list_user_ids(&self, key: &str) -> BoxResult<Vec<u64>> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&BlacklistListRequest { user_id: 0 })?;
        let raw = sdk
            .rpc_call(routes::blacklist::LIST.to_string(), body)
            .await?;
        let resp: BlacklistListCompat = serde_json::from_str(&raw).map_err(|e| {
            boxed_err(format!(
                "decode {} failed: {e}; raw={raw}",
                routes::blacklist::LIST
            ))
        })?;
        Ok(resp
            .users
            .into_iter()
            .filter_map(|u| u.blocked_user_id.or(u.user_id))
            .collect())
    }

    pub async fn user_qrcode_generate(&self, key: &str) -> BoxResult<UserQRCodeGenerateResponse> {
        self.rpc_typed(
            key,
            routes::user_qrcode::GENERATE,
            &UserQRCodeGenerateRequest {
                expire_seconds: Some(3600),
                user_id: 0,
            },
        )
        .await
    }

    pub async fn user_qrcode_get(&self, key: &str) -> BoxResult<UserQRCodeGetResponse> {
        let uid = self.user_id(key)?;
        self.rpc_typed(
            key,
            routes::user_qrcode::GET,
            &UserQRCodeGetRequest {
                user_id: uid.to_string(),
            },
        )
        .await
    }

    pub async fn user_qrcode_refresh(&self, key: &str) -> BoxResult<UserQRCodeRefreshResponse> {
        let uid = self.user_id(key)?;
        self.rpc_typed(
            key,
            routes::user_qrcode::REFRESH,
            &UserQRCodeRefreshRequest {
                user_id: uid.to_string(),
            },
        )
        .await
    }

    pub async fn reject_friend_request(
        &self,
        to: &str,
        from_user_id: u64,
    ) -> BoxResult<FriendRejectResponse> {
        self.rpc_typed(
            to,
            routes::friend::REJECT,
            &FriendRejectRequest {
                from_user_id,
                target_user_id: 0,
                message: Some("rejected by accounts example".to_string()),
            },
        )
        .await
    }

    pub async fn remove_friend(
        &self,
        key: &str,
        friend_id: u64,
    ) -> BoxResult<FriendRemoveResponse> {
        self.rpc_typed(
            key,
            routes::friend::DELETE,
            &FriendRemoveRequest {
                friend_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn group_info(&self, key: &str, group_id: u64) -> BoxResult<GroupInfoResponse> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&GroupInfoRequest {
            group_id,
            user_id: 0,
        })?;
        let raw = sdk.rpc_call(routes::group::INFO.to_string(), body).await?;
        let wrapped: GroupInfoWrapped = serde_json::from_str(&raw).map_err(|e| {
            boxed_err(format!(
                "decode {} failed: {e}; raw={raw}",
                routes::group::INFO
            ))
        })?;
        Ok(wrapped.group_info)
    }

    pub async fn group_member_add(
        &self,
        key: &str,
        group_id: u64,
        user_id: u64,
    ) -> BoxResult<GroupMemberAddResponse> {
        self.rpc_typed(
            key,
            routes::group_member::ADD,
            &GroupMemberAddRequest {
                group_id,
                user_id,
                role: Some("member".to_string()),
                inviter_id: 0,
            },
        )
        .await
    }

    pub async fn group_member_remove(
        &self,
        key: &str,
        group_id: u64,
        user_id: u64,
    ) -> BoxResult<GroupMemberRemoveResponse> {
        self.rpc_typed(
            key,
            routes::group_member::REMOVE,
            &GroupMemberRemoveRequest {
                group_id,
                user_id,
                operator_id: 0,
            },
        )
        .await
    }

    pub async fn group_member_leave(
        &self,
        key: &str,
        group_id: u64,
    ) -> BoxResult<GroupMemberLeaveResponse> {
        self.rpc_typed(
            key,
            routes::group_member::LEAVE,
            &GroupMemberLeaveRequest {
                group_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn group_member_mute(
        &self,
        key: &str,
        group_id: u64,
        user_id: u64,
        secs: u64,
    ) -> BoxResult<u64> {
        self.rpc_typed(
            key,
            routes::group_member::MUTE,
            &GroupMemberMuteRequest {
                group_id,
                user_id,
                mute_duration: secs,
                operator_id: 0,
            },
        )
        .await
    }

    pub async fn group_member_unmute(
        &self,
        key: &str,
        group_id: u64,
        user_id: u64,
    ) -> BoxResult<bool> {
        self.rpc_typed(
            key,
            routes::group_member::UNMUTE,
            &GroupMemberUnmuteRequest {
                group_id,
                user_id,
                operator_id: 0,
            },
        )
        .await
    }

    pub async fn group_role_set(
        &self,
        key: &str,
        group_id: u64,
        operator_id: u64,
        user_id: u64,
        role: &str,
    ) -> BoxResult<GroupRoleSetResponse> {
        self.rpc_typed(
            key,
            routes::group_role::SET,
            &GroupRoleSetRequest {
                group_id,
                operator_id,
                user_id,
                role: role.to_string(),
            },
        )
        .await
    }

    pub async fn group_settings_get(
        &self,
        key: &str,
        group_id: u64,
    ) -> BoxResult<GroupSettingsGetResponse> {
        self.rpc_typed(
            key,
            routes::group_settings::GET,
            &GroupSettingsGetRequest {
                group_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn group_settings_update(
        &self,
        key: &str,
        group_id: u64,
        operator_id: u64,
        settings: GroupSettingsPatch,
    ) -> BoxResult<GroupSettingsUpdateResponse> {
        self.rpc_typed(
            key,
            routes::group_settings::UPDATE,
            &GroupSettingsUpdateRequest {
                group_id,
                operator_id,
                settings,
            },
        )
        .await
    }

    pub async fn group_mute_all(
        &self,
        key: &str,
        group_id: u64,
        operator_id: u64,
        muted: bool,
    ) -> BoxResult<GroupMuteAllResponse> {
        self.rpc_typed(
            key,
            routes::group_settings::MUTE_ALL,
            &GroupMuteAllRequest {
                group_id,
                operator_id,
                muted,
            },
        )
        .await
    }

    pub async fn group_approval_list(
        &self,
        key: &str,
        group_id: u64,
        operator_id: u64,
    ) -> BoxResult<GroupApprovalListResponse> {
        self.rpc_typed(
            key,
            routes::group_approval::LIST,
            &GroupApprovalListRequest {
                group_id,
                operator_id,
            },
        )
        .await
    }

    pub async fn group_qrcode_generate(
        &self,
        key: &str,
        group_id: u64,
    ) -> BoxResult<GroupQRCodeGenerateResponse> {
        self.rpc_typed(
            key,
            routes::group_qrcode::GENERATE,
            &GroupQRCodeGenerateRequest {
                group_id,
                expire_seconds: Some(3600),
                operator_id: 0,
            },
        )
        .await
    }

    pub async fn channel_pin(
        &self,
        key: &str,
        channel_id: u64,
        pinned: bool,
    ) -> BoxResult<ChannelPinResponse> {
        self.rpc_typed(
            key,
            routes::channel::PIN,
            &ChannelPinRequest {
                user_id: 0,
                channel_id,
                pinned,
            },
        )
        .await
    }

    pub async fn channel_mute(
        &self,
        key: &str,
        channel_id: u64,
        muted: bool,
    ) -> BoxResult<ChannelMuteResponse> {
        self.rpc_typed(
            key,
            routes::channel::MUTE,
            &ChannelMuteRequest {
                user_id: 0,
                channel_id,
                muted,
            },
        )
        .await
    }

    pub async fn channel_hide(&self, key: &str, channel_id: u64) -> BoxResult<ChannelHideResponse> {
        self.rpc_typed(
            key,
            routes::channel::HIDE,
            &ChannelHideRequest {
                user_id: 0,
                channel_id,
            },
        )
        .await
    }

    pub async fn sticker_package_list(&self, key: &str) -> BoxResult<StickerPackageListResponse> {
        self.rpc_typed(
            key,
            routes::sticker::PACKAGE_LIST,
            &StickerPackageListRequest {},
        )
        .await
    }

    pub async fn sticker_package_detail(
        &self,
        key: &str,
        package_id: String,
    ) -> BoxResult<StickerPackageDetailResponse> {
        self.rpc_typed(
            key,
            routes::sticker::PACKAGE_DETAIL,
            &StickerPackageDetailRequest { package_id },
        )
        .await
    }

    pub async fn file_request_upload_token(
        &self,
        key: &str,
        filename: &str,
        size: i64,
        mime: &str,
        file_type: &str,
    ) -> BoxResult<FileRequestUploadTokenResponse> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&FileRequestUploadTokenRequest {
            user_id: self.user_id(key)?,
            filename: Some(filename.to_string()),
            file_size: size,
            mime_type: mime.to_string(),
            file_type: file_type.to_string(),
            business_type: "message".to_string(),
        })?;
        let raw = sdk
            .rpc_call(routes::file::REQUEST_UPLOAD_TOKEN.to_string(), body)
            .await?;
        let resp: FileRequestUploadTokenCompat = serde_json::from_str(&raw).map_err(|e| {
            boxed_err(format!(
                "decode {} failed: {e}; raw={raw}",
                routes::file::REQUEST_UPLOAD_TOKEN
            ))
        })?;
        Ok(FileRequestUploadTokenResponse {
            token: resp.token,
            upload_url: resp.upload_url,
            file_id: resp
                .file_id
                .unwrap_or_else(|| "unknown-file-id".to_string()),
        })
    }

    pub async fn file_upload_callback(
        &self,
        key: &str,
        file_id: &str,
        status: &str,
    ) -> BoxResult<FileUploadCallbackResponse> {
        self.rpc_typed(
            key,
            routes::file::UPLOAD_CALLBACK,
            &FileUploadCallbackRequest {
                file_id: file_id.to_string(),
                user_id: self.user_id(key)?,
                status: status.to_string(),
            },
        )
        .await
    }

    pub async fn message_revoke(
        &self,
        key: &str,
        channel_id: u64,
        server_message_id: u64,
    ) -> BoxResult<MessageRevokeResponse> {
        self.rpc_typed(
            key,
            routes::message::REVOKE,
            &MessageRevokeRequest {
                server_message_id,
                channel_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn message_status_count(
        &self,
        key: &str,
        channel_id: Option<u64>,
    ) -> BoxResult<MessageStatusCountResponse> {
        self.rpc_typed(
            key,
            routes::message_status::COUNT,
            &MessageStatusCountRequest { channel_id },
        )
        .await
    }

    pub async fn message_read_stats(
        &self,
        key: &str,
        channel_id: u64,
        server_message_id: u64,
    ) -> BoxResult<MessageReadStatsResponse> {
        self.rpc_typed(
            key,
            routes::message_status::READ_STATS,
            &MessageReadStatsRequest {
                message_id: server_message_id,
                channel_id,
            },
        )
        .await
    }

    pub async fn reaction_stats(
        &self,
        key: &str,
        server_message_id: u64,
    ) -> BoxResult<MessageReactionStatsResponse> {
        self.rpc_typed(
            key,
            routes::message_reaction::STATS,
            &MessageReactionStatsRequest {
                server_message_id,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn get_difference(
        &self,
        key: &str,
        channel_id: u64,
        channel_type: u8,
        last_pts: u64,
        limit: Option<u32>,
    ) -> BoxResult<GetDifferenceResponse> {
        self.rpc_typed(
            key,
            routes::sync::GET_DIFFERENCE,
            &GetDifferenceRequest {
                channel_id,
                channel_type,
                last_pts,
                limit,
            },
        )
        .await
    }

    pub async fn profile_get(&self, key: &str) -> BoxResult<AccountProfileGetResponse> {
        self.rpc_typed(
            key,
            routes::account_profile::GET,
            &AccountProfileGetRequest { user_id: 0 },
        )
        .await
    }

    pub async fn profile_update(
        &self,
        key: &str,
        display_name: Option<String>,
        bio: Option<String>,
    ) -> BoxResult<AccountProfileUpdateResponse> {
        self.rpc_typed(
            key,
            routes::account_profile::UPDATE,
            &AccountProfileUpdateRequest {
                display_name,
                avatar_url: None,
                bio,
                extra_fields: HashMap::new(),
                user_id: 0,
            },
        )
        .await
    }

    pub async fn privacy_get(&self, key: &str) -> BoxResult<AccountPrivacyGetResponse> {
        self.rpc_typed(
            key,
            routes::privacy::GET,
            &AccountPrivacyGetRequest { user_id: 0 },
        )
        .await
    }

    pub async fn privacy_update(
        &self,
        key: &str,
        allow_receive_message_from_non_friend: Option<bool>,
    ) -> BoxResult<AccountPrivacyUpdateResponse> {
        self.rpc_typed(
            key,
            routes::privacy::UPDATE,
            &AccountPrivacyUpdateRequest {
                allow_add_by_group: None,
                allow_search_by_phone: None,
                allow_search_by_username: None,
                allow_search_by_email: None,
                allow_search_by_qrcode: None,
                allow_view_by_non_friend: None,
                allow_receive_message_from_non_friend,
                user_id: 0,
            },
        )
        .await
    }

    pub async fn login_with_new_sdk(&self, key: &str, suffix: &str) -> BoxResult<bool> {
        let cfg = self.account_config(key)?;
        let data_dir = self.base_dir.join(format!("{key}_login_{suffix}"));
        std::fs::create_dir_all(&data_dir)?;
        let sdk = PrivchatSdk::new(PrivchatConfig {
            endpoints: self.endpoints.clone(),
            connection_timeout_secs: 30,
            data_dir: data_dir.to_string_lossy().to_string(),
        });
        sdk.connect().await?;
        let login = sdk
            .login(
                cfg.username.clone(),
                cfg.password.clone(),
                cfg.device_id.clone(),
            )
            .await?;
        sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
            .await?;
        let ok = sdk
            .session_snapshot()
            .await?
            .map(|s| s.user_id == cfg.user_id)
            .unwrap_or(false);
        let _ = sdk.disconnect().await;
        sdk.shutdown().await;
        Ok(ok)
    }

    pub async fn verify_login_notice_persisted(
        &self,
        key: &str,
        suffix: &str,
    ) -> BoxResult<(bool, String)> {
        let cfg = self.account_config(key)?;
        let data_dir = self.base_dir.join(format!("{key}_login_notice_{suffix}"));
        std::fs::create_dir_all(&data_dir)?;

        let sdk = PrivchatSdk::new(PrivchatConfig {
            endpoints: self.endpoints.clone(),
            connection_timeout_secs: 30,
            data_dir: data_dir.to_string_lossy().to_string(),
        });

        let mut details = String::new();
        let mut ok = false;

        let run = async {
            sdk.connect().await?;
            let login = sdk
                .login(
                    cfg.username.clone(),
                    cfg.password.clone(),
                    pseudo_uuid_v4_like(),
                )
                .await?;
            sdk.authenticate(login.user_id, login.token.clone(), login.device_id.clone())
                .await?;
            sdk.run_bootstrap_sync().await?;

            for _ in 0..20 {
                let channels = sdk.list_channels(200, 0).await?;
                if let Some(system_channel) = channels.iter().find(|c| {
                    c.channel_type == 1
                        && (c.channel_name == "1"
                            || c.channel_remark == "1"
                            || c.channel_name == "System Message"
                            || c.channel_remark == "System Message"
                            || c.channel_name == "__system_1__"
                            || c.channel_remark == "__system_1__")
                }) {
                    let messages = sdk
                        .list_messages(
                            system_channel.channel_id,
                            system_channel.channel_type,
                            50,
                            0,
                        )
                        .await?;
                    let has_login_notice = messages.iter().any(|m| {
                        m.from_uid == 1 && m.content.contains("设备登录了")
                    });
                    details = format!(
                        "system_channel={} unread={} messages={} has_login_notice={}",
                        system_channel.channel_id,
                        system_channel.unread_count,
                        messages.len(),
                        has_login_notice
                    );
                    if has_login_notice {
                        ok = true;
                        return Ok::<(), BoxError>(());
                    }
                }
                if details.is_empty() {
                    let preview: Vec<String> = channels
                        .iter()
                        .take(6)
                        .map(|c| {
                            format!(
                                "{}:{}:{}:{}",
                                c.channel_id, c.channel_type, c.channel_name, c.channel_remark
                            )
                        })
                        .collect();
                    details = format!(
                        "system channel not matched; local channels={}",
                        preview.join(" | ")
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }

            if details.is_empty() {
                details = "system channel not found".to_string();
            }
            Ok::<(), BoxError>(())
        };

        let run_result = run.await;
        let _ = sdk.disconnect().await;
        sdk.shutdown().await;
        run_result?;
        Ok((ok, details))
    }

    pub async fn cleanup(&self) -> BoxResult<()> {
        for account in self.accounts.values() {
            let _ = account.sdk.disconnect().await;
            account.sdk.shutdown().await;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct FriendApplyCompat {
    #[serde(deserialize_with = "deserialize_u64_from_string_or_number")]
    user_id: u64,
    username: String,
    status: String,
    added_at: String,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlacklistCheckCompat {
    #[serde(alias = "blocked")]
    is_blocked: bool,
}

#[derive(Debug, Deserialize)]
struct GroupInfoWrapped {
    group_info: GroupInfoResponse,
}

#[derive(Debug, Deserialize)]
struct FileRequestUploadTokenCompat {
    #[serde(alias = "upload_token")]
    token: String,
    upload_url: String,
    file_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BlacklistListCompat {
    users: Vec<BlacklistUserCompat>,
}

#[derive(Debug, Deserialize)]
struct BlacklistUserCompat {
    user_id: Option<u64>,
    blocked_user_id: Option<u64>,
}

fn deserialize_u64_from_string_or_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("invalid numeric u64")),
        serde_json::Value::String(s) => s
            .parse::<u64>()
            .map_err(|e| serde::de::Error::custom(format!("invalid u64 string: {e}"))),
        _ => Err(serde::de::Error::custom(
            "expected string or number for u64",
        )),
    }
}

fn parse_env_u16(name: &str, default: u16) -> u16 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(default)
}

fn now_millis() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn direct_key(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}:{b}")
    } else {
        format!("{b}:{a}")
    }
}

fn pseudo_uuid_v4_like() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seq = LOCAL_MSG_SEQ.fetch_add(1, Ordering::Relaxed) as u128;
    let pid = std::process::id() as u128;
    let mut v = nanos ^ (seq << 8) ^ (pid << 48);
    // Set version(4) and variant(10xx) bits.
    v &= !(0xF_u128 << 76);
    v |= 0x4_u128 << 76;
    v &= !(0x3_u128 << 62);
    v |= 0x2_u128 << 62;
    let hex = format!("{v:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn boxed_err(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}
