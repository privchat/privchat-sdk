use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use privchat_protocol::rpc::routes;
use privchat_protocol::rpc::{
    AccountSearchQueryRequest, AccountSearchResponse, BlacklistAddRequest, BlacklistAddResponse,
    BlacklistCheckRequest, BlacklistCheckResponse, BlacklistListRequest,
    BlacklistRemoveRequest, BlacklistRemoveResponse, ClientSubmitRequest, ClientSubmitResponse,
    FriendAcceptRequest, FriendAcceptResponse, FriendApplyRequest, FriendApplyResponse,
    FriendCheckRequest, FriendCheckResponse, FriendPendingRequest, FriendPendingResponse,
    GetChannelPtsRequest, GetChannelPtsResponse, GetOrCreateDirectChannelRequest,
    GetOrCreateDirectChannelResponse, GroupCreateRequest, GroupCreateResponse,
    GroupMemberListRequest, GroupMemberListResponse, MessageHistoryGetRequest,
    MessageHistoryResponse, MessageReactionAddRequest, MessageReactionAddResponse,
    MessageReactionListRequest, MessageReactionListResponse, MessageReactionRemoveRequest,
    MessageReactionRemoveResponse, MessageReadListRequest, MessageReadListResponse,
    MessageStatusReadRequest, MessageStatusReadResponse, UserQRCodeGenerateRequest,
    UserQRCodeGenerateResponse, UserQRCodeGetRequest, UserQRCodeGetResponse,
    UserQRCodeRefreshRequest, UserQRCodeRefreshResponse,
};
use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, SessionSnapshot, TransportProtocol};
use serde::de::DeserializeOwned;
use serde::Serialize;

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
    pub base_dir: PathBuf,
}

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
        let base_dir = std::env::temp_dir().join(format!("privchat-rust-accounts-{}-{}", ts, std::process::id()));
        std::fs::create_dir_all(&base_dir)?;

        let mut manager = Self {
            accounts: HashMap::new(),
            direct_channels: HashMap::new(),
            group_channels: HashMap::new(),
            endpoints,
            base_dir,
        };

        let suffix = format!("{}{}", ts % 100000, std::process::id());
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
        self.accounts.get(key).ok_or_else(|| boxed_err(format!("account not found: {key}")))
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

    pub async fn send_friend_request(&self, from: &str, to_user_id: u64) -> BoxResult<FriendApplyResponse> {
        let sdk = &self.account(from)?.sdk;
        let body = serde_json::to_string(&FriendApplyRequest {
            target_user_id: to_user_id,
            message: Some("hello from accounts example".to_string()),
            source: Some("friend".to_string()),
            source_id: Some(to_user_id.to_string()),
            from_user_id: 0,
        })?;
        let raw = sdk.rpc_call(routes::friend::APPLY.to_string(), body).await?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let user_id = value
            .get("user_id")
            .and_then(|v| match v {
                serde_json::Value::Number(n) => n.as_u64(),
                serde_json::Value::String(s) => s.parse::<u64>().ok(),
                _ => None,
            })
            .unwrap_or(0);
        Ok(FriendApplyResponse {
            user_id,
            username: value
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            status: value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            added_at: value
                .get("added_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            message: value
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
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

    pub async fn accept_friend_request(&self, to: &str, from_user_id: u64) -> BoxResult<FriendAcceptResponse> {
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

    pub fn cache_group_channel(&mut self, name: &str, channel_id: u64) {
        self.group_channels.insert(name.to_string(), channel_id);
    }

    pub fn cached_group_channel(&self, name: &str) -> Option<u64> {
        self.group_channels.get(name).copied()
    }

    pub async fn create_group(&self, owner: &str, name: &str, members: Vec<u64>) -> BoxResult<GroupCreateResponse> {
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

    pub async fn list_group_members(&self, key: &str, group_id: u64) -> BoxResult<GroupMemberListResponse> {
        self.rpc_typed(
            key,
            routes::group_member::LIST,
            &GroupMemberListRequest { group_id, user_id: 0 },
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
        let pts_resp: GetChannelPtsResponse = self
            .rpc_typed(
                key,
                routes::sync::GET_CHANNEL_PTS,
                &GetChannelPtsRequest {
                    channel_id,
                    channel_type,
                },
            )
            .await?;

        let local_message_id = next_local_message_id();
        self.rpc_typed(
            key,
            routes::sync::SUBMIT,
            &ClientSubmitRequest {
                local_message_id,
                channel_id,
                channel_type,
                last_pts: pts_resp.current_pts,
                command_type: "text".to_string(),
                payload: serde_json::json!({ "text": text }),
                client_timestamp: now_millis(),
                device_id: None,
            },
        )
        .await
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

    pub async fn add_reaction(&self, key: &str, server_message_id: u64, emoji: &str) -> BoxResult<MessageReactionAddResponse> {
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

    pub async fn list_reactions(&self, key: &str, server_message_id: u64) -> BoxResult<MessageReactionListResponse> {
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

    pub async fn mark_read(&self, key: &str, channel_id: u64, server_message_id: u64) -> BoxResult<MessageStatusReadResponse> {
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

    pub async fn read_list(&self, key: &str, channel_id: u64, server_message_id: u64) -> BoxResult<MessageReadListResponse> {
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

    pub async fn blacklist_add(&self, key: &str, blocked_user_id: u64) -> BoxResult<BlacklistAddResponse> {
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

    pub async fn blacklist_remove(&self, key: &str, blocked_user_id: u64) -> BoxResult<BlacklistRemoveResponse> {
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

    pub async fn blacklist_check(&self, key: &str, target_user_id: u64) -> BoxResult<BlacklistCheckResponse> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&BlacklistCheckRequest {
            user_id: 0,
            target_user_id,
        })?;
        let raw = sdk
            .rpc_call(routes::blacklist::CHECK.to_string(), body)
            .await?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let is_blocked = value
            .get("is_blocked")
            .or_else(|| value.get("blocked"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(BlacklistCheckResponse { is_blocked })
    }

    pub async fn blacklist_list_user_ids(&self, key: &str) -> BoxResult<Vec<u64>> {
        let sdk = &self.account(key)?.sdk;
        let body = serde_json::to_string(&BlacklistListRequest { user_id: 0 })?;
        let raw = sdk
            .rpc_call(routes::blacklist::LIST.to_string(), body)
            .await?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        let users = value
            .get("users")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let ids = users
            .into_iter()
            .filter_map(|u| {
                u.get("blocked_user_id")
                    .or_else(|| u.get("user_id"))
                    .and_then(|v| v.as_u64())
            })
            .collect();
        Ok(ids)
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

    pub async fn cleanup(&self) -> BoxResult<()> {
        for account in self.accounts.values() {
            let _ = account.sdk.disconnect().await;
            account.sdk.shutdown().await;
        }
        Ok(())
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

fn next_local_message_id() -> u64 {
    let base = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let seq = LOCAL_MSG_SEQ.fetch_add(1, Ordering::Relaxed);
    (base << 12) | (seq & 0xFFF)
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
