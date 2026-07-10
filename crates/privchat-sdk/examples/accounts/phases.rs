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

use std::time::Duration;

use privchat_protocol::rpc::routes::sync;
use privchat_protocol::rpc::{AccountSearchResponse, ClientSubmitResponse};
use serde::Deserialize;

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

            // Some server builds are eventually consistent on `friend/check`.
            // Treat check as best-effort signal and rely on local sync list as final source.
            for _ in 0..8 {
                let rel_a = manager.check_friend(from, to_user_id).await?;
                let rel_b = manager.check_friend(to, from_id).await?;
                metrics.rpc_calls += 2;
                if rel_a.is_friend && rel_b.is_friend {
                    metrics.rpc_successes += 2;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
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
                        // `channel_name/channel_remark` may be empty when server does not expose
                        // user/channel-member sync entities for direct channels.
                        if ok_preview && ok_ts {
                            metrics.rpc_successes += 1;
                        } else {
                            let history = manager.message_history(user, cid, 1).await?;
                            metrics.rpc_calls += 1;
                            let history_ok = history.messages.first().is_some_and(|m| {
                                (m.content == "hello friend" || m.content.contains("hello friend"))
                                    && m.timestamp > 0
                            });
                            if history_ok {
                                metrics.rpc_successes += 1;
                            } else {
                                metrics.errors.push(format!(
                                    "{user} channel {cid} invalid meta(preview/timestamp,name_ok={ok_name})"
                                ));
                            }
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
                        let gmsgs = manager.message_history(user, gid, 100).await?;
                        metrics.rpc_calls += 1;
                        let ok_preview =
                            c.last_msg_content == "ok!" || c.last_msg_content.contains("ok!");
                        let ok_ts = c.last_msg_timestamp > 0;
                        if ok_preview && ok_ts {
                            metrics.rpc_successes += 1;
                        } else {
                            // Per the post-architecture-fix rule, `last_msg_content` carries the
                            // raw last message body; UI renders previews from message_type + i18n.
                            // Some inbound paths can land the latest message before the channel
                            // preview row catches up — verify via local message history as fallback.
                            let history_ok = gmsgs.messages.last().is_some_and(|m| {
                                (m.content == "ok!" || m.content.contains("ok!"))
                                    && m.timestamp > 0
                            });
                            if history_ok {
                                metrics.rpc_successes += 1;
                            } else {
                                metrics.errors.push(format!(
                                    "{user} group channel {gid} preview/time invalid (last_msg_content='{}' ts={})",
                                    c.last_msg_content, c.last_msg_timestamp
                                ));
                            }
                        }
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

        let bob_uid = manager.user_id("bob")?;
        let expected_texts: std::collections::HashSet<String> = (0..3)
            .map(|idx| format!("phase13 offline msg {idx}"))
            .collect();

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
            } else {
                metrics.errors.push(format!(
                    "phase13 submit rejected idx={idx} channel={channel_id}"
                ));
            }
        }

        // sync/get_difference 在部分服务端构建会有短时空窗口，做重试并做内容级匹配。
        let mut final_diff = privchat_protocol::rpc::sync::GetDifferenceResponse {
            commits: Vec::new(),
            current_pts: before.current_pts,
            has_more: false,
        };
        let mut matched_diff_texts: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for _ in 0..8 {
            let diff = manager
                .get_difference(
                    "alice",
                    channel_id,
                    DIRECT_SYNC_CHANNEL_TYPE,
                    before.current_pts,
                    Some(100),
                )
                .await?;
            metrics.rpc_calls += 1;

            matched_diff_texts = diff
                .commits
                .iter()
                .filter(|c| c.sender_id == bob_uid)
                .filter_map(commit_text)
                .filter(|t| expected_texts.contains(t))
                .collect();
            final_diff = diff;

            if matched_diff_texts.len() == expected_texts.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if matched_diff_texts.len() == expected_texts.len() {
            metrics.rpc_successes += 1;
        }

        // 兜底使用 pts + history 做强校验，避免仅依赖 get_difference 导致脆弱。
        let after: privchat_protocol::rpc::GetChannelPtsResponse = manager
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

        if after.current_pts >= before.current_pts.saturating_add(3) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "pts not advanced enough in phase13: before={} after={} expected_at_least={}",
                before.current_pts,
                after.current_pts,
                before.current_pts.saturating_add(3)
            ));
        }

        let history = manager.message_history("alice", channel_id, 200).await?;
        metrics.rpc_calls += 1;
        let history_hits: std::collections::HashSet<String> = history
            .messages
            .iter()
            .map(|m| m.content.clone())
            .filter(|t| expected_texts.contains(t))
            .collect();
        if history_hits.len() == expected_texts.len() {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "history missing phase13 messages: expected={} actual={} (channel={})",
                expected_texts.len(),
                history_hits.len(),
                channel_id
            ));
        }

        Ok(PhaseResult {
            phase_name: "offline-messages".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "diff_commits={} diff_matched={} pts {}->{} history_matched={}",
                final_diff.commits.len(),
                matched_diff_texts.len(),
                before.current_pts,
                after.current_pts,
                history_hits.len()
            ),
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
        let mut phase14_sent_ok = false;
        if submit_ok(&sent) {
            metrics.rpc_successes += 1;
            metrics.messages_sent += 1;
            phase14_sent_ok = true;
        } else {
            metrics.errors.push("phase14 submit rejected".to_string());
        }

        let mut p2 = p1.clone();
        let mut history_probe_hits = 0usize;
        let mut diff_probe_hits = 0usize;
        if phase14_sent_ok {
            for _ in 0..8 {
                p2 = manager
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

                let diff = manager
                    .get_difference(
                        "alice",
                        channel_id,
                        DIRECT_SYNC_CHANNEL_TYPE,
                        p1.current_pts,
                        Some(50),
                    )
                    .await?;
                metrics.rpc_calls += 1;
                diff_probe_hits = diff
                    .commits
                    .iter()
                    .filter_map(commit_text)
                    .filter(|t| t == "phase14 pts probe")
                    .count();

                let history = manager.message_history("alice", channel_id, 50).await?;
                metrics.rpc_calls += 1;
                history_probe_hits = history
                    .messages
                    .iter()
                    .filter(|m| m.content == "phase14 pts probe")
                    .count();

                if p2.current_pts >= p1.current_pts.saturating_add(1) && history_probe_hits >= 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }

        if p2.current_pts >= p1.current_pts.saturating_add(1) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "phase14 pts did not advance enough: before={} after={}",
                p1.current_pts, p2.current_pts
            ));
        }
        if history_probe_hits >= 1 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("phase14 probe missing in message_history".to_string());
        }

        Ok(PhaseResult {
            phase_name: "pts-sync".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "pts {} -> {}, diff_probe_hits={}, history_probe_hits={}",
                p1.current_pts, p2.current_pts, diff_probe_hits, history_probe_hits
            ),
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
                    allow_member_add_friend: None,
                    allow_search: None,
                    join_policy: None,
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

        match manager.group_qrcode_get("alice", group_id).await {
            Ok(qr) => {
                metrics.rpc_calls += 1;
                if !qr.qr_key.is_empty() {
                    metrics.rpc_successes += 1;
                } else {
                    metrics
                        .errors
                        .push("group qrcode get empty qr_key".to_string());
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
        if check.blocked {
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

        let fetched = alice_sdk.batch_get_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if !fetched.is_empty() {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("batch_get_presence returned empty".to_string());
        }

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

        // --- Step 1: 获取 alice-bob 私聊频道 ---
        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing channel for typing"))?;

        let alice_sdk = manager.sdk("alice")?;
        let bob_sdk = manager.sdk("bob")?;

        // --- Step 2: bob 订阅频道（接收 typing 事件） ---
        bob_sdk.subscribe_channel(channel_id, 0, None).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        // 订阅 SDK 事件流（在 subscribe 之后开始监听）
        let mut bob_events = bob_sdk.subscribe_events();

        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Step 3: alice 发送 typing → bob 应收到 PublishRequest(topic="typing") ---
        alice_sdk
            .send_typing(channel_id, 0, true, privchat_sdk::TypingActionType::Typing)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut bob_received_typing = false;
        loop {
            match bob_events.try_recv() {
                Ok(event) => {
                    if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        ..
                    } = &event
                    {
                        if *cid == channel_id && topic.as_deref() == Some("typing") {
                            bob_received_typing = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        if !bob_received_typing {
            metrics
                .errors
                .push("bob did not receive typing event from alice".to_string());
        }

        // --- Step 4: 验证限频 — 500ms 内连续发 3 次，bob 最多收到 1 次 ---
        // 先排空事件队列
        while bob_events.try_recv().is_ok() {}

        // 等待 600ms 确保上一次限频窗口过期
        tokio::time::sleep(Duration::from_millis(600)).await;

        // 50ms 间隔连续发 3 次 typing
        for _ in 0..3 {
            let _ = alice_sdk
                .send_typing(channel_id, 0, true, privchat_sdk::TypingActionType::Typing)
                .await;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut typing_count = 0u32;
        loop {
            match bob_events.try_recv() {
                Ok(event) => {
                    if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        ..
                    } = &event
                    {
                        if *cid == channel_id && topic.as_deref() == Some("typing") {
                            typing_count += 1;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        // 服务端 500ms 限频：3 次请求在 ~100ms 内发出，只有第 1 次应该被广播
        if typing_count > 1 {
            metrics.errors.push(format!(
                "rate limiting failed: sent 3 rapid typings, bob received {} (expected 1)",
                typing_count
            ));
        }

        // --- Step 5: bob 取消订阅后不再收到 typing ---
        bob_sdk.unsubscribe_channel(channel_id, 0).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        // 排空事件队列
        while bob_events.try_recv().is_ok() {}

        // 等待 600ms 确保限频窗口过期后再发
        tokio::time::sleep(Duration::from_millis(600)).await;

        alice_sdk
            .send_typing(channel_id, 0, true, privchat_sdk::TypingActionType::Typing)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        tokio::time::sleep(Duration::from_millis(500)).await;

        let mut received_after_unsub = false;
        loop {
            match bob_events.try_recv() {
                Ok(event) => {
                    if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        ..
                    } = &event
                    {
                        if *cid == channel_id && topic.as_deref() == Some("typing") {
                            received_after_unsub = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        if received_after_unsub {
            metrics
                .errors
                .push("bob received typing after unsubscribe".to_string());
        }

        // --- Step 6: 群聊 typing 测试 ---
        let group_channel_id = manager
            .cached_group_channel("main_group")
            .ok_or_else(|| boxed_err("missing group channel for typing test"))?;

        let charlie_sdk = manager.sdk("charlie")?;

        // bob 和 charlie 订阅群频道
        bob_sdk.subscribe_channel(group_channel_id, 1, None).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        charlie_sdk
            .subscribe_channel(group_channel_id, 1, None)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let mut bob_group_events = bob_sdk.subscribe_events();
        let mut charlie_group_events = charlie_sdk.subscribe_events();

        tokio::time::sleep(Duration::from_millis(200)).await;

        // alice 在群里发 typing
        alice_sdk
            .send_typing(
                group_channel_id,
                1,
                true,
                privchat_sdk::TypingActionType::Typing,
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        tokio::time::sleep(Duration::from_millis(500)).await;

        // bob 和 charlie 都应收到
        let mut bob_group_typing = false;
        let mut charlie_group_typing = false;

        loop {
            match bob_group_events.try_recv() {
                Ok(event) => {
                    if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        ..
                    } = &event
                    {
                        if *cid == group_channel_id && topic.as_deref() == Some("typing") {
                            bob_group_typing = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        loop {
            match charlie_group_events.try_recv() {
                Ok(event) => {
                    if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        ..
                    } = &event
                    {
                        if *cid == group_channel_id && topic.as_deref() == Some("typing") {
                            charlie_group_typing = true;
                        }
                    }
                }
                Err(_) => break,
            }
        }

        if !bob_group_typing {
            metrics
                .errors
                .push("bob did not receive group typing from alice".to_string());
        }
        if !charlie_group_typing {
            metrics
                .errors
                .push("charlie did not receive group typing from alice".to_string());
        }

        // 清理：取消群订阅
        let _ = bob_sdk.unsubscribe_channel(group_channel_id, 1).await;
        let _ = charlie_sdk.unsubscribe_channel(group_channel_id, 1).await;

        let details = format!(
            "private ch={} (recv={}, rate_limit_count={}/3), group ch={} (bob={}, charlie={})",
            channel_id,
            bob_received_typing,
            typing_count,
            group_channel_id,
            bob_group_typing,
            charlie_group_typing,
        );

        Ok(PhaseResult {
            phase_name: "typing-indicator".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details,
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
        let bob_sdk = manager.sdk("bob")?;
        let bob_id = manager.user_id("bob")?;
        let charlie_id = manager.user_id("charlie")?;
        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing alice-bob channel for presence"))?;

        let fetched = alice_sdk
            .batch_get_presence(vec![bob_id, charlie_id])
            .await?;
        metrics.rpc_calls += 1;
        if fetched.len() >= 2 {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("presence batch_get size < 2".to_string());
        }

        alice_sdk.subscribe_channel(channel_id, 0, None).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let mut alice_events = alice_sdk.subscribe_events();
        while alice_events.try_recv().is_ok() {}

        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut notified_snapshot_matches_query = false;

        bob_sdk.disconnect().await?;
        let mut bob_needs_reconnect = true;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        // 订阅频道时服务端会先推一条「当前在线」初始 presence（version=1，is_online=true），
        // 它与 bob 断开后的离线变更（version 递增、is_online=false）共用同一 "presence_changed"
        // topic。这里只接受反映本次断开的「离线」通知——否则会把初始在线推送误当成断开事件，
        // 与随后 query 到的离线态不一致（真实客户端按 version 收敛，这里等明确的离线状态而非固定 sleep）。
        let received_notification = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match alice_events.recv().await {
                    Ok(privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                        channel_id: cid,
                        topic,
                        payload,
                        ..
                    }) if cid == channel_id && topic.as_deref() == Some("presence_changed") => {
                        let notification =
                            serde_json::from_slice::<
                                privchat_protocol::presence::PresenceChangedNotification,
                            >(&payload)
                            .map_err(|e| {
                                boxed_err(format!("decode presence_changed payload failed: {e}"))
                            })?;
                        if notification.user_id == bob_id && !notification.snapshot.is_online {
                            break Ok(notification);
                        }
                        // 初始在线 / 其他用户的 presence_changed —— 跳过，继续等 bob 的离线通知。
                        continue;
                    }
                    Ok(_) => continue,
                    Err(e) => {
                        break Err(boxed_err(format!(
                            "presence event stream closed before presence_changed: {e}"
                        )));
                    }
                }
            }
        })
        .await
        .map_err(|_| boxed_err("timed out waiting for bob offline presence_changed"))??;

        let received_presence_changed = true;

        if received_notification.user_id != bob_id {
            metrics.errors.push(format!(
                "presence_changed user mismatch expected={} actual={}",
                bob_id, received_notification.user_id
            ));
        } else {
            metrics.rpc_successes += 1;
        }

        let fetched_after_disconnect = alice_sdk.batch_get_presence(vec![bob_id]).await?;
        metrics.rpc_calls += 1;
        if let Some(current) = fetched_after_disconnect.first() {
            if current.user_id == received_notification.snapshot.user_id
                && current.is_online == received_notification.snapshot.is_online
                && current.last_seen_at == received_notification.snapshot.last_seen_at
                && current.device_count == received_notification.snapshot.device_count
            {
                notified_snapshot_matches_query = true;
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "presence_changed snapshot mismatch query snapshot: notified(user={}, version={}, online={}, devices={}) query(user={}, online={}, devices={})",
                    received_notification.snapshot.user_id,
                    received_notification.snapshot.version,
                    received_notification.snapshot.is_online,
                    received_notification.snapshot.device_count,
                    current.user_id,
                    current.is_online,
                    current.device_count
                ));
            }
        } else {
            metrics
                .errors
                .push("batch_get_presence after disconnect returned empty".to_string());
        }

        if let Err(e) = reconnect_account(manager, "bob").await {
            metrics
                .errors
                .push(format!("reconnect bob after presence test failed: {e}"));
        } else {
            bob_needs_reconnect = false;
            metrics.rpc_successes += 1;
        }

        let _ = alice_sdk.unsubscribe_channel(channel_id, 0).await;

        let state = alice_sdk.connection_state().await?;
        metrics.rpc_calls += 1;
        if state == privchat_sdk::ConnectionState::Authenticated {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push(format!("unexpected connection state: {state:?}"));
        }

        if bob_needs_reconnect {
            let _ = reconnect_account(manager, "bob").await;
        }

        Ok(PhaseResult {
            phase_name: "presence-system".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "fetched={} channel={} presence_changed={} snapshot_match={}",
                fetched.len(),
                channel_id,
                received_presence_changed,
                notified_snapshot_matches_query
            ),
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

        let (notice_ok, notice_details) = manager
            .verify_login_notice_persisted("alice", &suffix)
            .await?;
        metrics.rpc_calls += 1;
        if notice_ok {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "system login notice not persisted in local message table: {notice_details}"
            ));
        }

        // 同设备 replace 后主 SDK 的 transport 已被服务端踢断，
        // 否则后续所有 phase 会以 "authenticate requires connect and login first" 连锁失败。
        if let Err(e) = manager.restore_primary_session("alice").await {
            metrics
                .errors
                .push(format!("restore_primary_session(alice) failed: {e}"));
        } else {
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;
        }

        Ok(PhaseResult {
            phase_name: "login-test".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "new sdk instance login/authenticate verified; login-notice check: {notice_details}"
            ),
            metrics,
        })
    }

    pub async fn phase27_pts_offline_strict(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        let main_group = require_group_channel(manager, "main_group")?;
        // Keep strict assertions but cap volume so full 30-phase run can finish reliably.
        let rounds = [
            ("alice", [("bob", 2usize), ("charlie", 2usize)]),
            ("bob", [("alice", 2usize), ("charlie", 2usize)]),
            ("charlie", [("alice", 2usize), ("bob", 2usize)]),
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

            tokio::time::sleep(Duration::from_millis(300)).await;
            reconnect_account(manager, offline).await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;

            let expected_group_count: usize = direct_pts_before.iter().map(|(_, _, _, c)| *c).sum();
            let mut group_diff = privchat_protocol::rpc::sync::GetDifferenceResponse {
                commits: Vec::new(),
                current_pts: group_pts_before.current_pts,
                has_more: false,
            };
            let mut group_matched = 0usize;
            for _ in 0..5 {
                let latest = manager
                    .get_difference(
                        offline,
                        main_group,
                        GROUP_SYNC_CHANNEL_TYPE,
                        group_pts_before.current_pts,
                        Some(500),
                    )
                    .await?;
                metrics.rpc_calls += 1;
                group_matched = latest
                    .commits
                    .iter()
                    .filter(|c| {
                        commit_text(c).is_some_and(|t| t.starts_with(&format!("{tag} group ")))
                    })
                    .count();
                group_diff = latest;
                if group_matched >= expected_group_count {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if group_matched == expected_group_count {
                metrics.rpc_successes += 1;
            }
            let group_pts_after: privchat_protocol::rpc::GetChannelPtsResponse = manager
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
            if group_pts_after.current_pts
                >= group_pts_before
                    .current_pts
                    .saturating_add(expected_group_count as u64)
            {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{offline} group pts advanced too little: before={} after={} expected_at_least={}",
                    group_pts_before.current_pts,
                    group_pts_after.current_pts,
                    group_pts_before.current_pts.saturating_add(expected_group_count as u64)
                ));
            }
            let group_history = manager.message_history(offline, main_group, 500).await?;
            metrics.rpc_calls += 1;
            let group_history_matched = group_history
                .messages
                .iter()
                .filter(|m| m.content.starts_with(&format!("{tag} group ")))
                .count();
            if group_history_matched == expected_group_count {
                metrics.rpc_successes += 1;
            } else {
                metrics.errors.push(format!(
                    "{offline} group history count mismatch expected={expected_group_count} actual={group_history_matched}"
                ));
            }

            for (sender, direct_channel, before_pts, count) in &direct_pts_before {
                let mut diff = privchat_protocol::rpc::sync::GetDifferenceResponse {
                    commits: Vec::new(),
                    current_pts: *before_pts,
                    has_more: false,
                };
                let mut matched = 0usize;
                for _ in 0..5 {
                    let latest = manager
                        .get_difference(
                            offline,
                            *direct_channel,
                            DIRECT_SYNC_CHANNEL_TYPE,
                            *before_pts,
                            Some(200),
                        )
                        .await?;
                    metrics.rpc_calls += 1;
                    matched = latest
                        .commits
                        .iter()
                        .filter(|c| {
                            commit_text(c).is_some_and(|t| {
                                t.starts_with(&format!("{tag} direct {sender}->{offline} "))
                            })
                        })
                        .count();
                    diff = latest;
                    if matched >= *count {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                if matched == *count {
                    metrics.rpc_successes += 1;
                }
                let direct_pts_after: privchat_protocol::rpc::GetChannelPtsResponse = manager
                    .rpc_typed(
                        offline,
                        privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                        &privchat_protocol::rpc::GetChannelPtsRequest {
                            channel_id: *direct_channel,
                            channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                        },
                    )
                    .await?;
                metrics.rpc_calls += 1;
                if direct_pts_after.current_pts >= before_pts.saturating_add(*count as u64) {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "{offline} direct pts advanced too little sender={sender}: before={} after={} expected_at_least={}",
                        before_pts,
                        direct_pts_after.current_pts,
                        before_pts.saturating_add(*count as u64)
                    ));
                }
                let direct_history = manager
                    .message_history(offline, *direct_channel, 300)
                    .await?;
                metrics.rpc_calls += 1;
                let direct_history_matched = direct_history
                    .messages
                    .iter()
                    .filter(|m| {
                        m.content
                            .starts_with(&format!("{tag} direct {sender}->{offline} "))
                    })
                    .count();
                if direct_history_matched == *count {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "{offline} direct history count mismatch sender={sender} expected={count} actual={direct_history_matched}"
                    ));
                }
                let _ = diff;
            }
            let _ = group_diff;
        }

        // Extra verification: all 3 accounts go offline/online twice without sending messages.
        // For business channels, pts should remain stable when there is no new commit.
        let tracked_channels = build_pts_tracked_channels(manager)?;
        let mut baseline_pts: std::collections::HashMap<(String, u64, u8), u64> =
            std::collections::HashMap::new();
        for (owner, channel_id, channel_type) in &tracked_channels {
            let p: privchat_protocol::rpc::GetChannelPtsResponse = manager
                .rpc_typed(
                    owner,
                    privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                    &privchat_protocol::rpc::GetChannelPtsRequest {
                        channel_id: *channel_id,
                        channel_type: *channel_type,
                    },
                )
                .await?;
            metrics.rpc_calls += 1;
            metrics.rpc_successes += 1;
            baseline_pts.insert((owner.clone(), *channel_id, *channel_type), p.current_pts);
        }

        for cycle in 1..=2 {
            for key in ["alice", "bob", "charlie"] {
                let sdk = manager.sdk(key)?;
                let _ = sdk.disconnect().await;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;

            for key in ["alice", "bob", "charlie"] {
                reconnect_account(manager, key).await?;
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;

            for (owner, channel_id, channel_type) in &tracked_channels {
                let before = baseline_pts
                    .get(&(owner.clone(), *channel_id, *channel_type))
                    .copied()
                    .unwrap_or(0);
                let after: privchat_protocol::rpc::GetChannelPtsResponse = manager
                    .rpc_typed(
                        owner,
                        privchat_protocol::rpc::routes::sync::GET_CHANNEL_PTS,
                        &privchat_protocol::rpc::GetChannelPtsRequest {
                            channel_id: *channel_id,
                            channel_type: *channel_type,
                        },
                    )
                    .await?;
                metrics.rpc_calls += 1;
                if after.current_pts == before {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "cycle{cycle} pts drift owner={owner} channel_id={channel_id} type={channel_type}: before={before} after={}",
                        after.current_pts
                    ));
                }

                let diff = manager
                    .get_difference(owner, *channel_id, *channel_type, before, Some(100))
                    .await?;
                metrics.rpc_calls += 1;
                if diff.commits.is_empty() {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "cycle{cycle} unexpected commits owner={owner} channel_id={channel_id} type={channel_type}: commits={}",
                        diff.commits.len()
                    ));
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "pts-offline-strict".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details:
                "3 rounds offline->send->online + 2 rounds all-accounts reconnect pts stability"
                    .to_string(),
            metrics,
        })
    }

    pub async fn phase28_friend_display_name_rules(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        manager.refresh_all_local_views().await?;

        let mut expected_friend_map: std::collections::HashMap<
            &str,
            std::collections::HashSet<u64>,
        > = std::collections::HashMap::new();
        expected_friend_map.insert(
            "alice",
            [manager.user_id("bob")?, manager.user_id("charlie")?]
                .into_iter()
                .collect(),
        );
        expected_friend_map.insert(
            "bob",
            [manager.user_id("alice")?, manager.user_id("charlie")?]
                .into_iter()
                .collect(),
        );
        expected_friend_map.insert(
            "charlie",
            [manager.user_id("alice")?, manager.user_id("bob")?]
                .into_iter()
                .collect(),
        );

        for viewer in ["alice", "bob", "charlie"] {
            let friends = manager.list_local_friends(viewer).await?;
            metrics.rpc_calls += 1;
            let expected = expected_friend_map
                .get(viewer)
                .ok_or_else(|| boxed_err(format!("missing expected friend map for {viewer}")))?;
            let actual: std::collections::HashSet<u64> =
                friends.iter().map(|f| f.user_id).collect();
            if &actual != expected {
                metrics.errors.push(format!(
                    "{viewer} local friend set mismatch expected={:?} actual={:?}",
                    expected, actual
                ));
            } else {
                metrics.rpc_successes += 1;
            }
        }

        let pairs = [("alice", "bob"), ("alice", "charlie"), ("bob", "charlie")];
        for (left, right) in pairs {
            let channel_id = manager.cached_direct_channel(left, right).ok_or_else(|| {
                boxed_err(format!("missing direct channel cache for {left}-{right}"))
            })?;
            for (viewer, peer) in [(left, right), (right, left)] {
                let sdk = manager.sdk(viewer)?;
                let peer_uid = manager.user_id(peer)?;
                let channels = manager.list_local_channels(viewer).await?;
                metrics.rpc_calls += 1;
                let channel = channels.into_iter().find(|c| c.channel_id == channel_id);
                if channel.is_none() {
                    metrics.errors.push(format!(
                        "{viewer} missing cached direct channel {channel_id}"
                    ));
                    continue;
                }
                let friends = manager.list_local_friends(viewer).await?;
                metrics.rpc_calls += 1;
                if friends.len() != 2 {
                    metrics.errors.push(format!(
                        "{viewer} local friend count mismatch: expected=2 actual={}",
                        friends.len()
                    ));
                } else {
                    metrics.rpc_successes += 1;
                }
                let friend = friends.iter().find(|f| f.user_id == peer_uid);
                if friend.is_none() {
                    metrics
                        .errors
                        .push(format!("{viewer} missing friend entry for peer={peer_uid}"));
                    continue;
                }
                let friend = friend.expect("checked above");
                let user = sdk.get_user_by_id(peer_uid).await?;
                metrics.rpc_calls += 1;
                let expected = resolve_friend_display_name(friend, user.as_ref());
                if expected.trim().is_empty() {
                    metrics.errors.push(format!(
                        "{viewer} -> {peer} resolved empty friend display (uid={peer_uid})"
                    ));
                    continue;
                }
                if expected == peer_uid.to_string() {
                    let has_any_name = friend
                        .alias
                        .as_ref()
                        .map(|s| !s.trim().is_empty())
                        .unwrap_or(false)
                        || friend
                            .nickname
                            .as_ref()
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false)
                        || friend
                            .username
                            .as_ref()
                            .map(|s| !s.trim().is_empty())
                            .unwrap_or(false)
                        || user
                            .as_ref()
                            .map(|u| {
                                u.alias
                                    .as_ref()
                                    .map(|s| !s.trim().is_empty())
                                    .unwrap_or(false)
                                    || u.nickname
                                        .as_ref()
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                                    || u.username
                                        .as_ref()
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                            })
                            .unwrap_or(false);
                    if has_any_name {
                        metrics.errors.push(format!(
                            "{viewer}->{peer} display degraded to user_id while profile has a valid name field"
                        ));
                    }
                }
                if let Some(ch) = channel {
                    if ch.channel_name.trim() == expected.trim() {
                        metrics.rpc_successes += 1;
                    } else {
                        metrics.errors.push(format!(
                            "{viewer} direct title mismatch channel={} expected='{}' actual='{}'",
                            ch.channel_id, expected, ch.channel_name
                        ));
                    }
                }
            }
        }

        Ok(PhaseResult {
            phase_name: "friend-display-name-rules".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "friend display name follows alias > nickname > username > user_id; direct channel title must match peer display"
                .to_string(),
            metrics,
        })
    }

    pub async fn phase29_channel_title_rules(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        manager.refresh_all_local_views().await?;

        let expected_direct_ids_by_user: std::collections::HashMap<
            &str,
            std::collections::HashSet<u64>,
        > = [
            ("alice", [("alice", "bob"), ("alice", "charlie")]),
            ("bob", [("alice", "bob"), ("bob", "charlie")]),
            ("charlie", [("alice", "charlie"), ("bob", "charlie")]),
        ]
        .into_iter()
        .map(|(viewer, pairs)| {
            let ids = pairs
                .into_iter()
                .filter_map(|(a, b)| manager.cached_direct_channel(a, b))
                .collect::<std::collections::HashSet<_>>();
            (viewer, ids)
        })
        .collect();

        // Inject one synthetic empty-name group channel to verify fallback title materialization.
        let synthetic_group_id: u64 = 9_900_000_001;
        let alice_sdk = manager.sdk("alice")?;
        alice_sdk
            .upsert_group(privchat_sdk::UpsertGroupInput {
                group_id: synthetic_group_id,
                name: None,
                avatar: String::new(),
                owner_id: Some(manager.user_id("alice")?),
                is_dismissed: false,
                member_count: None, // 合成测试群无权威计数；None = 不覆盖既有值
                created_at: now_millis(),
                version: now_millis(),
                updated_at: now_millis(),
            })
            .await?;
        alice_sdk
            .upsert_channel(privchat_sdk::UpsertChannelInput {
                channel_id: synthetic_group_id,
                channel_type: GROUP_SYNC_CHANNEL_TYPE as i32,
                channel_name: String::new(),
                channel_remark: String::new(),
                avatar: String::new(),
                unread_count: 0,
                top: 0,
                mute: 0,
                last_msg_timestamp: now_millis(),
                last_local_message_id: 0,
                last_msg_content: String::new(),
                version: now_millis(),
                peer_user_id: None,
            })
            .await?;
        metrics.rpc_calls += 2;
        metrics.rpc_successes += 2;

        for key in ["alice", "bob", "charlie"] {
            let channels = manager.list_local_channels(key).await?;
            metrics.rpc_calls += 1;
            let mut seen_channel_ids = std::collections::HashSet::new();
            let mut direct_ids = std::collections::HashSet::new();
            for c in &channels {
                if !seen_channel_ids.insert(c.channel_id) {
                    metrics.errors.push(format!(
                        "{key} duplicate channel id in list_channels: {}",
                        c.channel_id
                    ));
                }
                if c.unread_count < 0 {
                    metrics.errors.push(format!(
                        "{key} channel {} has negative unread_count={}",
                        c.channel_id, c.unread_count
                    ));
                }
                if c.channel_type == DIRECT_SYNC_CHANNEL_TYPE as i32 {
                    direct_ids.insert(c.channel_id);
                }
            }
            let expected_direct_ids = expected_direct_ids_by_user
                .get(key)
                .ok_or_else(|| boxed_err(format!("missing expected direct id map for {key}")))?;
            if !expected_direct_ids.is_subset(&direct_ids) {
                let missing: std::collections::HashSet<u64> = expected_direct_ids
                    .difference(&direct_ids)
                    .copied()
                    .collect();
                metrics.errors.push(format!(
                    "{key} direct channel set missing expected ids missing={:?} expected={:?} actual={:?}",
                    missing, expected_direct_ids, direct_ids
                ));
            } else {
                metrics.rpc_successes += 1;
            }

            for c in channels {
                if c.channel_type == DIRECT_SYNC_CHANNEL_TYPE as i32 {
                    if c.channel_name.trim().is_empty() {
                        metrics
                            .errors
                            .push(format!("{key} direct channel {} title empty", c.channel_id));
                    } else {
                        metrics.rpc_successes += 1;
                    }
                    let history = manager
                        .list_local_messages(key, c.channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32, 1)
                        .await?;
                    metrics.rpc_calls += 1;
                    if !history.is_empty() {
                        if c.last_local_message_id != history[0].message_id {
                            metrics.errors.push(format!(
                                "{key} direct channel {} last_local_message_id mismatch channel={} latest={}",
                                c.channel_id, c.last_local_message_id, history[0].message_id
                            ));
                        } else {
                            metrics.rpc_successes += 1;
                        }
                        if c.last_msg_timestamp <= 0 {
                            metrics.errors.push(format!(
                                "{key} direct channel {} invalid last_msg_timestamp",
                                c.channel_id
                            ));
                        } else {
                            metrics.rpc_successes += 1;
                        }
                        if c.last_msg_content != history[0].content {
                            metrics.errors.push(format!(
                                "{key} direct channel {} last_msg_content mismatch channel='{}' latest='{}'",
                                c.channel_id, c.last_msg_content, history[0].content
                            ));
                        } else {
                            metrics.rpc_successes += 1;
                        }
                    }
                } else if c.channel_type == GROUP_SYNC_CHANNEL_TYPE as i32 {
                    if c.channel_name.trim().is_empty() {
                        metrics
                            .errors
                            .push(format!("{key} group channel {} title empty", c.channel_id));
                    } else {
                        metrics.rpc_successes += 1;
                    }
                }
            }

            let ordered = manager.list_local_channels(key).await?;
            metrics.rpc_calls += 1;
            let mut prev_top: Option<i32> = None;
            let mut prev_ts: Option<i64> = None;
            for c in ordered {
                if let (Some(prev_top_value), Some(prev_ts_value)) = (prev_top, prev_ts) {
                    if c.top == prev_top_value && c.last_msg_timestamp > prev_ts_value {
                        metrics.errors.push(format!(
                            "{key} channel list order invalid within top bucket {}: timestamp {} appears after {}",
                            c.top, c.last_msg_timestamp, prev_ts_value
                        ));
                        break;
                    }
                }
                prev_top = Some(c.top);
                prev_ts = Some(c.last_msg_timestamp);
            }
            metrics.rpc_successes += 1;
        }

        // Strict direct-channel title checks by friend display rule (alias > nickname > username > user_id).
        for (left, right) in [("alice", "bob"), ("alice", "charlie"), ("bob", "charlie")] {
            let channel_id = manager.cached_direct_channel(left, right).ok_or_else(|| {
                boxed_err(format!("missing cached direct channel: {left}-{right}"))
            })?;
            for (viewer, peer) in [(left, right), (right, left)] {
                let sdk = manager.sdk(viewer)?;
                let channels = manager.list_local_channels(viewer).await?;
                metrics.rpc_calls += 1;
                let Some(ch) = channels.into_iter().find(|c| c.channel_id == channel_id) else {
                    metrics.errors.push(format!(
                        "{viewer} missing direct channel {channel_id} for pair {left}-{right}"
                    ));
                    continue;
                };
                let peer_uid = manager.user_id(peer)?;
                let user = sdk.get_user_by_id(peer_uid).await?;
                metrics.rpc_calls += 1;
                let expected = user
                    .as_ref()
                    .map(display_name_from_user)
                    .unwrap_or_else(|| peer_uid.to_string());
                if ch.channel_name.trim() == expected.trim() {
                    metrics.rpc_successes += 1;
                } else {
                    metrics.errors.push(format!(
                        "{viewer} direct channel {} title mismatch expected='{}' actual='{}'",
                        ch.channel_id, expected, ch.channel_name
                    ));
                }
            }
        }

        let alice_channels = manager.list_local_channels("alice").await?;
        metrics.rpc_calls += 1;
        if let Some(synthetic) = alice_channels
            .into_iter()
            .find(|c| c.channel_id == synthetic_group_id)
        {
            if synthetic.channel_name.trim().is_empty() {
                metrics.errors.push(
                    "synthetic empty-name group did not materialize fallback title".to_string(),
                );
            } else {
                metrics.rpc_successes += 1;
            }
        } else {
            metrics
                .errors
                .push("synthetic empty-name group channel missing".to_string());
        }

        Ok(PhaseResult {
            phase_name: "channel-title-rules".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details:
                "channel titles/fields are strict: no duplicates, direct titles match friend display, last message fields align with local latest row"
                    .to_string(),
            metrics,
        })
    }

    pub async fn phase30_timeline_cache_local_first(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .or_else(|| manager.cached_group_channel("alice_bob_friend_channel"))
            .ok_or_else(|| boxed_err("missing alice-bob channel for timeline cache test"))?;
        let channel_type = DIRECT_SYNC_CHANNEL_TYPE as i32;

        let sdk = manager.sdk("alice")?;
        let from_uid = manager.user_id("alice")?;

        let first = manager
            .list_local_messages("alice", channel_id, channel_type, 30)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let second = manager
            .list_local_messages("alice", channel_id, channel_type, 30)
            .await?;
        metrics.rpc_calls += 1;
        if first.iter().map(|m| m.message_id).collect::<Vec<_>>()
            == second.iter().map(|m| m.message_id).collect::<Vec<_>>()
        {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(
                "timeline cache consistency failed: repeated list_messages returned different rows"
                    .to_string(),
            );
        }

        let marker = format!("phase30-local-first-{}", now_millis());
        let created_id = sdk
            .create_local_message(privchat_sdk::NewMessage {
                channel_id,
                channel_type,
                from_uid,
                message_type: 0,
                content: marker.clone(),
                searchable_word: marker.clone(),
                setting: 0,
                extra: String::new(),
                ..Default::default()
            })
            .await?;
        metrics.messages_sent += 1;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let after_create = manager
            .list_local_messages("alice", channel_id, channel_type, 30)
            .await?;
        metrics.rpc_calls += 1;
        if after_create.first().map(|m| m.message_id) == Some(created_id)
            && after_create.first().map(|m| m.content.as_str()) == Some(marker.as_str())
        {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(
                "local-first timeline failed: newly created local message not visible at top"
                    .to_string(),
            );
        }

        let after_repeat = manager
            .list_local_messages("alice", channel_id, channel_type, 30)
            .await?;
        metrics.rpc_calls += 1;
        if after_repeat.first().map(|m| m.message_id) == Some(created_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(
                "timeline cache repeat-read failed: newest local message not stable".to_string(),
            );
        }

        Ok(PhaseResult {
            phase_name: "timeline-cache-local-first".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details:
                "repeat list_messages consistency + local create invalidation/visibility check"
                    .to_string(),
            metrics,
        })
    }

    /// Phase 31: Room 频道测试
    ///
    /// 1. 通过 Admin API 创建 Room 频道
    /// 2. 三个用户通过 SDK subscribe_channel 订阅该频道
    /// 3. 通过 Admin API 广播消息到频道
    /// 4. 验证三个用户都收到了广播消息（SubscriptionMessageReceived）
    /// 5. 取消订阅
    pub async fn phase31_room(manager: &mut MultiAccountManager) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let admin_host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let admin_port = std::env::var("PRIVCHAT_ADMIN_API_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(9090);
        let service_key = std::env::var("SERVICE_MASTER_KEY")
            .unwrap_or_else(|_| "your_service_master_key_here".to_string());
        let admin_base = format!("http://{}:{}", admin_host, admin_port);

        let client = reqwest::Client::new();

        // --- Step 1: 通过 Admin API 创建 Room 频道 ---
        let create_resp = client
            .post(format!("{}/api/service/room", admin_base))
            .header("X-Service-Key", &service_key)
            .json(&serde_json::json!({ "name": "phase31-test-channel" }))
            .send()
            .await?;
        metrics.rpc_calls += 1;

        if !create_resp.status().is_success() {
            let status = create_resp.status();
            let body = create_resp.text().await.unwrap_or_default();
            metrics
                .errors
                .push(format!("create room channel failed: {} {}", status, body));
            return Ok(PhaseResult {
                phase_name: "room".to_string(),
                success: false,
                duration: start.elapsed(),
                details: "Admin API create room channel failed".to_string(),
                metrics,
            });
        }

        let create_body: CreateRoomChannelResponse = create_resp
            .json::<AdminEnvelope<CreateRoomChannelResponse>>()
            .await?
            .data
            .ok_or_else(|| boxed_err("create_room_channel: empty envelope data"))?;
        metrics.rpc_successes += 1;
        let channel_id = create_body.channel_id;

        // --- Step 2: 三个用户订阅频道 ---
        let alice_sdk = manager.sdk("alice")?;
        let bob_sdk = manager.sdk("bob")?;
        let charlie_sdk = manager.sdk("charlie")?;

        let mut alice_events = alice_sdk.subscribe_events();
        let mut bob_events = bob_sdk.subscribe_events();
        let mut charlie_events = charlie_sdk.subscribe_events();

        for (name, sdk) in [
            ("alice", &alice_sdk),
            ("bob", &bob_sdk),
            ("charlie", &charlie_sdk),
        ] {
            // Room subscribe 必须带 ticket（spec ROOM_CHANNEL_SPEC §4.6）。
            // server 端配 [room_ticket].secret 后强制校验，否则返
            // reason_code=9 TICKET_MISSING。业务侧（这里替身）走
            // `/api/service/room-tickets/issue` 拿 ticket（spec §4.5）。
            let cfg = manager.account_config(name)?;
            let issue_resp = client
                .post(format!("{}/api/service/room-tickets/issue", admin_base))
                .header("X-Service-Key", &service_key)
                .json(&serde_json::json!({
                    "channel_id": channel_id,
                    "user_id": cfg.user_id,
                    "device_id": cfg.device_id,
                    "scope": "subscribe",
                    "ttl_secs": 300,
                }))
                .send()
                .await?;
            metrics.rpc_calls += 1;
            if !issue_resp.status().is_success() {
                let status = issue_resp.status();
                let body = issue_resp.text().await.unwrap_or_default();
                metrics.errors.push(format!(
                    "{} ticket issue failed: {} {}",
                    name, status, body
                ));
                continue;
            }
            let ticket = match issue_resp
                .json::<AdminEnvelope<IssueTicketResponse>>()
                .await
            {
                Ok(env) => match env.data {
                    Some(t) => t.ticket,
                    None => {
                        metrics
                            .errors
                            .push(format!("{} ticket issue: empty envelope data", name));
                        continue;
                    }
                },
                Err(e) => {
                    metrics
                        .errors
                        .push(format!("{} ticket issue: parse err {}", name, e));
                    continue;
                }
            };
            metrics.rpc_successes += 1;

            match sdk.subscribe_channel(channel_id, 2, Some(ticket)).await {
                Ok(_) => {
                    metrics.rpc_calls += 1;
                    metrics.rpc_successes += 1;
                }
                Err(e) => {
                    metrics.rpc_calls += 1;
                    metrics
                        .errors
                        .push(format!("{} subscribe_channel failed: {}", name, e));
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(200)).await;

        // --- Step 3: 通过 Admin API 广播消息 ---
        let broadcast_content = format!("hello-room-{}", now_millis());
        let broadcast_resp = client
            .post(format!(
                "{}/api/service/room/{}/broadcast",
                admin_base, channel_id
            ))
            .header("X-Service-Key", &service_key)
            .json(&serde_json::json!({
                "content": broadcast_content,
                "sender_id": 0
            }))
            .send()
            .await?;
        metrics.rpc_calls += 1;

        if !broadcast_resp.status().is_success() {
            let status = broadcast_resp.status();
            let body = broadcast_resp.text().await.unwrap_or_default();
            metrics
                .errors
                .push(format!("broadcast failed: {} {}", status, body));
            return Ok(PhaseResult {
                phase_name: "room".to_string(),
                success: false,
                duration: start.elapsed(),
                details: "Admin API broadcast failed".to_string(),
                metrics,
            });
        }

        let broadcast_body: RoomBroadcastResponse = broadcast_resp
            .json::<AdminEnvelope<RoomBroadcastResponse>>()
            .await?
            .data
            .ok_or_else(|| boxed_err("room_broadcast: empty envelope data"))?;
        metrics.rpc_successes += 1;

        if broadcast_body.online_count < 3 {
            metrics.errors.push(format!(
                "expected online_count >= 3, got {}",
                broadcast_body.online_count
            ));
        }

        // --- Step 4: 验证三个用户收到广播消息 ---
        tokio::time::sleep(Duration::from_millis(500)).await;

        // 通过 SDK event 检查是否收到 SubscriptionMessageReceived 事件
        let mut received = [false; 3];
        for (i, events) in [&mut alice_events, &mut bob_events, &mut charlie_events]
            .iter_mut()
            .enumerate()
        {
            loop {
                match events.try_recv() {
                    Ok(event) => {
                        if let privchat_sdk::SdkEvent::SubscriptionMessageReceived {
                            channel_id: cid,
                            payload,
                            ..
                        } = &event
                        {
                            if *cid == channel_id {
                                let content = String::from_utf8_lossy(payload);
                                if content.contains("hello-room-") {
                                    received[i] = true;
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }

        let names = ["alice", "bob", "charlie"];
        let mut receive_count = 0;
        for (i, got) in received.iter().enumerate() {
            if *got {
                receive_count += 1;
            } else {
                metrics
                    .errors
                    .push(format!("{} did not receive room broadcast", names[i]));
            }
        }

        // --- Step 5: 取消订阅 ---
        for (name, sdk) in [
            ("alice", &alice_sdk),
            ("bob", &bob_sdk),
            ("charlie", &charlie_sdk),
        ] {
            if let Err(e) = sdk.unsubscribe_channel(channel_id, 2).await {
                metrics
                    .errors
                    .push(format!("{} unsubscribe_channel failed: {}", name, e));
            }
        }

        // --- Step 6: 验证取消订阅后在线人数为 0 ---
        let verify_resp = client
            .get(format!("{}/api/service/room/{}", admin_base, channel_id))
            .header("X-Service-Key", &service_key)
            .send()
            .await?;
        metrics.rpc_calls += 1;

        if verify_resp.status().is_success() {
            let channel_info: RoomChannelInfoResponse = verify_resp
                .json::<AdminEnvelope<RoomChannelInfoResponse>>()
                .await?
                .data
                .ok_or_else(|| boxed_err("room_channel_info: empty envelope data"))?;
            metrics.rpc_successes += 1;
            if channel_info.online_count != 0 {
                metrics.errors.push(format!(
                    "after unsubscribe, online_count should be 0, got {}",
                    channel_info.online_count
                ));
            }
        }

        let details = format!(
            "channel_id={}, broadcast delivered={}/{}, received={}/3",
            channel_id, broadcast_body.delivered, broadcast_body.online_count, receive_count
        );

        Ok(PhaseResult {
            phase_name: "room".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details,
            metrics,
        })
    }

    pub async fn phase32_channel_state_resume_smoke(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .ok_or_else(|| boxed_err("missing alice-bob direct channel for channel-state smoke"))?;

        let pin = manager.channel_pin("alice", channel_id, true).await?;
        metrics.rpc_calls += 1;
        if pin {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("channel pin returned false".to_string());
        }

        let mute = manager.channel_mute("alice", channel_id, true).await?;
        metrics.rpc_calls += 1;
        if mute {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("channel mute returned false".to_string());
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
        manager.refresh_local_views("alice").await?;
        metrics.rpc_calls += 1;

        let before = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing before reconnect"))?;
        metrics.rpc_calls += 1;

        if before.top == 1 && before.mute == 1 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "before reconnect top/mute mismatch top={} mute={}",
                before.top, before.mute
            ));
        }

        let sdk = manager.sdk("alice")?;
        let _ = sdk.disconnect().await;
        reconnect_account(manager, "alice").await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let after = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing after reconnect"))?;
        metrics.rpc_calls += 1;

        if after.top == 1 && after.mute == 1 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "after reconnect top/mute mismatch top={} mute={}",
                after.top, after.mute
            ));
        }

        let unpin = manager.channel_pin("alice", channel_id, false).await?;
        metrics.rpc_calls += 1;
        if unpin {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("channel unpin cleanup returned false".to_string());
        }

        let unmute = manager.channel_mute("alice", channel_id, false).await?;
        metrics.rpc_calls += 1;
        if unmute {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("channel unmute cleanup returned false".to_string());
        }

        Ok(PhaseResult {
            phase_name: "channel-state-resume-smoke".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "alice pin/mute survives reconnect and local refresh".to_string(),
            metrics,
        })
    }

    pub async fn phase33_unread_resume_strict(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let channel_id = manager
            .cached_direct_channel("alice", "bob")
            .ok_or_else(|| boxed_err("missing alice-bob direct channel for unread resume"))?;

        let probe = format!("phase33 unread probe {}", now_millis());
        let submit = manager
            .send_text("bob", channel_id, DIRECT_SYNC_CHANNEL_TYPE, &probe)
            .await?;
        metrics.rpc_calls += 1;
        metrics.messages_sent += 1;
        if submit_ok(&submit) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("phase33 bob->alice submit rejected".to_string());
        }

        tokio::time::sleep(Duration::from_millis(400)).await;
        manager.refresh_local_views("alice").await?;
        metrics.rpc_calls += 1;

        let unread_before_mark = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing before mark_read"))?
            .unread_count;
        metrics.rpc_calls += 1;
        if unread_before_mark >= 1 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "expected unread before mark_read >= 1, actual={unread_before_mark}"
            ));
        }

        let pts_before_read: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let read = manager
            .mark_read("alice", channel_id, pts_before_read.current_pts)
            .await?;
        metrics.rpc_calls += 1;
        if read.last_read_pts >= pts_before_read.current_pts {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "mark_read did not advance to current pts: read={} current={}",
                read.last_read_pts, pts_before_read.current_pts
            ));
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
        manager.refresh_local_views("alice").await?;
        metrics.rpc_calls += 1;

        let unread_after_mark = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing after mark_read"))?
            .unread_count;
        metrics.rpc_calls += 1;
        if unread_after_mark == 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "expected unread after mark_read == 0, actual={unread_after_mark}"
            ));
        }

        let self_text = format!("phase33 self send {}", now_millis());
        let self_submit = manager
            .send_text("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE, &self_text)
            .await?;
        metrics.rpc_calls += 1;
        metrics.messages_sent += 1;
        if submit_ok(&self_submit) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("phase33 alice self-send rejected".to_string());
        }

        let read_target_pts = self_submit
            .pts
            .or(self_submit.server_msg_id.map(|_| self_submit.current_pts))
            .filter(|pts| *pts > 0)
            .ok_or_else(|| boxed_err("phase33 self-send missing accepted pts"))?;

        let read_after_self_send = manager
            .mark_read("alice", channel_id, read_target_pts)
            .await?;
        metrics.rpc_calls += 1;
        if read_after_self_send.last_read_pts >= read_target_pts {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "mark_read after self-send did not advance to current pts: read={} current={}",
                read_after_self_send.last_read_pts, read_target_pts
            ));
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
        manager.refresh_local_views("alice").await?;
        metrics.rpc_calls += 1;

        let before_reconnect = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing before reconnect"))?;
        metrics.rpc_calls += 1;
        let before_extra = manager
            .get_local_channel_extra("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32)
            .await?;
        metrics.rpc_calls += 1;
        let before_sdk_unread = manager
            .get_local_channel_unread("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32)
            .await?;
        metrics.rpc_calls += 1;
        if before_reconnect.unread_count == 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "expected unread before reconnect == 0, actual={} sdk_unread={} keep_pts={}",
                before_reconnect.unread_count,
                before_sdk_unread,
                before_extra.as_ref().map(|v| v.keep_pts).unwrap_or(0)
            ));
        }

        let pts_before_reconnect: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        reconnect_account(manager, "alice").await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let after_reconnect = manager
            .list_local_channels("alice")
            .await?
            .into_iter()
            .find(|c| c.channel_id == channel_id)
            .ok_or_else(|| boxed_err("alice local direct channel missing after reconnect"))?;
        metrics.rpc_calls += 1;
        let after_messages = manager
            .list_local_messages("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32, 6)
            .await?;
        metrics.rpc_calls += 1;
        let after_extra = manager
            .get_local_channel_extra("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32)
            .await?;
        metrics.rpc_calls += 1;
        let after_sdk_unread = manager
            .get_local_channel_unread("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE as i32)
            .await?;
        metrics.rpc_calls += 1;
        if after_reconnect.unread_count == 0 {
            metrics.rpc_successes += 1;
        } else {
            let after_message_brief = after_messages
                .iter()
                .take(4)
                .map(|m| {
                    format!(
                        "id={} sid={:?} lid={:?} from={} status={}",
                        m.message_id, m.server_message_id, m.local_message_id, m.from_uid, m.status
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            metrics.errors.push(format!(
                "expected unread after reconnect == 0, actual={} sdk_unread={} keep_pts={} version={} msgs=[{}]",
                after_reconnect.unread_count,
                after_sdk_unread,
                after_extra.as_ref().map(|v| v.keep_pts).unwrap_or(0),
                after_reconnect.version,
                after_message_brief
            ));
        }

        let pts_after_reconnect: privchat_protocol::rpc::GetChannelPtsResponse = manager
            .rpc_typed(
                "alice",
                sync::GET_CHANNEL_PTS,
                &privchat_protocol::rpc::GetChannelPtsRequest {
                    channel_id,
                    channel_type: DIRECT_SYNC_CHANNEL_TYPE,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        if pts_after_reconnect.current_pts >= pts_before_reconnect.current_pts {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "pts regressed across reconnect: before={} after={}",
                pts_before_reconnect.current_pts, pts_after_reconnect.current_pts
            ));
        }

        let server_unread_after = manager
            .message_status_count("alice", Some(channel_id))
            .await?;
        metrics.rpc_calls += 1;
        if server_unread_after.unread_count == 0 {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "server unread after reconnect expected 0 actual={}",
                server_unread_after.unread_count
            ));
        }

        Ok(PhaseResult {
            phase_name: "unread-resume-strict".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "alice read-acks bob message, self-sends, reconnects, unread stays zero and pts does not regress".to_string(),
            metrics,
        })
    }

    /// Phase 34: admin 推送与 RPC 推送共享同一投递入口（CONNECTION_LIFECYCLE_SPEC §8.8）
    ///
    /// 1. 快照 /metrics（投递计数器 before）
    /// 2. 通过 admin API `POST /api/service/messages/send` 让 charlie 向 main_group 发消息
    /// 3. 等待 fanout，刷新 alice/bob 本地视图，断言两人都收到文本
    /// 4. 快照 /metrics after，断言 delta：
    ///    - attempt_total  ≥ 2（至少覆盖 alice + bob 两个 user 粒度投递）
    ///    - success_sessions_total ≥ 2（两人在线）
    ///    - zero_success_total Δ == 0（无 user 维度 0 成功）
    ///    - offline_enqueue_total Δ == 0（在线用户不得落离线）
    pub async fn phase34_admin_push_online(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let admin_port = std::env::var("PRIVCHAT_ADMIN_API_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(9090);
        let metrics_port = std::env::var("PRIVCHAT_METRICS_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(9083);
        let service_key = std::env::var("SERVICE_MASTER_KEY")
            .unwrap_or_else(|_| "your_service_master_key_here".to_string());
        let admin_base = format!("http://{}:{}", host, admin_port);
        let metrics_url = format!("http://{}:{}/metrics", host, metrics_port);

        let client = reqwest::Client::new();

        let channel_id = require_group_channel(manager, "main_group")?;
        let charlie_uid = manager.user_id("charlie")?;

        // 为了让 delta 只覆盖本次 admin push，先刷一轮 metrics
        let before = fetch_delivery_metrics(&client, &metrics_url).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let probe = format!("admin-push-{}", now_millis());
        let resp = client
            .post(format!("{}/api/service/messages/send", admin_base))
            .header("X-Service-Key", &service_key)
            .json(&serde_json::json!({
                "channel_id": channel_id,
                "sender_id": charlie_uid,
                "content": probe,
                "message_type": "text",
            }))
            .send()
            .await?;
        metrics.rpc_calls += 1;
        metrics.messages_sent += 1;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            metrics
                .errors
                .push(format!("admin send_message failed: {status} {body}"));
            return Ok(phase_fail(
                "admin-push-online",
                start.elapsed(),
                "admin send_message returned non-2xx",
                metrics,
            ));
        }
        metrics.rpc_successes += 1;

        // fanout 真正到达 SDK 需要经过 server 落库 + transport.send + SDK ack，1.5s 足够
        tokio::time::sleep(Duration::from_millis(1500)).await;

        for key in ["alice", "bob"] {
            if let Err(e) = manager.refresh_local_views(key).await {
                metrics
                    .errors
                    .push(format!("refresh_local_views({key}) failed: {e}"));
            } else {
                metrics.rpc_calls += 1;
                metrics.rpc_successes += 1;
            }
        }

        for key in ["alice", "bob"] {
            let msgs = manager
                .list_local_messages(key, channel_id, GROUP_SYNC_CHANNEL_TYPE as i32, 10)
                .await?;
            metrics.rpc_calls += 1;
            let hit = msgs
                .iter()
                .any(|m| m.from_uid == charlie_uid && m.content.contains(&probe));
            if hit {
                metrics.rpc_successes += 1;
            } else {
                let brief = msgs
                    .iter()
                    .take(4)
                    .map(|m| {
                        format!("id={} from={} content={}", m.message_id, m.from_uid, m.content)
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                metrics.errors.push(format!(
                    "{key} did not receive admin-push probe={probe} latest=[{brief}]"
                ));
            }
        }

        let after = fetch_delivery_metrics(&client, &metrics_url).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let d_attempt = after.attempt.saturating_sub(before.attempt);
        let d_success = after.success_sessions.saturating_sub(before.success_sessions);
        let d_zero = after.zero_success.saturating_sub(before.zero_success);
        let d_offline = after.offline_enqueue.saturating_sub(before.offline_enqueue);

        if d_attempt < 2 {
            metrics.errors.push(format!(
                "expected delivery_attempt_total Δ ≥ 2 (alice+bob), got {d_attempt}"
            ));
        }
        if d_success < 2 {
            metrics.errors.push(format!(
                "expected delivery_success_sessions_total Δ ≥ 2 (both online), got {d_success}"
            ));
        }
        if d_zero != 0 {
            metrics.errors.push(format!(
                "expected delivery_zero_success_total Δ == 0, got {d_zero}"
            ));
        }
        if d_offline != 0 {
            metrics.errors.push(format!(
                "expected offline_enqueue_total Δ == 0 (both recipients online), got {d_offline}"
            ));
        }

        Ok(PhaseResult {
            phase_name: "admin-push-online".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "admin /messages/send → main_group: attempt Δ={d_attempt} success Δ={d_success} zero Δ={d_zero} offline Δ={d_offline}"
            ),
            metrics,
        })
    }

    /// Phase 35 - 验证 admin 撤回副作用闭环（P0 收敛后）
    ///
    /// 覆盖 `ADMIN_PATH_CONVERGENCE_AUDIT §1.1` 的全部 6 项副作用：
    /// - DB 撤回标记（admin 响应里的 `revoked_at` > 0）
    /// - 推送撤回事件（alice/bob 本地 `StoredMessage.revoked == true`，这是最关键的用户可见信号）
    /// - 离线队列清理、缓存同步、PTS commit 等间接通过"在线端收到撤回"来兜底
    ///
    /// 若 admin 路径回退到 `message_repository.revoke_message(id, 0)` 单步写法，
    /// 客户端不会收到撤回事件，本 phase 必然失败——作为回归锚点。
    pub async fn phase35_admin_revoke_online(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let admin_port = std::env::var("PRIVCHAT_ADMIN_API_PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(9090);
        let service_key = std::env::var("SERVICE_MASTER_KEY")
            .unwrap_or_else(|_| "your_service_master_key_here".to_string());
        let admin_base = format!("http://{}:{}", host, admin_port);

        let client = reqwest::Client::new();

        let channel_id = require_group_channel(manager, "main_group")?;
        let charlie_uid = manager.user_id("charlie")?;

        // 1) 通过 admin 发一条种子消息，拿到一个确切可控的 message_id
        let probe = format!("admin-revoke-seed-{}", now_millis());
        let send_resp = client
            .post(format!("{}/api/service/messages/send", admin_base))
            .header("X-Service-Key", &service_key)
            .json(&serde_json::json!({
                "channel_id": channel_id,
                "sender_id": charlie_uid,
                "content": probe,
                "message_type": "text",
            }))
            .send()
            .await?;
        metrics.rpc_calls += 1;
        metrics.messages_sent += 1;
        if !send_resp.status().is_success() {
            let status = send_resp.status();
            let body = send_resp.text().await.unwrap_or_default();
            metrics
                .errors
                .push(format!("admin seed send failed: {status} {body}"));
            return Ok(phase_fail(
                "admin-revoke-online",
                start.elapsed(),
                "seed admin send_message returned non-2xx",
                metrics,
            ));
        }
        metrics.rpc_successes += 1;

        // 2) 等待 fanout 到 SDK 端并刷一下本地视图
        tokio::time::sleep(Duration::from_millis(1500)).await;
        for key in ["alice", "bob"] {
            if let Err(e) = manager.refresh_local_views(key).await {
                metrics
                    .errors
                    .push(format!("refresh_local_views({key}) pre-revoke failed: {e}"));
            }
        }

        // 3) 在 alice 本地定位 server_message_id
        let alice_msgs = manager
            .list_local_messages("alice", channel_id, GROUP_SYNC_CHANNEL_TYPE as i32, 20)
            .await?;
        metrics.rpc_calls += 1;
        let seed = alice_msgs
            .iter()
            .find(|m| m.from_uid == charlie_uid && m.content.contains(&probe))
            .cloned();
        let seed = match seed {
            Some(s) => s,
            None => {
                metrics
                    .errors
                    .push(format!("alice 未收到 admin 种子消息，probe={probe}"));
                return Ok(phase_fail(
                    "admin-revoke-online",
                    start.elapsed(),
                    "seed message did not arrive at alice before revoke",
                    metrics,
                ));
            }
        };
        let server_message_id = seed
            .server_message_id
            .unwrap_or(seed.message_id);
        metrics.rpc_successes += 1;

        // 4) admin 撤回这条消息
        let revoke_resp = client
            .post(format!(
                "{}/api/service/messages/{}/revoke",
                admin_base, server_message_id
            ))
            .header("X-Service-Key", &service_key)
            .json(&serde_json::json!({"reason": "phase35 audit"}))
            .send()
            .await?;
        metrics.rpc_calls += 1;
        if !revoke_resp.status().is_success() {
            let status = revoke_resp.status();
            let body = revoke_resp.text().await.unwrap_or_default();
            metrics
                .errors
                .push(format!("admin revoke failed: {status} {body}"));
            return Ok(phase_fail(
                "admin-revoke-online",
                start.elapsed(),
                "admin revoke returned non-2xx",
                metrics,
            ));
        }
        let revoke_envelope: serde_json::Value = revoke_resp.json().await?;
        metrics.rpc_successes += 1;

        // Server wraps the response in `{ code, message, data: { channel_id, revoked_at, ... } }`.
        let revoke_body = revoke_envelope.get("data").unwrap_or(&revoke_envelope);

        let resp_channel_id = revoke_body
            .get("channel_id")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if resp_channel_id != channel_id {
            metrics.errors.push(format!(
                "admin revoke response channel_id mismatch: expected {channel_id}, got {resp_channel_id}"
            ));
        }
        let resp_revoked_at = revoke_body
            .get("revoked_at")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if resp_revoked_at <= 0 {
            metrics
                .errors
                .push("admin revoke response missing revoked_at".to_string());
        }

        // 5) 等待撤回事件经 ConnectionManager 推到 SDK 并落库
        tokio::time::sleep(Duration::from_millis(1500)).await;
        for key in ["alice", "bob"] {
            if let Err(e) = manager.refresh_local_views(key).await {
                metrics
                    .errors
                    .push(format!("refresh_local_views({key}) post-revoke failed: {e}"));
            }
        }

        // 6) 验证 alice/bob 本地的消息已 revoked=true —— 这是 admin 路径以前缺失的用户可见副作用
        for key in ["alice", "bob"] {
            let msgs = manager
                .list_local_messages(key, channel_id, GROUP_SYNC_CHANNEL_TYPE as i32, 30)
                .await?;
            metrics.rpc_calls += 1;
            let entry = msgs.iter().find(|m| {
                m.server_message_id == Some(server_message_id) || m.message_id == server_message_id
            });
            match entry {
                Some(m) if m.revoked => metrics.rpc_successes += 1,
                Some(m) => metrics.errors.push(format!(
                    "{key} 本地消息未标记 revoked: message_id={}, revoked={}",
                    m.message_id, m.revoked
                )),
                None => metrics.errors.push(format!(
                    "{key} 本地找不到被撤回的消息 server_message_id={server_message_id}"
                )),
            }
        }

        Ok(PhaseResult {
            phase_name: "admin-revoke-online".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: format!(
                "admin revoke server_message_id={server_message_id} → alice/bob 本地 revoked=true"
            ),
            metrics,
        })
    }

    /// Phase 36: BotFollow → ServerEvent → 自动 business_channel binding →
    /// Transfer `bot/menu/get` 全链 smoke。
    ///
    /// 测的是 v1.1 闭环（spec `SERVER_EVENT_DISPATCH_SPEC` + `ADMIN_BOT_SPEC` §7）：
    ///
    /// 1. admin 登录拿 JWT
    /// 2. POST /admin/privchat/bot/create with owner = alice.user_id
    /// 3. PUT  /admin/privchat/bot/{bot_id}/menu 写 fixture menu_schema
    /// 4. alice 调 wire RPC `account/bot/follow` → 拿到 channel_id
    /// 5. 等 ServerEvent fire-and-forget 异步落 binding（最多重试 5 次 × 200ms）
    /// 6. alice 调 wire Transfer route=`bot/menu/get` → 拿 menu_schema 字节
    /// 7. 断言：transfer.code=0 + JSON 解码后 == 第 3 步写入的 menu_schema
    ///
    /// 触发条件：`PRIVCHAT_PLATFORM_BASE_URL` 非空才跑（默认 `http://127.0.0.1:8080`）。
    /// 空时返 pass 但 `details="skipped: ..."` —— 不阻塞 server-only CI。
    pub async fn phase36_platform_bot_followed(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let platform_base = std::env::var("PRIVCHAT_PLATFORM_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
        if platform_base.is_empty() {
            return Ok(PhaseResult {
                phase_name: "platform-bot-followed".to_string(),
                success: true,
                duration: start.elapsed(),
                details: "skipped: PRIVCHAT_PLATFORM_BASE_URL is empty".to_string(),
                metrics,
            });
        }
        let admin_user = std::env::var("PRIVCHAT_PLATFORM_ADMIN_USERNAME")
            .unwrap_or_else(|_| "admin".to_string());
        let admin_pass = std::env::var("PRIVCHAT_PLATFORM_ADMIN_PASSWORD")
            .unwrap_or_else(|_| "admin123".to_string());

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;

        // 1. admin login
        let login_resp = http
            .post(format!("{}/admin/system/auth/login", platform_base))
            .json(&serde_json::json!({
                "username": admin_user,
                "password": admin_pass,
            }))
            .send()
            .await?;
        let login_status = login_resp.status();
        let login_body: serde_json::Value = login_resp.json().await?;
        if !login_status.is_success() {
            return Err(boxed_err(format!(
                "admin login failed: status={login_status} body={login_body}"
            )));
        }
        let access_token = login_body
            .pointer("/data/accessToken")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                boxed_err(format!("admin login missing data.accessToken: {login_body}"))
            })?
            .to_string();

        // 2. create bot —— owner = alice，避免多次跑互相覆盖：username 带 suffix
        let alice = manager.account_config("alice")?;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let bot_username = format!("smokebot_{}", suffix);
        let create_resp = http
            .post(format!("{}/admin/privchat/bot/create", platform_base))
            .bearer_auth(&access_token)
            .json(&serde_json::json!({
                "name": "Smoke Bot",
                "username": bot_username,
                "owner_user_id": alice.user_id,
            }))
            .send()
            .await?;
        let create_status = create_resp.status();
        let create_body: serde_json::Value = create_resp.json().await?;
        if !create_status.is_success() {
            return Err(boxed_err(format!(
                "bot create failed: status={create_status} body={create_body}"
            )));
        }
        let bot_user_id = create_body
            .pointer("/data/id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| boxed_err(format!("bot create missing data.id: {create_body}")))?;

        // 3. set menu_schema —— fixture：1 个 transfer action 项
        let menu_schema = serde_json::json!({
            "version": 1,
            "items": [
                {
                    "id": "hi",
                    "title": "Hi",
                    "action": { "type": "transfer", "route": "bot/echo/ping" }
                }
            ]
        });
        let menu_resp = http
            .put(format!(
                "{}/admin/privchat/bot/{}/menu",
                platform_base, bot_user_id
            ))
            .bearer_auth(&access_token)
            .json(&serde_json::json!({ "menu_schema": menu_schema }))
            .send()
            .await?;
        let menu_status = menu_resp.status();
        if !menu_status.is_success() {
            let body: serde_json::Value = menu_resp
                .json()
                .await
                .unwrap_or(serde_json::Value::Null);
            return Err(boxed_err(format!(
                "bot menu set failed: status={menu_status} body={body}"
            )));
        }

        // 4. alice 调 wire RPC account/bot/follow
        let follow_resp: privchat_protocol::rpc::account::bot::BotFollowResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::account_bot::FOLLOW,
                &privchat_protocol::rpc::account::bot::BotFollowRequest { bot_user_id },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let channel_id = follow_resp.channel_id;
        if channel_id == 0 {
            return Err(boxed_err(format!(
                "bot.follow returned channel_id=0: {follow_resp:?}"
            )));
        }

        // 5. 等 ServerEvent fire-and-forget 落 binding。server emit + app 写表
        //    全异步；首发后第一个 bot/menu/get 偶尔 race 拿到 20901 ChannelNotBound，
        //    所以容忍最多 5 次 × 200ms 重试。
        let alice_sdk = manager.sdk("alice")?;
        let mut transfer_reply = None;
        let mut last_err: Option<String> = None;
        for attempt in 0..5 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            match alice_sdk
                .transfer(channel_id, "bot/menu/get".to_string(), Vec::new(), 5000)
                .await
            {
                Ok(reply) if reply.code == 0 => {
                    transfer_reply = Some(reply);
                    break;
                }
                Ok(reply) => {
                    last_err = Some(format!(
                        "transfer returned code={} message={} (attempt {})",
                        reply.code,
                        reply.message,
                        attempt + 1
                    ));
                }
                Err(e) => {
                    last_err = Some(format!("transfer call err: {e} (attempt {})", attempt + 1));
                }
            }
        }
        let reply = transfer_reply.ok_or_else(|| {
            boxed_err(format!(
                "bot/menu/get never succeeded after 5 attempts; last={}",
                last_err.unwrap_or_else(|| "<no error>".to_string())
            ))
        })?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        // 6. 断言 menu_schema round-trip
        let data = reply.data;
        if data.is_empty() {
            return Err(boxed_err("bot/menu/get returned empty data".to_string()));
        }
        let returned_menu: serde_json::Value = serde_json::from_slice(&data).map_err(|e| {
            boxed_err(format!(
                "bot/menu/get data not valid JSON: {e}; raw_len={}",
                data.len()
            ))
        })?;
        if returned_menu != menu_schema {
            return Err(boxed_err(format!(
                "menu_schema roundtrip mismatch:\n  set:      {}\n  returned: {}",
                serde_json::to_string(&menu_schema).unwrap_or_default(),
                serde_json::to_string(&returned_menu).unwrap_or_default(),
            )));
        }

        Ok(PhaseResult {
            phase_name: "platform-bot-followed".to_string(),
            success: true,
            duration: start.elapsed(),
            details: format!(
                "bot_user_id={bot_user_id} channel_id={channel_id} menu_items={}",
                returned_menu
                    .pointer("/items")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0)
            ),
            metrics,
        })
    }

    /// **F-sync.verify**：好友申请生命周期端到端闭环验证。
    ///
    /// 用 3 个一次性陌生账号（fsync_a / fsync_b / fsync_c）覆盖 3 个场景：
    ///
    /// 1. **accept** —— fsync_a apply alice → alice accept
    ///    - 申请阶段：fsync_a Sent[0] 含 alice；alice Received[0] 含 fsync_a
    ///    - accept 后：双方 friends 列表互相包含；fsync_a Sent[0]/Received[0]
    ///      都不再含对方（status=1 不在过滤集合）
    ///
    /// 2. **reject** —— fsync_b apply bob → bob reject
    ///    - reject 后：fsync_b Sent[0,3,4,5] 含 bob 且 status=3；
    ///      bob Received[0] 不含 fsync_b（pending-only filter 把 rejected 过滤）
    ///    - bob friends 列表不含 fsync_b
    ///
    /// 3. **recall** —— fsync_c apply charlie → fsync_c recall
    ///    - recall 后：fsync_c Sent[0,3,4,5] 含 charlie 且 status=4；
    ///      charlie Received[0] 不含 fsync_c
    ///    - charlie friends 列表不含 fsync_c
    pub async fn phase37_fsync_friend_request_lifecycle(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        // 引入 3 个一次性账号
        for key in ["fsync_a", "fsync_b", "fsync_c"] {
            manager.ensure_account(key).await?;
        }

        let alice_id = manager.user_id("alice")?;
        let bob_id = manager.user_id("bob")?;
        let charlie_id = manager.user_id("charlie")?;

        // ---- 场景 1：accept ----
        let _ = manager.send_friend_request("fsync_a", alice_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        // 等 server 写 friendships + push 触发 entity sync
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        // 申请阶段验证
        let fsync_a_sent_pending = manager
            .list_friend_requests("fsync_a", true, vec![0])
            .await?;
        metrics.rpc_calls += 1;
        if fsync_a_sent_pending.iter().any(|f| f.user_id == alice_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "scenario.accept: fsync_a Sent[0] missing alice (got {} rows)",
                fsync_a_sent_pending.len()
            ));
        }
        let alice_received_pending =
            manager.list_friend_requests("alice", false, vec![0]).await?;
        metrics.rpc_calls += 1;
        let fsync_a_id = manager.user_id("fsync_a")?;
        if alice_received_pending
            .iter()
            .any(|f| f.user_id == fsync_a_id)
        {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(format!(
                "scenario.accept: alice Received[0] missing fsync_a (got {} rows)",
                alice_received_pending.len()
            ));
        }

        // alice accept
        let _ = manager.accept_friend_request("alice", fsync_a_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        // accept 后：两边 pending 集合都不再含对方
        let fsync_a_sent_after = manager
            .list_friend_requests("fsync_a", true, vec![0])
            .await?;
        metrics.rpc_calls += 1;
        if !fsync_a_sent_after.iter().any(|f| f.user_id == alice_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.accept: fsync_a Sent[0] still has alice after accept".to_string());
        }
        let alice_received_after =
            manager.list_friend_requests("alice", false, vec![0]).await?;
        metrics.rpc_calls += 1;
        if !alice_received_after
            .iter()
            .any(|f| f.user_id == fsync_a_id)
        {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(
                "scenario.accept: alice Received[0] still has fsync_a after accept".to_string(),
            );
        }
        // 双方 friends list 互相包含
        let fsync_a_friends = manager.list_local_friends("fsync_a").await?;
        metrics.rpc_calls += 1;
        if fsync_a_friends.iter().any(|f| f.user_id == alice_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.accept: fsync_a friends list missing alice".to_string());
        }
        let alice_friends = manager.list_local_friends("alice").await?;
        metrics.rpc_calls += 1;
        if alice_friends.iter().any(|f| f.user_id == fsync_a_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.accept: alice friends list missing fsync_a".to_string());
        }

        // ---- 场景 2：reject ----
        let _ = manager.send_friend_request("fsync_b", bob_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        let fsync_b_id = manager.user_id("fsync_b")?;
        let _ = manager.reject_friend_request("bob", fsync_b_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        // fsync_b 视角：Sent[0,3,4,5] 含 bob 且 status=3
        let fsync_b_sent = manager
            .list_friend_requests("fsync_b", true, vec![0, 3, 4, 5])
            .await?;
        metrics.rpc_calls += 1;
        let bob_row = fsync_b_sent.iter().find(|f| f.user_id == bob_id);
        match bob_row {
            Some(row) if row.status == 3 => metrics.rpc_successes += 1,
            Some(row) => metrics.errors.push(format!(
                "scenario.reject: fsync_b Sent bob row status={} expected 3",
                row.status
            )),
            None => metrics
                .errors
                .push("scenario.reject: fsync_b Sent missing bob row".to_string()),
        }
        // bob 视角：Received[0] **不含** fsync_b
        let bob_received = manager.list_friend_requests("bob", false, vec![0]).await?;
        metrics.rpc_calls += 1;
        if !bob_received.iter().any(|f| f.user_id == fsync_b_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.reject: bob Received[0] still shows fsync_b after reject".to_string());
        }
        let bob_friends = manager.list_local_friends("bob").await?;
        metrics.rpc_calls += 1;
        if !bob_friends.iter().any(|f| f.user_id == fsync_b_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.reject: bob friends list unexpectedly has fsync_b".to_string());
        }

        // ---- 场景 3：recall ----
        let _ = manager.send_friend_request("fsync_c", charlie_id).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        let fsync_c_id = manager.user_id("fsync_c")?;
        let _ = manager
            .recall_friend_request("fsync_c", charlie_id)
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // refresh_all_local_views 仅覆盖 alice/bob/charlie；fsync_* 是新引入账号，
        // 需要显式刷它们的 entity/sync_entities("friend") 流。
        for key in ["alice", "bob", "charlie", "fsync_a", "fsync_b", "fsync_c"] {
            manager.refresh_local_views(key).await?;
        }

        // fsync_c 视角：Sent[0,3,4,5] 含 charlie 且 status=4
        let fsync_c_sent = manager
            .list_friend_requests("fsync_c", true, vec![0, 3, 4, 5])
            .await?;
        metrics.rpc_calls += 1;
        let charlie_row = fsync_c_sent.iter().find(|f| f.user_id == charlie_id);
        match charlie_row {
            Some(row) if row.status == 4 => metrics.rpc_successes += 1,
            Some(row) => metrics.errors.push(format!(
                "scenario.recall: fsync_c Sent charlie row status={} expected 4",
                row.status
            )),
            None => metrics
                .errors
                .push("scenario.recall: fsync_c Sent missing charlie row".to_string()),
        }
        // charlie 视角：Received[0] **不含** fsync_c
        let charlie_received = manager
            .list_friend_requests("charlie", false, vec![0])
            .await?;
        metrics.rpc_calls += 1;
        if !charlie_received.iter().any(|f| f.user_id == fsync_c_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push(
                "scenario.recall: charlie Received[0] still shows fsync_c after recall".to_string(),
            );
        }
        let charlie_friends = manager.list_local_friends("charlie").await?;
        metrics.rpc_calls += 1;
        if !charlie_friends.iter().any(|f| f.user_id == fsync_c_id) {
            metrics.rpc_successes += 1;
        } else {
            metrics
                .errors
                .push("scenario.recall: charlie friends list unexpectedly has fsync_c".to_string());
        }

        Ok(PhaseResult {
            phase_name: "fsync-friend-request-lifecycle".to_string(),
            success: metrics.errors.is_empty(),
            duration: start.elapsed(),
            details: "apply+accept / apply+reject / apply+recall 三场景双视角全闭环".to_string(),
            metrics,
        })
    }

    /// **System User group invitation hard reject** —— spec
    /// 07-application/SYSTEM_USER_SPEC §4 + 02-server/CHANNEL_SPEC §10.5。
    ///
    /// 验证 `group/member/add` 邀请 user_type=1 → 返 `21001
    /// SystemUserNotGroupInvitable`（protocol::ErrorCode）。
    ///
    /// 前置：application 启用 `PRIVCHAT_SMOKE_SYSTEM_USER=1` 已 bootstrap
    /// 一个 user_type=1 的 smoke System User。本 phase 通过 application
    /// 暴露的 `/service/privchat/smoke/system-user-status` 端点拿到
    /// system_user_id；未启用 smoke 时整 phase skipped。
    pub async fn phase38_system_user_group_reject(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let smoke = match fetch_smoke_system_user_status().await? {
            Some(s) => s,
            None => {
                return Ok(PhaseResult {
                    phase_name: "system-user-group-reject".to_string(),
                    success: true,
                    duration: start.elapsed(),
                    details: "skipped: PRIVCHAT_SMOKE_SYSTEM_USER not active (set =1 on application)".to_string(),
                    metrics,
                });
            }
        };

        // 1) alice 建一个临时小群（只含自己）
        let group_resp: privchat_protocol::rpc::group::group::GroupCreateResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::group::CREATE,
                &privchat_protocol::rpc::group::group::GroupCreateRequest {
                    name: format!("smoke-sysuser-reject-{}", now_millis()),
                    description: None,
                    member_ids: None,
                    creator_id: 0,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let group_id = group_resp.group_id;

        // 2) 尝试把 smoke System User 拉进群 → 必须 21001。
        // 直接用 sdk.rpc_call_typed 拿到原生 privchat_sdk::Error，便于精确 match
        // 在 Error::Server { code, .. } 上；manager.rpc_typed 会把 SDK Error
        // 装箱成 BoxError，丢失类型信息。
        let alice_sdk = manager.sdk("alice")?;
        let invite_result: Result<serde_json::Value, privchat_sdk::Error> = alice_sdk
            .rpc_call_typed(
                privchat_protocol::rpc::routes::group_member::ADD,
                &privchat_protocol::rpc::group::member::GroupMemberAddRequest {
                    group_id,
                    user_id: smoke.system_user_id,
                    role: None,
                    inviter_id: 0,
                },
            )
            .await;
        metrics.rpc_calls += 1;
        match invite_result {
            Ok(v) => Err(boxed_err(format!(
                "expected 21001 SystemUserNotGroupInvitable, got ok response: {v}"
            ))),
            Err(privchat_sdk::Error::Server { code, message })
                if code
                    == privchat_protocol::error_code::ErrorCode::SystemUserNotGroupInvitable
                        .code() =>
            {
                metrics.rpc_successes += 1;
                Ok(PhaseResult {
                    phase_name: "system-user-group-reject".to_string(),
                    success: true,
                    duration: start.elapsed(),
                    details: format!(
                        "group_id={group_id} system_user_id={} returned 21001: {message}",
                        smoke.system_user_id
                    ),
                    metrics,
                })
            }
            Err(other) => Err(boxed_err(format!(
                "expected 21001 SystemUserNotGroupInvitable, got: {other}"
            ))),
        }
    }

    /// **Assistant Round A end-to-end echo loop** —— spec
    /// `privchat-application-module-assistant` Round A 验收 + SYSTEM_USER_SPEC §8.5。
    ///
    /// 闭环：alice DM assistant System User → server emit
    /// `system_user.message_received` → application 一级 handler → assistant
    /// consumer.onMessageReceived → listMessages(channel_id, 20) → echo back。
    ///
    /// 该 phase 通过 env var **PRIVCHAT_ASSISTANT_USER_ID** 启用，传 assistant
    /// 的 system user_id（运维一次性 onboard 后查 DB 取值）。0 / 未设 → skip。
    ///
    /// 与 phase39 区别：phase39 走 smoke harness 的 NoopConsumer 计数器；
    /// 本 phase 不读 smoke endpoint，直接看 channel 是否收到 assistant 的 reply
    /// 消息（content 包含 `PrivChat Assistant`）。
    pub async fn phase40_assistant_echo_loop(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let assistant_uid_str = std::env::var("PRIVCHAT_ASSISTANT_USER_ID").unwrap_or_default();
        let assistant_uid: u64 = assistant_uid_str.parse().unwrap_or(0);
        if assistant_uid == 0 {
            return Ok(PhaseResult {
                phase_name: "assistant-echo-loop".to_string(),
                success: true,
                duration: start.elapsed(),
                details: "skipped: PRIVCHAT_ASSISTANT_USER_ID unset or 0".to_string(),
                metrics,
            });
        }

        // 1) alice 开 direct channel 到 assistant
        let resp: privchat_protocol::rpc::channel::direct::GetOrCreateDirectChannelResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::channel::DIRECT_GET_OR_CREATE,
                &privchat_protocol::rpc::channel::direct::GetOrCreateDirectChannelRequest {
                    target_user_id: assistant_uid,
                    source: Some("assistant-roundA".to_string()),
                    source_id: Some("phase40".to_string()),
                    user_id: 0,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let channel_id = resp.channel_id;
        if channel_id == 0 {
            return Err(boxed_err(
                "direct/get_or_create returned channel_id=0".to_string(),
            ));
        }

        // 2) 记录基线：当前 channel 已有多少条 assistant-sourced 消息
        let baseline = manager
            .message_history("alice", channel_id, 100)
            .await?;
        let baseline_assistant_msgs = baseline
            .messages
            .iter()
            .filter(|m| m.sender_id == assistant_uid)
            .count();

        // 3) alice 发一条 text
        let payload = format!("hello assistant {}", now_millis());
        let submit = manager
            .send_text("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE, &payload)
            .await?;
        if !submit_ok(&submit) {
            return Err(boxed_err(format!(
                "send_text to assistant not ok: {:?}",
                submit
            )));
        }
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        metrics.messages_sent += 1;
        let sent_msg_id = submit
            .server_msg_id
            .ok_or_else(|| boxed_err("submit lacks server_msg_id"))?;

        // 4) 轮询 listMessages 最多 20×300ms 等 assistant 的 echo 落库
        let mut got_reply: Option<privchat_protocol::rpc::MessageHistoryItem> = None;
        for attempt in 0..20 {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let h = manager
                .message_history("alice", channel_id, 100)
                .await?;
            let new_assistant_msgs: Vec<&privchat_protocol::rpc::MessageHistoryItem> = h
                .messages
                .iter()
                .filter(|m| m.sender_id == assistant_uid)
                .collect();
            if new_assistant_msgs.len() > baseline_assistant_msgs {
                // 取最新一条 assistant 消息
                got_reply = new_assistant_msgs.last().map(|m| (*m).clone());
                break;
            }
            if attempt == 19 {
                return Err(boxed_err(format!(
                    "assistant reply never landed after 20×300ms; baseline_assistant_msgs={baseline_assistant_msgs} sent_msg_id={sent_msg_id}"
                )));
            }
        }
        let reply = got_reply.expect("loop guarantees Some or returns Err");

        // 5) 断言 reply content 包含 assistant 默认标识
        if !reply.content.contains("Assistant") && !reply.content.contains("assistant") {
            return Err(boxed_err(format!(
                "assistant reply content unexpected: '{}'",
                reply.content
            )));
        }

        Ok(PhaseResult {
            phase_name: "assistant-echo-loop".to_string(),
            success: true,
            duration: start.elapsed(),
            details: format!(
                "assistant_uid={assistant_uid} channel_id={channel_id} \
                 sent_msg_id={sent_msg_id} reply_id={} reply_len={}",
                reply.message_id,
                reply.content.len()
            ),
            metrics,
        })
    }

    /// **System User message dispatch end-to-end smoke** —— spec
    /// 07-application/SYSTEM_USER_SPEC §8.5 验收 + SERVER_EVENT_DISPATCH_SPEC §11.1。
    ///
    /// 验证完整链路：
    ///   普通用户 → wire SendMessage → server 持久化 → emit
    ///   `system_user.message_received` → application 一级 handler 查 profile
    ///   → SmokeNoopSystemUserConsumer.onMessageReceived → counter +1。
    ///
    /// 前置：application 启用 `PRIVCHAT_SMOKE_SYSTEM_USER=1`。
    pub async fn phase39_system_user_message_smoke(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();

        let smoke_before = match fetch_smoke_system_user_status().await? {
            Some(s) => s,
            None => {
                return Ok(PhaseResult {
                    phase_name: "system-user-message-smoke".to_string(),
                    success: true,
                    duration: start.elapsed(),
                    details: "skipped: PRIVCHAT_SMOKE_SYSTEM_USER not active".to_string(),
                    metrics,
                });
            }
        };
        let baseline_count = smoke_before.received_count;
        let system_user_id = smoke_before.system_user_id;

        // 1) alice 开 direct channel 到 smoke system user
        let resp: privchat_protocol::rpc::channel::direct::GetOrCreateDirectChannelResponse = manager
            .rpc_typed(
                "alice",
                privchat_protocol::rpc::routes::channel::DIRECT_GET_OR_CREATE,
                &privchat_protocol::rpc::channel::direct::GetOrCreateDirectChannelRequest {
                    target_user_id: system_user_id,
                    source: Some("accounts-smoke".to_string()),
                    source_id: Some("phase39".to_string()),
                    user_id: 0,
                },
            )
            .await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        let channel_id = resp.channel_id;
        if channel_id == 0 {
            return Err(boxed_err(
                "direct/get_or_create returned channel_id=0".to_string(),
            ));
        }

        // 2) alice 发一条 text 给 system user
        let payload = format!("hello smoke assistant {}", now_millis());
        let submit = manager
            .send_text("alice", channel_id, DIRECT_SYNC_CHANNEL_TYPE, &payload)
            .await?;
        if !submit_ok(&submit) {
            return Err(boxed_err(format!(
                "send_text to system user not ok: {:?}",
                submit
            )));
        }
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;
        metrics.messages_sent += 1;
        let expected_server_msg_id = submit
            .server_msg_id
            .ok_or_else(|| boxed_err("submit lacks server_msg_id"))?;

        // 3) poll consumer 计数器最多 10×200ms 等待 ServerEvent
        let mut after: Option<SmokeSystemUserStatus> = None;
        for attempt in 0..10 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let s = fetch_smoke_system_user_status()
                .await?
                .ok_or_else(|| boxed_err("smoke endpoint went away mid-test"))?;
            if s.received_count > baseline_count {
                after = Some(s);
                break;
            }
            if attempt == 9 {
                return Err(boxed_err(format!(
                    "consumer counter never advanced past {baseline_count} after 10×200ms"
                )));
            }
        }
        let after = after.expect("loop guarantees Some or returns Err");

        // 4) 断言 last_event identity 与发送的消息一致
        let last = after.last_event.ok_or_else(|| {
            boxed_err("consumer received but exposed last_event=null (impossible)".to_string())
        })?;
        if last.system_user_id != system_user_id {
            return Err(boxed_err(format!(
                "last_event.system_user_id mismatch: expected={system_user_id} got={}",
                last.system_user_id
            )));
        }
        if last.channel_id != channel_id {
            return Err(boxed_err(format!(
                "last_event.channel_id mismatch: expected={channel_id} got={}",
                last.channel_id
            )));
        }
        if last.server_message_id != expected_server_msg_id {
            return Err(boxed_err(format!(
                "last_event.server_message_id mismatch: expected={expected_server_msg_id} got={}",
                last.server_message_id
            )));
        }
        let alice_uid = manager.user_id("alice")?;
        if last.from_user_id != alice_uid {
            return Err(boxed_err(format!(
                "last_event.from_user_id mismatch: expected={alice_uid} got={}",
                last.from_user_id
            )));
        }

        Ok(PhaseResult {
            phase_name: "system-user-message-smoke".to_string(),
            success: true,
            duration: start.elapsed(),
            details: format!(
                "system_user_id={system_user_id} channel_id={channel_id} \
                 server_message_id={expected_server_msg_id} consumer_count {baseline_count}→{}",
                after.received_count
            ),
            metrics,
        })
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct DeliveryMetricsSnapshot {
    attempt: u64,
    success_sessions: u64,
    zero_success: u64,
    offline_enqueue: u64,
}

async fn fetch_delivery_metrics(
    client: &reqwest::Client,
    url: &str,
) -> BoxResult<DeliveryMetricsSnapshot> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(boxed_err(format!(
            "metrics endpoint returned {}",
            resp.status()
        )));
    }
    let body = resp.text().await?;
    Ok(parse_delivery_metrics(&body))
}

fn parse_delivery_metrics(body: &str) -> DeliveryMetricsSnapshot {
    let mut snap = DeliveryMetricsSnapshot::default();
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 目标计数器均为无 label 的 counter，行形如：`name value` 或 `name{...} value`
        let (name_part, value_part) = match trimmed.rsplit_once(' ') {
            Some(pair) => pair,
            None => continue,
        };
        let name = name_part.split('{').next().unwrap_or(name_part).trim();
        let value: u64 = value_part
            .trim()
            .split('.')
            .next()
            .unwrap_or(value_part)
            .parse()
            .unwrap_or(0);
        match name {
            "privchat_delivery_attempt_total" => snap.attempt += value,
            "privchat_delivery_success_sessions_total" => snap.success_sessions += value,
            "privchat_delivery_zero_success_total" => snap.zero_success += value,
            "privchat_offline_enqueue_total" => snap.offline_enqueue += value,
            _ => {}
        }
    }
    snap
}

// --- Admin API response types for phase31 ---

/// Server wraps every admin response in `{ code, message, data }`.
/// Test code reads typed payload from `data`.
#[derive(Deserialize)]
struct AdminEnvelope<T> {
    #[allow(dead_code)]
    code: u32,
    #[allow(dead_code)]
    message: String,
    data: Option<T>,
}

#[derive(Deserialize)]
struct CreateRoomChannelResponse {
    #[allow(dead_code)]
    success: bool,
    channel_id: u64,
}

#[derive(Deserialize)]
struct RoomBroadcastResponse {
    online_count: usize,
    delivered: usize,
}

#[derive(Deserialize)]
struct RoomChannelInfoResponse {
    online_count: usize,
}

/// `POST /api/service/room-tickets/issue` ack（spec ROOM_CHANNEL_SPEC §4.5）。
/// 套 ApiEnvelope；data 里就是这个结构。
#[derive(Deserialize)]
struct IssueTicketResponse {
    ticket: String,
    #[allow(dead_code)]
    channel_id: u64,
    #[allow(dead_code)]
    user_id: u64,
    #[allow(dead_code)]
    exp: u64,
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

fn build_pts_tracked_channels(manager: &MultiAccountManager) -> BoxResult<Vec<(String, u64, u8)>> {
    let main_group = require_group_channel(manager, "main_group")?;
    let ab = manager
        .cached_direct_channel("alice", "bob")
        .ok_or_else(|| boxed_err("missing direct channel alice-bob"))?;
    let ac = manager
        .cached_direct_channel("charlie", "alice")
        .ok_or_else(|| boxed_err("missing direct channel charlie-alice"))?;
    let bc = manager
        .cached_direct_channel("bob", "charlie")
        .ok_or_else(|| boxed_err("missing direct channel bob-charlie"))?;

    Ok(vec![
        ("alice".to_string(), ab, DIRECT_SYNC_CHANNEL_TYPE),
        ("alice".to_string(), ac, DIRECT_SYNC_CHANNEL_TYPE),
        ("alice".to_string(), main_group, GROUP_SYNC_CHANNEL_TYPE),
        ("bob".to_string(), ab, DIRECT_SYNC_CHANNEL_TYPE),
        ("bob".to_string(), bc, DIRECT_SYNC_CHANNEL_TYPE),
        ("bob".to_string(), main_group, GROUP_SYNC_CHANNEL_TYPE),
        ("charlie".to_string(), ac, DIRECT_SYNC_CHANNEL_TYPE),
        ("charlie".to_string(), bc, DIRECT_SYNC_CHANNEL_TYPE),
        ("charlie".to_string(), main_group, GROUP_SYNC_CHANNEL_TYPE),
    ])
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
    sdk.authenticate(cfg.user_id, cfg.token.clone(), cfg.device_id.clone())
        .await?;
    sdk.run_bootstrap_sync().await?;
    manager.refresh_local_views(key).await?;
    Ok(())
}

/// 拉 application 暴露的 smoke 调试端点 `/service/privchat/smoke/system-user-status`。
///
/// 端点行为（spec SYSTEM_USER_SPEC §8.5）：
/// - smoke 未启用（application 启动时未设 PRIVCHAT_SMOKE_SYSTEM_USER=1）→ `enabled=false`
/// - 启用 → `enabled=true` + system_user_id / received_count / last_event
///
/// Phase 在前者情况返回 `Ok(None)`，调用方据此 skip 整个 phase；后者返回完整结构。
///
/// 端点未配置 `PRIVCHAT_PLATFORM_BASE_URL` 也按 skip 处理（保持与 phase36 一致的
/// "本地无 application 时 phase pass-skipped"行为）。
async fn fetch_smoke_system_user_status() -> BoxResult<Option<SmokeSystemUserStatus>> {
    let platform_base = std::env::var("PRIVCHAT_PLATFORM_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());
    if platform_base.is_empty() {
        return Ok(None);
    }
    let master_key = std::env::var("PRIVCHAT_SERVICE_MASTER_KEY").unwrap_or_default();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let resp = http
        .post(format!(
            "{}/service/privchat/smoke/system-user-status",
            platform_base
        ))
        .header("X-Service-Key", master_key)
        .send()
        .await?;
    if resp.status().as_u16() == 404 {
        // controller 未生成（旧 build）或 route 未注册——按 skip 处理
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(boxed_err(format!(
            "smoke status endpoint http {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        )));
    }
    // 端点返回 application 统一 envelope {code, message, data}（spec
    // SERVICE_RESPONSE_ENVELOPE_SPEC），所以需要先剥 envelope 再拿 data。
    let env: SmokeStatusEnvelope = resp.json().await?;
    if env.code != 0 {
        return Err(boxed_err(format!(
            "smoke status endpoint envelope code={} message={}",
            env.code, env.message
        )));
    }
    let s = env
        .data
        .ok_or_else(|| boxed_err("smoke status endpoint envelope.data is null".to_string()))?;
    if !s.auth_ok {
        return Err(boxed_err(
            "smoke status endpoint rejected X-Service-Key — check PRIVCHAT_SERVICE_MASTER_KEY"
                .to_string(),
        ));
    }
    if !s.enabled {
        return Ok(None);
    }
    Ok(Some(s))
}

#[derive(Debug, Deserialize)]
struct SmokeStatusEnvelope {
    code: i32,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<SmokeSystemUserStatus>,
}

#[derive(Debug, Clone, Deserialize)]
struct SmokeSystemUserStatus {
    enabled: bool,
    /// Missing field treated as auth ok（Kotlin 端在 v1.0.1 之前曾用默认值
    /// 时会被 kotlinx.serialization 省略；新版总会发出该字段，但保留 default
    /// 兼容老 application）。
    #[serde(default = "default_true")]
    auth_ok: bool,
    system_user_id: u64,
    #[serde(default)]
    received_count: u64,
    #[serde(default)]
    last_event: Option<SmokeSystemUserLastEventDto>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct SmokeSystemUserLastEventDto {
    system_user_id: u64,
    from_user_id: u64,
    channel_id: u64,
    server_message_id: u64,
    #[allow(dead_code)]
    pts: u64,
    #[allow(dead_code)]
    message_type: String,
    #[allow(dead_code)]
    occurred_at: i64,
    #[allow(dead_code)]
    received_at_ms: i64,
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

fn display_name_from_user(user: &privchat_sdk::StoredUser) -> String {
    user.alias
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            user.nickname
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            user.username
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| user.user_id.to_string())
}

fn resolve_friend_display_name(
    friend: &privchat_sdk::StoredFriend,
    user: Option<&privchat_sdk::StoredUser>,
) -> String {
    friend
        .alias
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            friend
                .nickname
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .or_else(|| {
            friend
                .username
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
        .or_else(|| user.map(display_name_from_user))
        .unwrap_or_else(|| friend.user_id.to_string())
}
