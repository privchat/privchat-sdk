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

use crate::account_manager::MultiAccountManager;
use crate::phases::TestPhases;
use crate::types::{PhaseResult, TestSummary};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

pub struct TestCoordinator {
    results: Vec<PhaseResult>,
}

impl TestCoordinator {
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    pub async fn run_all(&mut self, manager: &mut MultiAccountManager) -> BoxResult<()> {
        if self.enabled("phase1_auth_and_bootstrap") {
            self.run_phase("phase1_auth_and_bootstrap", TestPhases::phase1_auth_and_bootstrap(manager).await)
                .await;
        }
        if self.enabled("phase2_friend_system") {
            self.run_phase("phase2_friend_system", TestPhases::phase2_friend_system(manager).await)
                .await;
        }
        if self.enabled("phase3_group_system") {
            self.run_phase("phase3_group_system", TestPhases::phase3_group_system(manager).await)
                .await;
        }
        if self.enabled("phase4_mixed_scenarios") {
            self.run_phase("phase4_mixed_scenarios", TestPhases::phase4_mixed_scenarios(manager).await)
                .await;
        }
        if self.enabled("phase5_message_reception") {
            self.run_phase("phase5_message_reception", TestPhases::phase5_message_reception(manager).await)
                .await;
        }
        if self.enabled("phase6_stickers") {
            self.run_phase("phase6_stickers", TestPhases::phase6_stickers(manager).await)
                .await;
        }
        if self.enabled("phase7_channel_management") {
            self.run_phase("phase7_channel_management", TestPhases::phase7_channel_management(manager).await)
                .await;
        }
        if self.enabled("phase8_read_receipts") {
            self.run_phase("phase8_read_receipts", TestPhases::phase8_read_receipts(manager).await)
                .await;
        }
        if self.enabled("phase9_file_upload") {
            self.run_phase("phase9_file_upload", TestPhases::phase9_file_upload(manager).await)
                .await;
        }
        if self.enabled("phase10_special_messages") {
            self.run_phase("phase10_special_messages", TestPhases::phase10_special_messages(manager).await)
                .await;
        }
        if self.enabled("phase11_message_history") {
            self.run_phase("phase11_message_history", TestPhases::phase11_message_history(manager).await)
                .await;
        }
        if self.enabled("phase12_message_revoke") {
            self.run_phase("phase12_message_revoke", TestPhases::phase12_message_revoke(manager).await)
                .await;
        }
        if self.enabled("phase13_offline_messages") {
            self.run_phase("phase13_offline_messages", TestPhases::phase13_offline_messages(manager).await)
                .await;
        }
        if self.enabled("phase14_pts_sync") {
            self.run_phase("phase14_pts_sync", TestPhases::phase14_pts_sync(manager).await)
                .await;
        }
        if self.enabled("phase15_advanced_group") {
            self.run_phase("phase15_advanced_group", TestPhases::phase15_advanced_group(manager).await)
                .await;
        }
        if self.enabled("phase16_message_reply") {
            self.run_phase("phase16_message_reply", TestPhases::phase16_message_reply(manager).await)
                .await;
        }
        if self.enabled("phase17_reactions") {
            self.run_phase("phase17_reactions", TestPhases::phase17_reactions(manager).await)
                .await;
        }
        if self.enabled("phase18_blacklist") {
            self.run_phase("phase18_blacklist", TestPhases::phase18_blacklist(manager).await)
                .await;
        }
        if self.enabled("phase19_mentions") {
            self.run_phase("phase19_mentions", TestPhases::phase19_mentions(manager).await)
                .await;
        }
        if self.enabled("phase20_stranger_messages") {
            self.run_phase("phase20_stranger_messages", TestPhases::phase20_stranger_messages(manager).await)
                .await;
        }
        if self.enabled("phase21_online_presence") {
            self.run_phase("phase21_online_presence", TestPhases::phase21_online_presence(manager).await)
                .await;
        }
        if self.enabled("phase22_typing_indicator") {
            self.run_phase("phase22_typing_indicator", TestPhases::phase22_typing_indicator(manager).await)
                .await;
        }
        if self.enabled("phase23_system_notifications") {
            self.run_phase("phase23_system_notifications", TestPhases::phase23_system_notifications(manager).await)
                .await;
        }
        if self.enabled("phase24_presence_system") {
            self.run_phase("phase24_presence_system", TestPhases::phase24_presence_system(manager).await)
                .await;
        }
        if self.enabled("phase25_statistics") {
            self.run_phase("phase25_statistics", TestPhases::phase25_statistics(manager).await)
                .await;
        }
        if self.enabled("phase26_login_test") {
            self.run_phase("phase26_login_test", TestPhases::phase26_login_test(manager).await)
                .await;
        }
        if self.enabled("phase27_pts_offline_strict") {
            self.run_phase("phase27_pts_offline_strict", TestPhases::phase27_pts_offline_strict(manager).await)
                .await;
        }
        if self.enabled("phase28_friend_display_name_rules") {
            self.run_phase("phase28_friend_display_name_rules", TestPhases::phase28_friend_display_name_rules(manager).await)
                .await;
        }
        if self.enabled("phase29_channel_title_rules") {
            self.run_phase("phase29_channel_title_rules", TestPhases::phase29_channel_title_rules(manager).await)
                .await;
        }
        if self.enabled("phase30_timeline_cache_local_first") {
            self.run_phase("phase30_timeline_cache_local_first", TestPhases::phase30_timeline_cache_local_first(manager).await)
                .await;
        }
        if self.enabled("phase31_room") {
            self.run_phase("phase31_room", TestPhases::phase31_room(manager).await)
                .await;
        }
        if self.enabled("phase32_channel_state_resume_smoke") {
            self.run_phase("phase32_channel_state_resume_smoke", TestPhases::phase32_channel_state_resume_smoke(manager).await)
                .await;
        }
        if self.enabled("phase33_unread_resume_strict") {
            self.run_phase("phase33_unread_resume_strict", TestPhases::phase33_unread_resume_strict(manager).await)
                .await;
        }
        if self.enabled("phase34_admin_push_online") {
            self.run_phase("phase34_admin_push_online", TestPhases::phase34_admin_push_online(manager).await)
                .await;
        }
        if self.enabled("phase35_admin_revoke_online") {
            self.run_phase("phase35_admin_revoke_online", TestPhases::phase35_admin_revoke_online(manager).await)
                .await;
        }
        if self.enabled("phase36_platform_bot_followed") {
            self.run_phase("phase36_platform_bot_followed", TestPhases::phase36_platform_bot_followed(manager).await)
                .await;
        }
        if self.enabled("phase37_fsync_friend_request_lifecycle") {
            self.run_phase("phase37_fsync_friend_request_lifecycle", TestPhases::phase37_fsync_friend_request_lifecycle(manager).await)
                .await;
        }
        if self.enabled("phase38_system_user_group_reject") {
            self.run_phase("phase38_system_user_group_reject", TestPhases::phase38_system_user_group_reject(manager).await)
                .await;
        }
        if self.enabled("phase39_system_user_message_smoke") {
            self.run_phase("phase39_system_user_message_smoke", TestPhases::phase39_system_user_message_smoke(manager).await)
                .await;
        }
        if self.enabled("phase40_assistant_echo_loop") {
            self.run_phase("phase40_assistant_echo_loop", TestPhases::phase40_assistant_echo_loop(manager).await)
                .await;
        }
        if self.enabled("phase41_outbox_text_durability") {
            self.run_phase("phase41_outbox_text_durability", TestPhases::phase41_outbox_text_durability(manager).await)
                .await;
        }
        if self.enabled("phase42_outbox_attachment_e2e") {
            self.run_phase("phase42_outbox_attachment_e2e", TestPhases::phase42_outbox_attachment_e2e(manager).await)
                .await;
        }
        if self.enabled("phase44_attachment_fidelity_e2e") {
            self.run_phase("phase44_attachment_fidelity_e2e", TestPhases::phase44_attachment_fidelity_e2e(manager).await)
                .await;
        }
        if self.enabled("phase45_resend_received_attachment") {
            self.run_phase("phase45_resend_received_attachment", TestPhases::phase45_resend_received_attachment(manager).await)
                .await;
        }
        if self.enabled("phase43_outbox_survives_restart") {
            self.run_phase("phase43_outbox_survives_restart", TestPhases::phase43_outbox_survives_restart(manager).await)
                .await;
        }
        Ok(())
    }

    /// `name` 是**调用点**传进来的 phase 名。
    ///
    /// 之前失败路径把它记成 "unknown"：一个不说明自己是什么的失败，等于没报——
    /// 排查时只能靠猜是 43 个 phase 里的哪一个。
    /// 跑哪些 phase。`PRIVCHAT_PHASES` 是逗号分隔的名字，缺省全跑。
    ///
    /// 稳定性门禁要连跑几十次单个 phase：整套跑一次两分钟，没有这个开关就做不到。
    fn enabled(&self, name: &str) -> bool {
        match std::env::var("PRIVCHAT_PHASES") {
            Ok(list) if !list.trim().is_empty() => {
                list.split(',').any(|want| want.trim() == name)
            }
            _ => true,
        }
    }

    async fn run_phase(&mut self, name: &str, result: BoxResult<PhaseResult>) {
        match result {
            Ok(r) => {
                let status = if r.success { "PASS" } else { "FAIL" };
                println!(
                    "[{status}] {:<18} | {:>4} ms | {}",
                    r.phase_name,
                    r.duration.as_millis(),
                    r.details
                );
                if !r.metrics.errors.is_empty() {
                    for e in &r.metrics.errors {
                        println!("  - {e}");
                    }
                }
                self.results.push(r);
            }
            Err(e) => {
                println!("[FAIL] {name} | phase runtime error: {e}");
                self.results.push(PhaseResult {
                    phase_name: name.to_string(),
                    success: false,
                    duration: std::time::Duration::from_millis(0),
                    details: e.to_string(),
                    metrics: Default::default(),
                });
            }
        }
    }

    pub fn summary(&self, duration: std::time::Duration) -> TestSummary {
        let total = self.results.len();
        let passed = self.results.iter().filter(|r| r.success).count();
        TestSummary {
            total,
            passed,
            failed: total.saturating_sub(passed),
            duration,
            results: self.results.clone(),
        }
    }
}
