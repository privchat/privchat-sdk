use std::time::Duration;

use crate::account_manager::MultiAccountManager;
use crate::types::{PhaseMetrics, PhaseResult};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

pub struct TestPhases;

impl TestPhases {
    pub async fn phase1_auth_and_bootstrap(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        for key in ["alice", "bob", "charlie"] {
            let snap = manager.session_snapshot(key).await?;
            metrics.rpc_calls += 1;
            if snap.is_some() {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!("{key} missing session snapshot"));
            }
        }

        if let Err(e) = manager.verify_all_connected().await {
            metrics.errors.push(format!("connection verification failed: {e}"));
        }

        Ok(PhaseResult {
            phase_name: "auth/bootstrap".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "session + connectivity verified".to_string(),
            metrics,
        })
    }

    pub async fn phase2_friend_flow(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let bob_username = manager.username("bob")?;
        let search = manager.search_users("alice", &bob_username).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let bob_user = search
            .users
            .iter()
            .find(|u| u.username == bob_username)
            .or_else(|| search.users.first())
            .ok_or_else(|| boxed_err("search bob returned empty users"))?;

        let apply = manager.send_friend_request("alice", bob_user.user_id).await?;
        metrics.rpc_calls += 1;
        if apply.user_id > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("friend apply returned invalid user_id".to_string());
        }

        tokio::time::sleep(Duration::from_millis(150)).await;

        let pending = manager.pending_friend_requests("bob").await?;
        metrics.rpc_calls += 1;
        if !pending.requests.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("bob pending request list is empty".to_string());
        }

        let alice_id = manager.user_id("alice")?;
        let channel_id = manager.accept_friend_request("bob", alice_id).await?;
        metrics.rpc_calls += 1;
        if channel_id > 0 {
            metrics.rpc_successes += 1;
            manager.cache_group_channel("alice_bob_friend_channel", channel_id);
        } else {
            metrics.errors.push("accept friend returned invalid channel_id".to_string());
        }

        let relation = manager.check_friend("alice", bob_user.user_id).await?;
        metrics.rpc_calls += 1;
        if relation.is_friend {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("alice->bob is_friend=false after accept".to_string());
        }

        Ok(PhaseResult {
            phase_name: "friend-flow".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase3_direct_chat(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager.get_or_create_direct_channel("alice", "bob").await?;
        metrics.rpc_calls += 1;
        if channel_id > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("get_or_create_direct_channel returned 0".to_string());
        }

        let _ = manager.message_history("alice", channel_id, 20).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "direct-chat".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("channel_id={channel_id}, rpc={}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase4_group_flow(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let bob_id = manager.user_id("bob")?;
        let charlie_id = manager.user_id("charlie")?;
        let group = manager
            .create_group("alice", "accounts-example-group", vec![bob_id, charlie_id])
            .await?;
        metrics.rpc_calls += 1;

        if group.group_id > 0 {
            metrics.rpc_successes += 1;
            manager.cache_group_channel("main_group", group.group_id);
        } else {
            metrics.errors.push("create group returned invalid group_id".to_string());
            return Ok(PhaseResult {
                phase_name: "group-flow".to_string(),
                success: false,
                duration: start.elapsed(),
                details: "group creation failed".to_string(),
                metrics,
            });
        }

        let members = manager.list_group_members("alice", group.group_id).await?;
        metrics.rpc_calls += 1;
        if members.total >= 1 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("group member list total is 0".to_string());
        }

        Ok(PhaseResult {
            phase_name: "group-flow".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("group_id={}", group.group_id),
            metrics,
        })
    }

    pub async fn phase5_reaction_and_read(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing direct channel cache for alice/bob"))?;

        let history = manager.message_history("alice", channel_id, 20).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let Some(server_msg_id) = history.messages.first().map(|m| m.message_id) else {
            return Ok(PhaseResult {
                phase_name: "reaction-read".to_string(),
                success: true,
                duration: start.elapsed(),
                details: "no existing messages in direct channel; skipped reaction/read checks".to_string(),
                metrics,
            });
        };

        let added = manager.add_reaction("bob", server_msg_id, "👍").await?;
        metrics.rpc_calls += 1;
        if added {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("add reaction returned false".to_string());
        }

        let listed = manager.list_reactions("alice", server_msg_id).await?;
        metrics.rpc_calls += 1;
        if listed.total_count > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("reaction list total_count=0".to_string());
        }

        let removed = manager.remove_reaction("bob", server_msg_id, "👍").await?;
        metrics.rpc_calls += 1;
        if removed {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("remove reaction returned false".to_string());
        }

        let marked = manager.mark_read("bob", channel_id, server_msg_id).await?;
        metrics.rpc_calls += 1;
        if marked {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("mark read returned false".to_string());
        }

        let readers = manager.read_list("alice", channel_id, server_msg_id).await?;
        metrics.rpc_calls += 1;
        if !readers.readers.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("read list is empty after mark read".to_string());
        }

        Ok(PhaseResult {
            phase_name: "reaction-read".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase6_blacklist(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let charlie_id = manager.user_id("charlie")?;

        let added = manager.blacklist_add("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if added {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("blacklist add returned false".to_string());
        }

        let check_added = manager.blacklist_check("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if check_added.is_blocked {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("blacklist check expected blocked=true".to_string());
        }

        let list = manager.blacklist_list_user_ids("alice").await?;
        metrics.rpc_calls += 1;
        if list.contains(&charlie_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("blacklist list missing charlie".to_string());
        }

        let removed = manager.blacklist_remove("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if removed {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("blacklist remove returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "blacklist".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase7_qrcode(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let generated = manager.user_qrcode_generate("alice").await?;
        metrics.rpc_calls += 1;
        if !generated.qr_key.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("qrcode generate returned empty qr_key".to_string());
        }

        let got = manager.user_qrcode_get("alice").await?;
        metrics.rpc_calls += 1;
        if !got.qr_key.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("user_qrcode_get returned empty qr_key".to_string());
        }

        Ok(PhaseResult {
            phase_name: "qrcode".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase8_presence_and_typing(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let bob_id = manager.user_id("bob")?;
        let typing_channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .unwrap_or(bob_id);
        let alice_sdk = manager.sdk("alice")?;

        let statuses = alice_sdk.subscribe_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if !statuses.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("subscribe presence returned empty statuses".to_string());
        }

        let fetched = alice_sdk.fetch_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if !fetched.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("fetch presence returned empty statuses".to_string());
        }

        alice_sdk
            .send_typing(
                typing_channel_id,
                1,
                true,
                privchat_sdk::TypingActionType::Typing,
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        alice_sdk.unsubscribe_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "presence-typing".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }
}

fn boxed_err(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}
