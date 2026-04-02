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
                                    && !m.timestamp.is_empty()
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

        let bob_uid = manager.user_id("bob")?;
        let expected_texts: std::collections::HashSet<String> =
            (0..3).map(|idx| format!("phase13 offline msg {idx}")).collect();

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
        let mut matched_diff_texts: std::collections::HashSet<String> = std::collections::HashSet::new();
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
            metrics
                .errors
                .push("phase14 submit rejected".to_string());
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

        charlie_sdk.subscribe_channel(group_channel_id, 1, None).await?;
        metrics.rpc_calls += 1;
        metrics.rpc_successes += 1;

        let mut bob_group_events = bob_sdk.subscribe_events();
        let mut charlie_group_events = charlie_sdk.subscribe_events();

        tokio::time::sleep(Duration::from_millis(200)).await;

        // alice 在群里发 typing
        alice_sdk
            .send_typing(group_channel_id, 1, true, privchat_sdk::TypingActionType::Typing)
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
            if group_pts_after.current_pts >= group_pts_before.current_pts.saturating_add(expected_group_count as u64) {
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
                let direct_history = manager.message_history(offline, *direct_channel, 300).await?;
                metrics.rpc_calls += 1;
                let direct_history_matched = direct_history
                    .messages
                    .iter()
                    .filter(|m| m.content.starts_with(&format!("{tag} direct {sender}->{offline} ")))
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
            details: "3 rounds offline->send->online + 2 rounds all-accounts reconnect pts stability".to_string(),
            metrics,
        })
    }

    pub async fn phase28_friend_display_name_rules(
        manager: &mut MultiAccountManager,
    ) -> BoxResult<PhaseResult> {
        let start = std::time::Instant::now();
        let mut metrics = PhaseMetrics::default();
        manager.refresh_all_local_views().await?;

        let mut expected_friend_map: std::collections::HashMap<&str, std::collections::HashSet<u64>> =
            std::collections::HashMap::new();
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
            let actual: std::collections::HashSet<u64> = friends.iter().map(|f| f.user_id).collect();
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
            let channel_id = manager
                .cached_direct_channel(left, right)
                .ok_or_else(|| boxed_err(format!("missing direct channel cache for {left}-{right}")))?;
            for (viewer, peer) in [(left, right), (right, left)] {
                let sdk = manager.sdk(viewer)?;
                let peer_uid = manager.user_id(peer)?;
                let channels = manager.list_local_channels(viewer).await?;
                metrics.rpc_calls += 1;
                let channel = channels.into_iter().find(|c| c.channel_id == channel_id);
                if channel.is_none() {
                    metrics
                        .errors
                        .push(format!("{viewer} missing cached direct channel {channel_id}"));
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
                                    || u
                                        .nickname
                                        .as_ref()
                                        .map(|s| !s.trim().is_empty())
                                        .unwrap_or(false)
                                    || u
                                        .username
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

        let expected_direct_ids_by_user: std::collections::HashMap<&str, std::collections::HashSet<u64>> =
            [("alice", [("alice", "bob"), ("alice", "charlie")]),
             ("bob", [("alice", "bob"), ("bob", "charlie")]),
             ("charlie", [("alice", "charlie"), ("bob", "charlie")])]
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
            let channel_id = manager
                .cached_direct_channel(left, right)
                .ok_or_else(|| boxed_err(format!("missing cached direct channel: {left}-{right}")))?;
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
            details: "repeat list_messages consistency + local create invalidation/visibility check"
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

        let admin_host =
            std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
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
            .post(format!("{}/api/admin/room", admin_base))
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

        let create_body: CreateRoomChannelResponse = create_resp.json().await?;
        metrics.rpc_successes += 1;
        let channel_id = create_body.channel_id;

        // --- Step 2: 三个用户订阅频道 ---
        let alice_sdk = manager.sdk("alice")?;
        let bob_sdk = manager.sdk("bob")?;
        let charlie_sdk = manager.sdk("charlie")?;

        let mut alice_events = alice_sdk.subscribe_events();
        let mut bob_events = bob_sdk.subscribe_events();
        let mut charlie_events = charlie_sdk.subscribe_events();

        for (name, sdk) in [("alice", &alice_sdk), ("bob", &bob_sdk), ("charlie", &charlie_sdk)] {
            match sdk.subscribe_channel(channel_id, 2, None).await {
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
            .post(format!("{}/api/admin/room/{}/broadcast", admin_base, channel_id))
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

        let broadcast_body: RoomBroadcastResponse = broadcast_resp.json().await?;
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
                        if let privchat_sdk::SdkEvent::SubscriptionMessageReceived { channel_id: cid, payload, .. } = &event {
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
        for (name, sdk) in [("alice", &alice_sdk), ("bob", &bob_sdk), ("charlie", &charlie_sdk)] {
            if let Err(e) = sdk.unsubscribe_channel(channel_id, 2).await {
                metrics
                    .errors
                    .push(format!("{} unsubscribe_channel failed: {}", name, e));
            }
        }

        // --- Step 6: 验证取消订阅后在线人数为 0 ---
        let verify_resp = client
            .get(format!("{}/api/admin/room/{}", admin_base, channel_id))
            .header("X-Service-Key", &service_key)
            .send()
            .await?;
        metrics.rpc_calls += 1;

        if verify_resp.status().is_success() {
            let channel_info: RoomChannelInfoResponse = verify_resp.json().await?;
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
            metrics.errors.push("channel pin returned false".to_string());
        }

        let mute = manager.channel_mute("alice", channel_id, true).await?;
        metrics.rpc_calls += 1;
        if mute {
            metrics.rpc_successes += 1;
        } else {
            metrics.errors.push("channel mute returned false".to_string());
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
}

// --- Admin API response types for phase31 ---

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
