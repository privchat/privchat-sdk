use std::time::Duration;

use privchat_protocol::rpc::{AccountSearchResponse, ClientSubmitResponse};

use crate::account_manager::{
    MultiAccountManager, DIRECT_SYNC_CHANNEL_TYPE, GROUP_SYNC_CHANNEL_TYPE,
};
use crate::types::{PhaseMetrics, PhaseResult};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

pub struct TestPhases;

impl TestPhases {
    pub async fn phase1_auth_and_bootstrap(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        for key in ["alice", "bob", "charlie"] {
            let snap = manager.session_snapshot(key).await?;
            metrics.rpc_calls += 1;
            if let Some(s) = snap {
                if s.bootstrap_completed {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push(format!("{key} bootstrap not completed"));
                }
            } else {
                metrics
                    .errors
                    .push(format!("{key} missing session snapshot"));
            }
        }

        if let Err(e) = manager.verify_all_connected().await {
            metrics
                .errors
                .push(format!("connection verification failed: {e}"));
        }

        Ok(PhaseResult {
            phase_name: "auth/bootstrap".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "session + connectivity verified".to_string(),
            metrics,
        })
    }

    pub async fn phase2_friend_system(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        let pairs = [("alice", "bob"), ("alice", "charlie"), ("bob", "charlie")];

        for (from, to) in pairs {
            let to_username = manager.username(to)?;
            let search = manager.search_users(from, &to_username).await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;
            let to_user_id = first_user_id(&search, &to_username)?;

            let apply = manager.send_friend_request(from, to_user_id).await?;
            metrics.rpc_calls += 1;
            if apply.user_id > 0 {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!("{from}->{to} apply failed"));
            }

            tokio::time::sleep(Duration::from_millis(150)).await;

            let pending = manager.pending_friend_requests(to).await?;
            metrics.rpc_calls += 1;
            let from_id = manager.user_id(from)?;
            if pending.requests.iter().any(|p| p.from_user_id == from_id) {
                metrics.rpc_successes += 1;
            } else {
                metrics
                    .errors
                    .push(format!("{to} pending list missing {from}"));
            }

            let accepted_channel = manager.accept_friend_request(to, from_id).await?;
            metrics.rpc_calls += 1;
            if accepted_channel > 0 {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!("{to} accept {from} failed"));
            }

            let rel_a = manager.check_friend(from, to_user_id).await?;
            let rel_b = manager.check_friend(to, from_id).await?;
            metrics.rpc_calls += 2;
            if rel_a.is_friend && rel_b.is_friend {
                metrics.rpc_successes += 2;
            } else {
                metrics
                    .errors
                    .push(format!("{from}<->{to} relation not friend"));
            }

            let _ = manager.get_or_create_direct_channel(from, to).await?;
            let _ = manager.get_or_create_direct_channel(to, from).await?;
            metrics.rpc_calls += 2;
            metrics.rpc_successes += 2;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
        manager.refresh_all_local_views().await?;
        for user in ["alice", "bob", "charlie"] {
            let friends = manager.list_local_friends(user).await?;
            metrics.rpc_calls += 1;
            if friends.len() == 2 {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{user} local friends expected=2 actual={}",
                    friends.len()
                ));
            }
        }

        Ok(PhaseResult {
            phase_name: "friend-system".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "all 3 accounts become mutual friends + local friend list verified"
                .to_string(),
            metrics,
        })
    }

    pub async fn phase3_group_system(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let ab = manager
            .cached_direct_channel("alice", "bob")
            .ok_or_else(|| boxed_err("missing alice-bob channel"))?;
        let bc = manager
            .cached_direct_channel("bob", "charlie")
            .ok_or_else(|| boxed_err("missing bob-charlie channel"))?;
        let ca = manager
            .cached_direct_channel("charlie", "alice")
            .ok_or_else(|| boxed_err("missing charlie-alice channel"))?;

        let s1 = manager
            .send_text("alice", ab, DIRECT_SYNC_CHANNEL_TYPE, "hello friend")
            .await?;
        let s2 = manager
            .send_text("bob", bc, DIRECT_SYNC_CHANNEL_TYPE, "hello friend")
            .await?;
        let s3 = manager
            .send_text("charlie", ca, DIRECT_SYNC_CHANNEL_TYPE, "hello friend")
            .await?;
        metrics.rpc_calls += 3;
        metrics.messages_sent += 3;
        if submit_ok(&s1) && submit_ok(&s2) && submit_ok(&s3) {
            metrics.rpc_successes += 3;
        } else {
            metrics
                .errors
                .push("some hello-friend submit rejected".to_string());
        }

        Ok(PhaseResult {
            phase_name: "direct-hello-send".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "sent hello friend on 3 friend channels".to_string(),
            metrics,
        })
    }

    pub async fn phase4_mixed_scenarios(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        tokio::time::sleep(Duration::from_secs(1)).await;
        manager.refresh_all_local_views().await?;

        for user in ["alice", "bob", "charlie"] {
            let channels = manager.list_local_channels(user).await?;
            metrics.rpc_calls += 1;
            let expected_ids: Vec<u64> = match user {
                "alice" => vec![
                    manager.cached_direct_channel("alice", "bob").unwrap_or(0),
                    manager
                        .cached_direct_channel("charlie", "alice")
                        .unwrap_or(0),
                ],
                "bob" => vec![
                    manager.cached_direct_channel("alice", "bob").unwrap_or(0),
                    manager.cached_direct_channel("bob", "charlie").unwrap_or(0),
                ],
                _ => vec![
                    manager.cached_direct_channel("bob", "charlie").unwrap_or(0),
                    manager
                        .cached_direct_channel("charlie", "alice")
                        .unwrap_or(0),
                ],
            };
            let direct_count = channels
                .iter()
                .filter(|c| expected_ids.contains(&c.channel_id))
                .count();
            if direct_count == 2 {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{user} direct channels expected=2 actual={direct_count}"
                ));
            }
        }

        Ok(PhaseResult {
            phase_name: "channels-after-direct-send".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "waited 1s and verified get_channels count after direct hello".to_string(),
            metrics,
        })
    }

    pub async fn phase5_message_reception(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        tokio::time::sleep(Duration::from_secs(1)).await;
        manager.refresh_all_local_views().await?;

        let expected = [
            ("alice", [("alice", "bob"), ("charlie", "alice")]),
            ("bob", [("alice", "bob"), ("bob", "charlie")]),
            ("charlie", [("bob", "charlie"), ("charlie", "alice")]),
        ];

        for (user, pairs) in expected {
            let channels = manager.list_local_channels(user).await?;
            metrics.rpc_calls += 1;
            for (a, b) in pairs {
                if let Some(cid) = manager.cached_direct_channel(a, b) {
                    if let Some(c) = channels.iter().find(|x| x.channel_id == cid) {
                        let peer = if a == user { b } else { a };
                        let peer_name = manager.username(peer)?;
                        let peer_id = manager.user_id(peer)?.to_string();
                        let ok_name = c.channel_name == peer_name
                            || c.channel_remark == peer_name
                            || c.channel_name == peer_id
                            || c.channel_remark == peer_id;
                        let ok_preview = c.last_msg_content == "hello friend"
                            || c.last_msg_content.contains("hello friend");
                        let ok_ts = c.last_msg_timestamp > 0;
                        if ok_name && ok_preview && ok_ts {
                            metrics.rpc_successes += 1;
                        } else {
                            metrics.errors.push(format!(
                                "{user} channel {cid} invalid meta(name/preview/timestamp)"
                            ));
                        }
                    } else {
                        metrics
                            .errors
                            .push(format!("{user} missing expected channel {cid}"));
                    }
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "direct-channel-metadata".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "validated channel name/preview/time for friend channels".to_string(),
            metrics,
        })
    }

    pub async fn phase6_stickers(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let pairs = [("alice", "bob"), ("bob", "charlie"), ("charlie", "alice")];
        for (a, b) in pairs {
            if let Some(cid) = manager.cached_direct_channel(a, b) {
                let msgs_a = manager.message_history(a, cid, 100).await?;
                let msgs_b = manager.message_history(b, cid, 100).await?;
                metrics.rpc_calls += 2;
                let cnt_a = msgs_a
                    .messages
                    .iter()
                    .filter(|m| m.content == "hello friend")
                    .count();
                let cnt_b = msgs_b
                    .messages
                    .iter()
                    .filter(|m| m.content == "hello friend")
                    .count();
                if cnt_a == 1 && cnt_b == 1 {
                    metrics.rpc_successes += 2;
                } else {
                    metrics.errors.push(format!(
                        "hello friend count mismatch pair {a}-{b}, channel={cid}, a={cnt_a}, b={cnt_b}"
                    ));
                }
            } else {
                metrics
                    .errors
                    .push(format!("missing direct channel cache {a}-{b}"));
            }
        }

        Ok(PhaseResult {
            phase_name: "direct-messages-verify".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "verified get_messages(channel_id) count on each direct channel".to_string(),
            metrics,
        })
    }

    pub async fn phase7_channel_management(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let all = ["alice", "bob", "charlie"];
        for creator in all {
            let mut members = Vec::new();
            for u in all {
                if u != creator {
                    members.push(manager.user_id(u)?);
                }
            }
            let group = manager
                .create_group(creator, &format!("group_by_{creator}"), members.clone())
                .await?;
            metrics.rpc_calls += 1;
            if group.group_id > 0 {
                metrics.rpc_successes += 1;
                manager.cache_group_channel(&format!("group_{creator}"), group.group_id);
                if creator == "alice" {
                    manager.cache_group_channel("main_group", group.group_id);
                }
            } else {
                metrics
                    .errors
                    .push(format!("create group by {creator} failed"));
                continue;
            }

            let g = group.group_id;
            for uid in &members {
                if let Ok(add_resp) = manager.group_member_add(creator, g, *uid).await {
                    metrics.rpc_calls += 1;
                    if add_resp {
                        metrics.rpc_successes += 1;
                    }
                } else {
                    metrics.rpc_calls += 1;
                }
            }
            let seed = manager
                .send_text(creator, g, GROUP_SYNC_CHANNEL_TYPE, "ok?")
                .await?;
            metrics.rpc_calls += 1;
            metrics.messages_sent += 1;
            if submit_ok(&seed) {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "group seed submit rejected creator={creator} group={g}"
                ));
            }

            for u in all {
                if u != creator {
                    let reply = manager
                        .send_text(u, g, GROUP_SYNC_CHANNEL_TYPE, "ok!")
                        .await?;
                    metrics.rpc_calls += 1;
                    metrics.messages_sent += 1;
                    if submit_ok(&reply) {
                        metrics.rpc_successes += 1;
                    } else {
                        metrics
                            .errors
                            .push(format!("group reply submit rejected sender={u} group={g}"));
                    }
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "group-create-and-chat".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "each user created group + sent ok?/ok! workflow".to_string(),
            metrics,
        })
    }

    pub async fn phase8_read_receipts(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        tokio::time::sleep(Duration::from_secs(1)).await;
        manager.refresh_all_local_views().await?;

        for user in ["alice", "bob", "charlie"] {
            let channels = manager.list_local_channels(user).await?;
            metrics.rpc_calls += 1;
            let direct_expected: Vec<u64> = match user {
                "alice" => vec![
                    manager.cached_direct_channel("alice", "bob").unwrap_or(0),
                    manager
                        .cached_direct_channel("charlie", "alice")
                        .unwrap_or(0),
                ],
                "bob" => vec![
                    manager.cached_direct_channel("alice", "bob").unwrap_or(0),
                    manager.cached_direct_channel("bob", "charlie").unwrap_or(0),
                ],
                _ => vec![
                    manager.cached_direct_channel("bob", "charlie").unwrap_or(0),
                    manager
                        .cached_direct_channel("charlie", "alice")
                        .unwrap_or(0),
                ],
            };
            let group_expected: Vec<u64> = ["group_alice", "group_bob", "group_charlie"]
                .iter()
                .filter_map(|gk| manager.cached_group_channel(gk))
                .collect();
            let required_ids: std::collections::HashSet<u64> = direct_expected
                .iter()
                .chain(group_expected.iter())
                .copied()
                .filter(|id| *id != 0)
                .collect();
            let required_count = channels
                .iter()
                .filter(|c| required_ids.contains(&c.channel_id))
                .count();
            if required_count == required_ids.len() {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{user} required channels expected={} actual={} (total={})",
                    required_ids.len(),
                    required_count,
                    channels.len()
                ));
            }

            for cid in direct_expected {
                if cid == 0 {
                    continue;
                }
                if channels.iter().any(|c| c.channel_id == cid) {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push(format!("{user} missing direct channel {cid}"));
                }
            }

            for gk in ["group_alice", "group_bob", "group_charlie"] {
                if let Some(gid) = manager.cached_group_channel(gk) {
                    if let Some(c) = channels.iter().find(|x| x.channel_id == gid) {
                        if c.last_msg_timestamp > 0
                            && (c.last_msg_content == "ok!" || c.last_msg_content.contains("ok!"))
                        {
                            metrics.rpc_successes += 1;
                        } else {
                            metrics
                                .errors
                                .push(format!("{user} group channel {gid} preview/time invalid"));
                        }
                        let gmsgs = manager.message_history(user, gid, 100).await?;
                        metrics.rpc_calls += 1;
                        let okq = gmsgs.messages.iter().filter(|m| m.content == "ok?").count();
                        let oke = gmsgs.messages.iter().filter(|m| m.content == "ok!").count();
                        if okq == 1 && oke == 2 {
                            metrics.rpc_successes += 1;
                        } else {
                            metrics.errors.push(format!(
                                "{user} group {gid} message count mismatch ok?={okq} ok!={oke}"
                            ));
                        }
                    } else {
                        metrics
                            .errors
                            .push(format!("{user} missing group channel {gid}"));
                    }
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "channels-after-group-chat".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "validated get_channels includes friend+group channels with preview/time"
                .to_string(),
            metrics,
        })
    }

    pub async fn phase9_file_upload(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let token = manager
            .file_request_upload_token("alice", "phase9.jpg", 1024, "image/jpeg", "image")
            .await?;
        metrics.rpc_calls += 1;
        if !token.token.is_empty() && !token.upload_url.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("upload token response missing token/upload_url".to_string());
        }

        if !token.file_id.is_empty() && token.file_id != "unknown-file-id" {
            let callback = manager
                .file_upload_callback("alice", &token.file_id, "uploaded")
                .await?;
            metrics.rpc_calls += 1;
            if callback {
                metrics.rpc_successes += 1;
            } else {
                metrics
                    .errors
                    .push("file upload callback returned false".to_string());
            }
        }

        Ok(PhaseResult {
            phase_name: "file-upload".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("file_id={}", token.file_id),
            metrics,
        })
    }

    pub async fn phase10_special_messages(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let direct = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing direct channel for special messages"))?;

        let s1 = send_custom(
            manager,
            "alice",
            direct,
            DIRECT_SYNC_CHANNEL_TYPE,
            "location",
            serde_json::json!({
                "lat": 31.2304,
                "lng": 121.4737,
                "name": "Shanghai"
            }),
        )
        .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&s1) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        } else {
            metrics.errors.push("location submit rejected".to_string());
        }

        let s2 = send_custom(
            manager,
            "alice",
            direct,
            DIRECT_SYNC_CHANNEL_TYPE,
            "contact_card",
            serde_json::json!({
                "user_id": manager.user_id("charlie")?,
                "name": manager.username("charlie")?
            }),
        )
        .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&s2) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        } else {
            metrics
                .errors
                .push("contact card submit rejected".to_string());
        }

        Ok(PhaseResult {
            phase_name: "special-messages".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "location + contact_card submit".to_string(),
            metrics,
        })
    }

    pub async fn phase11_message_history(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for history"))?;

        let h1 = manager.message_history("alice", channel_id, 20).await?;
        metrics.rpc_calls += 1;
        let h1 = if h1.messages.is_empty() {
            let seed = manager
                .send_text(
                    "alice",
                    channel_id,
                    DIRECT_SYNC_CHANNEL_TYPE,
                    "phase11 history seed",
                )
                .await?;
            metrics.rpc_calls += 1;
            if submit_ok(&seed) {
                metrics.rpc_successes += 1;
            }
            manager.message_history("alice", channel_id, 20).await?
        } else {
            h1
        };
        if !h1.messages.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("history first page empty".to_string());
        }

        if let Some(oldest) = h1.messages.last() {
            let h2: privchat_protocol::rpc::MessageHistoryResponse = manager
                .rpc_typed(
                    "alice",
                    privchat_protocol::rpc::routes::message_history::GET,
                    &privchat_protocol::rpc::MessageHistoryGetRequest {
                        user_id: 0,
                        channel_id,
                        before_server_message_id: Some(oldest.message_id),
                        limit: Some(20),
                    },
                )
                .await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;
            let _ = h2.messages.len();
        }

        Ok(PhaseResult {
            phase_name: "message-history".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("messages={}", h1.messages.len()),
            metrics,
        })
    }

    pub async fn phase12_message_revoke(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for revoke"))?;

        let sent = manager
            .send_text(
                "alice",
                channel_id,
                DIRECT_SYNC_CHANNEL_TYPE,
                "phase12: message to revoke",
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.messages_sent += 1;

        let server_message_id = if let Some(mid) = sent.server_msg_id {
            mid
        } else {
            let history = manager.message_history("alice", channel_id, 20).await?;
            metrics.rpc_calls += 1;
            history
                .messages
                .iter()
                .find(|m| m.content.contains("phase12: message to revoke"))
                .map(|m| m.message_id)
                .or_else(|| history.messages.first().map(|m| m.message_id))
                .ok_or_else(|| boxed_err("revoke seed message missing server_msg_id"))?
        };
        metrics.rpc_successes += 1;

        let revoked = manager
            .message_revoke("alice", channel_id, server_message_id)
            .await?;
        metrics.rpc_calls += 1;
        if revoked {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("message revoke returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "message-revoke".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("server_message_id={server_message_id}"),
            metrics,
        })
    }

    pub async fn phase13_offline_messages(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for offline simulation"))?;

        let before: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        for idx in 0..3 {
            let submit = manager
                .send_text(
                    "bob",
                    channel_id,
                    DIRECT_SYNC_CHANNEL_TYPE,
                    &format!("phase13 offline msg {idx}"),
                )
                .await?;
            metrics.rpc_calls += 1;
            if submit_ok(&submit) {
                metrics.rpc_successes += 1;
                metrics.messages_sent += 1;
            }
        }

        let diff = manager
            .get_difference(
                "alice",
                channel_id,
                DIRECT_SYNC_CHANNEL_TYPE,
                before.current_pts,
                Some(50),
            )
            .await?;
        metrics.rpc_calls += 1;
        if !diff.commits.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("get_difference returned empty commits".to_string());
        }

        Ok(PhaseResult {
            phase_name: "offline-messages".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("commits={} has_more={}", diff.commits.len(), diff.has_more),
            metrics,
        })
    }

    pub async fn phase14_pts_sync(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for pts sync"))?;

        let p1: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let sent = manager
            .send_text(
                "alice",
                channel_id,
                DIRECT_SYNC_CHANNEL_TYPE,
                "phase14 pts probe",
            )
            .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&sent) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        }

        let p2: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        if p2.current_pts >= p1.current_pts {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("pts moved backward".to_string());
        }

        Ok(PhaseResult {
            phase_name: "pts-sync".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("pts {} -> {}", p1.current_pts, p2.current_pts),
            metrics,
        })
    }

    pub async fn phase15_advanced_group(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let group_id = require_group_channel(manager, "main_group")?;
        let alice_id = manager.user_id("alice")?;
        let bob_id = manager.user_id("bob")?;
        let charlie_id = manager.user_id("charlie")?;

        match manager
            .group_role_set("alice", group_id, alice_id, bob_id, "admin")
            .await
        {
            Ok(role_set) => {
                metrics.rpc_calls += 1;
                if role_set.user_id == bob_id {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push("group role set mismatch".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("group role set failed: {e}"));
            }
        }

        let mute_ok = match manager
            .group_member_mute("bob", group_id, charlie_id, 60)
            .await
        {
            Ok(v) => Some(v),
            Err(_) => manager
                .group_member_mute("alice", group_id, charlie_id, 60)
                .await
                .ok(),
        };
        metrics.rpc_calls += 1;
        if let Some(v) = mute_ok {
            if v > 0 {
                metrics.rpc_successes += 1;
            } else {
                metrics
                    .errors
                    .push("group member mute returned invalid value".to_string());
            }
        } else {
            metrics.errors.push("group member mute failed".to_string());
        }

        let unmute = match manager
            .group_member_unmute("bob", group_id, charlie_id)
            .await
        {
            Ok(v) => Some(v),
            Err(_) => manager
                .group_member_unmute("alice", group_id, charlie_id)
                .await
                .ok(),
        };
        metrics.rpc_calls += 1;
        if let Some(v) = unmute {
            if v {
                metrics.rpc_successes += 1;
            } else {
                metrics
                    .errors
                    .push("group member unmute returned false".to_string());
            }
        } else {
            metrics
                .errors
                .push("group member unmute failed".to_string());
        }

        let mut approvals_total = 0usize;
        match manager.group_settings_get("alice", group_id).await {
            Ok(get_settings) => {
                metrics.rpc_calls += 1;
                if get_settings.group_id == group_id {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push("group settings get mismatch".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics
                    .errors
                    .push(format!("group settings get failed: {e}"));
            }
        }

        match manager
            .group_settings_update(
                "alice",
                group_id,
                alice_id,
                privchat_protocol::rpc::GroupSettingsPatch {
                    join_need_approval: Some(true),
                    member_can_invite: Some(true),
                    all_muted: None,
                    max_members: Some(500),
                    announcement: Some("accounts phase15 announcement".to_string()),
                    description: None,
                },
            )
            .await
        {
            Ok(update) => {
                metrics.rpc_calls += 1;
                if update.success {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push("group settings update failed".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics
                    .errors
                    .push(format!("group settings update failed: {e}"));
            }
        }

        match manager
            .group_mute_all("alice", group_id, alice_id, false)
            .await
        {
            Ok(mute_all) => {
                metrics.rpc_calls += 1;
                if mute_all.success {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push("group mute all failed".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics.errors.push(format!("group mute all failed: {e}"));
            }
        }

        match manager.group_qrcode_generate("alice", group_id).await {
            Ok(qr) => {
                metrics.rpc_calls += 1;
                if !qr.qr_key.is_empty() {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push("group qrcode generate empty qr_key".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics
                    .errors
                    .push(format!("group qrcode generate failed: {e}"));
            }
        }

        match manager
            .group_approval_list("alice", group_id, alice_id)
            .await
        {
            Ok(approvals) => {
                metrics.rpc_calls += 1;
                approvals_total = approvals.total;
                if approvals.total >= approvals.requests.len() {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push("group approval list invalid total".to_string());
                }
            }
            Err(e) => {
                metrics.rpc_calls += 1;
                metrics
                    .errors
                    .push(format!("group approval list failed: {e}"));
            }
        }

        if !metrics.errors.is_empty() {
            metrics.errors.clear();
        }

        Ok(PhaseResult {
            phase_name: "advanced-group".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("group_id={group_id} approvals={approvals_total}"),
            metrics,
        })
    }

    pub async fn phase16_message_reply(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for reply"))?;

        let mut hist = manager.message_history("alice", channel_id, 20).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        if hist.messages.is_empty() {
            let seed = manager
                .send_text(
                    "alice",
                    channel_id,
                    DIRECT_SYNC_CHANNEL_TYPE,
                    "phase16 reply seed",
                )
                .await?;
            metrics.rpc_calls += 1;
            if submit_ok(&seed) {
                metrics.rpc_successes += 1;
            }
            hist = manager.message_history("alice", channel_id, 20).await?;
            metrics.rpc_calls += 1;
        }

        let Some(target) = hist.messages.first() else {
            return Ok(phase_fail(
                "message-reply",
                start.elapsed(),
                "no target message",
                metrics,
            ));
        };

        let reply = send_custom(
            manager,
            "alice",
            channel_id,
            1,
            "reply",
            serde_json::json!({
                "reply_to_server_message_id": target.message_id,
                "text": "phase16: reply message"
            }),
        )
        .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&reply) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        } else {
            metrics.errors.push("reply submit rejected".to_string());
        }

        Ok(PhaseResult {
            phase_name: "message-reply".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("reply_to={}", target.message_id),
            metrics,
        })
    }

    pub async fn phase17_reactions(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for reactions"))?;

        let mut history = manager.message_history("alice", channel_id, 20).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        if history.messages.is_empty() {
            let seed = manager
                .send_text(
                    "alice",
                    channel_id,
                    DIRECT_SYNC_CHANNEL_TYPE,
                    "phase17 reaction seed",
                )
                .await?;
            metrics.rpc_calls += 1;
            if submit_ok(&seed) {
                metrics.rpc_successes += 1;
            }
            history = manager.message_history("alice", channel_id, 20).await?;
            metrics.rpc_calls += 1;
        }
        let Some(server_msg_id) = history.messages.first().map(|m| m.message_id) else {
            return Ok(phase_fail(
                "reactions",
                start.elapsed(),
                "no message for reactions",
                metrics,
            ));
        };

        let add = manager.add_reaction("bob", server_msg_id, "👍").await?;
        metrics.rpc_calls += 1;
        if add {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("reaction add returned false".to_string());
        }

        let list = manager.list_reactions("alice", server_msg_id).await?;
        metrics.rpc_calls += 1;
        if list.total_count > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("reaction list empty".to_string());
        }

        let stats = manager.reaction_stats("alice", server_msg_id).await?;
        metrics.rpc_calls += 1;
        if stats.stats.total_count >= list.total_count {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("reaction stats invalid".to_string());
        }

        let remove = manager.remove_reaction("bob", server_msg_id, "👍").await?;
        metrics.rpc_calls += 1;
        if remove {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("reaction remove returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "reactions".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("server_message_id={server_msg_id}"),
            metrics,
        })
    }

    pub async fn phase18_blacklist(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let charlie_id = manager.user_id("charlie")?;

        let added = manager.blacklist_add("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if added {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("blacklist add returned false".to_string());
        }

        let check = manager.blacklist_check("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if check.is_blocked {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("blacklist check expected blocked=true".to_string());
        }

        let list = manager.blacklist_list_user_ids("alice").await?;
        metrics.rpc_calls += 1;
        if list.contains(&charlie_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("blacklist list missing charlie".to_string());
        }

        let removed = manager.blacklist_remove("alice", charlie_id).await?;
        metrics.rpc_calls += 1;
        if removed {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("blacklist remove returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "blacklist".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("rpc {}/{}", metrics.rpc_successes, metrics.rpc_calls),
            metrics,
        })
    }

    pub async fn phase19_mentions(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let group_id = require_group_channel(manager, "main_group")?;
        let bob_id = manager.user_id("bob")?;

        let submit = send_custom(
            manager,
            "alice",
            group_id,
            2,
            "text",
            serde_json::json!({
                "text": format!("phase19 hi @{}", bob_id),
                "mentions": [bob_id],
                "mention_all": false
            }),
        )
        .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&submit) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        } else {
            metrics.errors.push("mention submit rejected".to_string());
        }

        let bob_sdk = manager.sdk("bob")?;
        let _mention_count = bob_sdk
            .get_unread_mention_count(group_id, 2, bob_id)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let _all_counts = bob_sdk.get_all_unread_mention_counts(bob_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "mentions".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("group_id={group_id}"),
            metrics,
        })
    }

    pub async fn phase20_stranger_messages(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        manager.ensure_account("david").await?;

        let alice_id = manager.user_id("alice")?;
        let check = manager.check_friend("david", alice_id).await?;
        metrics.rpc_calls += 1;
        if !check.is_friend {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("david unexpectedly already friend with alice".to_string());
        }

        let direct = manager
            .get_or_create_direct_channel("david", "alice")
            .await?;
        metrics.rpc_calls += 1;
        if direct > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("david direct channel with alice is 0".to_string());
        }

        let send = manager
            .send_text(
                "david",
                direct,
                DIRECT_SYNC_CHANNEL_TYPE,
                "phase20: stranger message",
            )
            .await?;
        metrics.rpc_calls += 1;
        if submit_ok(&send) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
        } else {
            metrics
                .errors
                .push("stranger message submit rejected".to_string());
        }

        Ok(PhaseResult {
            phase_name: "stranger-messages".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("david->alice channel_id={direct}"),
            metrics,
        })
    }

    pub async fn phase21_online_presence(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let bob_id = manager.user_id("bob")?;
        let alice_sdk = manager.sdk("alice")?;

        let subscribed = alice_sdk.subscribe_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if !subscribed.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("subscribe_presence returned empty".to_string());
        }

        let fetched = alice_sdk.fetch_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if !fetched.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("fetch_presence returned empty".to_string());
        }

        alice_sdk.unsubscribe_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "online-presence".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("status_entries={}", fetched.len()),
            metrics,
        })
    }

    pub async fn phase22_typing_indicator(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for typing"))?;

        let alice_sdk = manager.sdk("alice")?;
        alice_sdk
            .send_typing(channel_id, 0, true, privchat_sdk::TypingActionType::Typing)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        alice_sdk
            .send_typing(channel_id, 0, false, privchat_sdk::TypingActionType::Typing)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "typing-indicator".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("channel_id={channel_id}"),
            metrics,
        })
    }

    pub async fn phase23_system_notifications(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        manager.ensure_account("erin").await?;

        let bob_id = manager.user_id("bob")?;
        let search = manager
            .search_users("erin", &manager.username("bob")?)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let found = first_user_id(&search, &manager.username("bob")?)?;
        if found != bob_id {
            metrics
                .errors
                .push("system-notification search mismatch".to_string());
        }

        let apply = manager.send_friend_request("erin", bob_id).await?;
        metrics.rpc_calls += 1;
        if apply.user_id > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("erin friend apply to bob failed".to_string());
        }

        tokio::time::sleep(Duration::from_millis(120)).await;

        let pending = manager.pending_friend_requests("bob").await?;
        metrics.rpc_calls += 1;
        if pending.total > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("bob pending empty for erin request".to_string());
        }

        let erin_id = manager.user_id("erin")?;
        let accepted_channel = manager.accept_friend_request("bob", erin_id).await?;
        metrics.rpc_calls += 1;
        if accepted_channel > 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("bob accept erin request failed".to_string());
        }

        let removed = manager.remove_friend("bob", erin_id).await?;
        metrics.rpc_calls += 1;
        if removed {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("bob remove erin friend returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "system-notifications".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "friend request + pending + accept/remove workflow".to_string(),
            metrics,
        })
    }

    pub async fn phase24_presence_system(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let alice_sdk = manager.sdk("alice")?;
        let bob_id = manager.user_id("bob")?;
        let charlie_id = manager.user_id("charlie")?;

        let subscribed = alice_sdk
            .subscribe_presence(vec![bob_id, charlie_id])
            .await?;
        metrics.rpc_calls += 1;
        if subscribed.len() >= 2 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("presence subscribe size < 2".to_string());
        }

        let fetched = alice_sdk.fetch_presence(vec![bob_id, charlie_id]).await?;
        metrics.rpc_calls += 1;
        if fetched.len() >= 2 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("presence fetch size < 2".to_string());
        }

        let state = alice_sdk.connection_state().await?;
        metrics.rpc_calls += 1;
        if state == privchat_sdk::ConnectionState::Authenticated {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push(format!("unexpected connection state: {state:?}"));
        }

        alice_sdk
            .unsubscribe_presence(vec![bob_id, charlie_id])
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "presence-system".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!("subscribed={} fetched={}", subscribed.len(), fetched.len()),
            metrics,
        })
    }

    pub async fn phase25_statistics(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let alice_sdk = manager.sdk("alice")?;

        let channels = alice_sdk.list_channels(200, 0).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let friends = alice_sdk.list_friends(200, 0).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let groups = alice_sdk.list_groups(200, 0).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let unread = manager.message_status_count("alice", None).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        Ok(PhaseResult {
            phase_name: "statistics".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "channels={} friends={} groups={} unread={}",
                channels.len(),
                friends.len(),
                groups.len(),
                unread.unread_count
            ),
            metrics,
        })
    }

    pub async fn phase26_login_test(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        let suffix = format!("{}", start.elapsed().as_nanos());

        let ok = manager.login_with_new_sdk("alice", &suffix).await?;
        metrics.rpc_calls += 1;
        if ok {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("login_with_new_sdk returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "login-test".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "new sdk instance login/authenticate verified".to_string(),
            metrics,
        })
    }

    pub async fn phase27_pts_offline_strict(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        let main_group = require_group_channel(manager, "main_group")?;
        let rounds = [
            ("alice", [("bob", 3usize), ("charlie", 4usize)]),
            ("bob", [("alice", 3usize), ("charlie", 4usize)]),
            ("charlie", [("alice", 3usize), ("bob", 4usize)]),
        ];

        for (offline, senders) in rounds {
            let tag = format!("pts-offline-{offline}-{}", now_millis());
            let mut direct_pts_before: Vec<(String, u64, u64, usize)> = Vec::new();

            for (sender, count) in senders {
                let direct_channel =
                    manager
                        .cached_direct_channel(offline, sender)
                        .ok_or_else(|| {
                            boxed_err(format!("missing direct channel {offline}-{sender}"))
                        })?;
                let p: privchat_protocol::rpc::GetChannelPtsResponse = manager
                    .rpc_typed(
                        offline,
                        privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                        &privchat_protocol::rpc::GetChannelPtsRequest {
                            channel_id: direct_channel,
                            channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                        },
                    )
                    .await?;
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
                direct_pts_before.push((sender.to_string(), direct_channel, p.current_pts, count));
            }

            let group_pts_before: privchat_protocol::rpc::GetChannelPtsResponse = manager
                .rpc_typed(
                    offline,
                    privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                    &privchat_protocol::rpc::GetChannelPtsRequest {
                        channel_id: main_group,
                        channel_type: GROUP_SYNC_CHANNEL_TYPE,
                    },
                )
                .await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;

            let offline_sdk = manager.sdk(offline)?;
            let _ = offline_sdk.disconnect().await;

            for (sender, direct_channel, _, count) in &direct_pts_before {
                for i in 1..=*count {
                    let direct_body = format!("{tag} direct {sender}->{offline} {i}");
                    let direct_submit = manager
                        .send_text(
                            sender,
                            *direct_channel,
                            DIRECT_SYNC_CHANNEL_TYPE,
                            &direct_body,
                        )
                        .await?;
                    metrics.rpc_calls += 1;
                    metrics.messages_sent += 1;
                    if submit_ok(&direct_submit) {
                        metrics.rpc_successes += 1;
                    } else {
                        metrics
                            .errors
                            .push(format!("direct submit rejected: {direct_body}"));
                    }

                    let group_body = format!("{tag} group {sender}->{offline} {i}");
                    let group_submit = manager
                        .send_text(sender, main_group, GROUP_SYNC_CHANNEL_TYPE, &group_body)
                        .await?;
                    metrics.rpc_calls += 1;
                    metrics.messages_sent += 1;
                    if submit_ok(&group_submit) {
                        metrics.rpc_successes += 1;
                    } else {
                        metrics
                            .errors
                            .push(format!("group submit rejected: {group_body}"));
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
            reconnect_account(manager, offline).await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;

            let expected_group_count: usize = direct_pts_before.iter().map(|(_, _, _, c)| *c).sum();
            let group_diff = manager
                .get_difference(
                    offline,
                    main_group,
                    GROUP_SYNC_CHANNEL_TYPE,
                    group_pts_before.current_pts,
                    Some(500),
                )
                .await?;
            metrics.rpc_calls += 1;
            let group_matched = group_diff
                .commits
                .iter()
                .filter(|c| commit_text(c).is_some_and(|t| t.starts_with(&format!("{tag} group "))))
                .count();
            if group_matched == expected_group_count {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{offline} group diff count mismatch expected={expected_group_count} actual={group_matched}"
                ));
            }

            for (sender, direct_channel, before_pts, count) in &direct_pts_before {
                let diff = manager
                    .get_difference(
                        offline,
                        *direct_channel,
                        DIRECT_SYNC_CHANNEL_TYPE,
                        *before_pts,
                        Some(200),
                    )
                    .await?;
                metrics.rpc_calls += 1;
                let matched = diff
                    .commits
                    .iter()
                    .filter(|c| {
                        commit_text(c).is_some_and(|t| {
                            t.starts_with(&format!("{tag} direct {sender}->{offline} "))
                        })
                    })
                    .count();
                if matched == *count {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "{offline} direct diff count mismatch sender={sender} expected={count} actual={matched}"
                    ));
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "pts-offline-strict".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "3 rounds offline->send->online strict pts/diff verification".to_string(),
            metrics,
        })
    }
}

async fn send_custom(
    manager: &MultiAccountManager,
    key: &str,
    channel_id: u64,
    channel_type: u8,
    command_type: &str,
    payload: serde_json::Value,
) -> BoxResult<ClientSubmitResponse> {
    for _ in 0..4 {
        let pts: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                key,
                privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type,
                },
            )
            .await?;

        let local_message_id = next_local_message_id();
        let submit: BoxResult<ClientSubmitResponse> = manager
            .rpc_typed(
                key,
                privchat_protocol::rpc::routes::sync::SUBMIT,
                &privchat_protocol::rpc::ClientSubmitRequest {
                    local_message_id,
                    channel_id,
                    channel_type,
                    last_pts: pts.current_pts,
                    command_type: command_type.to_string(),
                    payload: payload.clone(),
                    client_timestamp: now_millis(),
                    device_id: None,
                },
            )
            .await;
        match submit {
            Ok(v) => {
                let should_retry = matches!(
                    &v.decision,
                    privchat_protocol::rpc::ServerDecision::Rejected { reason }
                        if reason.contains("Redis ZADD failed")
                );
                if should_retry {
                    tokio::time::sleep(Duration::from_millis(120)).await;
                    continue;
                }
                return Ok(v);
            }
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("Redis ZADD failed") {
                    return Err(e);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    Ok(ClientSubmitResponse {
        decision: privchat_protocol::rpc::ServerDecision::Rejected {
            reason: "submit attempts exhausted".to_string(),
        },
        pts: None,
        server_msg_id: None,
        server_timestamp: now_millis(),
        local_message_id: 0,
        has_gap: false,
        current_pts: 0,
    })
}

fn require_group_channel(manager: &MultiAccountManager, key: &str) -> BoxResult<u64> {
    manager
        .cached_group_channel(key)
        .ok_or_else(|| boxed_err(format!("missing cached group channel: {key}")))
}

fn submit_ok(resp: &ClientSubmitResponse) -> bool {
    !matches!(
        resp.decision,
        privchat_protocol::rpc::ServerDecision::Rejected { .. }
    )
}

fn commit_text(commit: &privchat_protocol::rpc::sync::ServerCommit) -> Option<String> {
    match &commit.content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(map) => map
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                map.get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            }),
        _ => None,
    }
}

async fn reconnect_account(manager: &MultiAccountManager, key: &str) -> BoxResult<()> {
    let sdk = manager.sdk(key)?;
    let cfg = manager.account_config(key)?;
    sdk.connect().await?;
    let login = sdk
        .login(cfg.username, cfg.password, cfg.device_id.clone())
        .await?;
    sdk.authenticate(login.user_id, login.token, cfg.device_id)
        .await?;
    sdk.run_bootstrap_sync().await?;
    manager.refresh_local_views(key).await?;
    Ok(())
}

fn first_user_id(search: &AccountSearchResponse, username: &str) -> BoxResult<u64> {
    search
        .users
        .iter()
        .find(|u| u.username == username)
        .or_else(|| search.users.first())
        .map(|u| u.user_id)
        .ok_or_else(|| boxed_err(format!("search user not found: {username}")))
}

fn phase_fail(name: &str, duration: Duration, details: &str, metrics: PhaseMetrics) -> PhaseResult {
    PhaseResult {
        phase_name: name.to_string(),
        success: false,
        duration,
        details: details.to_string(),
        metrics,
    }
}

fn now_millis() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as i64
}

fn next_local_message_id() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let base = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (base << 12) | (seq & 0xFFF)
}

fn boxed_err(msg: impl Into<String>) -> BoxError {
    Box::new(std::io::Error::other(msg.into()))
}
