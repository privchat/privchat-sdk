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
        self.run_phase(TestPhases::phase1_auth_and_bootstrap(manager).await)
            .await;
        self.run_phase(TestPhases::phase2_friend_system(manager).await)
            .await;
        self.run_phase(TestPhases::phase3_group_system(manager).await)
            .await;
        self.run_phase(TestPhases::phase4_mixed_scenarios(manager).await)
            .await;
        self.run_phase(TestPhases::phase5_message_reception(manager).await)
            .await;
        self.run_phase(TestPhases::phase6_stickers(manager).await)
            .await;
        self.run_phase(TestPhases::phase7_channel_management(manager).await)
            .await;
        self.run_phase(TestPhases::phase8_read_receipts(manager).await)
            .await;
        self.run_phase(TestPhases::phase9_file_upload(manager).await)
            .await;
        self.run_phase(TestPhases::phase10_special_messages(manager).await)
            .await;
        self.run_phase(TestPhases::phase11_message_history(manager).await)
            .await;
        self.run_phase(TestPhases::phase12_message_revoke(manager).await)
            .await;
        self.run_phase(TestPhases::phase13_offline_messages(manager).await)
            .await;
        self.run_phase(TestPhases::phase14_pts_sync(manager).await)
            .await;
        self.run_phase(TestPhases::phase15_advanced_group(manager).await)
            .await;
        self.run_phase(TestPhases::phase16_message_reply(manager).await)
            .await;
        self.run_phase(TestPhases::phase17_reactions(manager).await)
            .await;
        self.run_phase(TestPhases::phase18_blacklist(manager).await)
            .await;
        self.run_phase(TestPhases::phase19_mentions(manager).await)
            .await;
        self.run_phase(TestPhases::phase20_stranger_messages(manager).await)
            .await;
        self.run_phase(TestPhases::phase21_online_presence(manager).await)
            .await;
        self.run_phase(TestPhases::phase22_typing_indicator(manager).await)
            .await;
        self.run_phase(TestPhases::phase23_system_notifications(manager).await)
            .await;
        self.run_phase(TestPhases::phase24_presence_system(manager).await)
            .await;
        self.run_phase(TestPhases::phase25_statistics(manager).await)
            .await;
        self.run_phase(TestPhases::phase26_login_test(manager).await)
            .await;
        self.run_phase(TestPhases::phase27_pts_offline_strict(manager).await)
            .await;
        self.run_phase(TestPhases::phase28_friend_display_name_rules(manager).await)
            .await;
        self.run_phase(TestPhases::phase29_channel_title_rules(manager).await)
            .await;
        self.run_phase(TestPhases::phase30_timeline_cache_local_first(manager).await)
            .await;
        self.run_phase(TestPhases::phase31_room(manager).await)
            .await;
        Ok(())
    }

    async fn run_phase(&mut self, result: BoxResult<PhaseResult>) {
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
                println!("[FAIL] phase runtime error: {e}");
                self.results.push(PhaseResult {
                    phase_name: "unknown".to_string(),
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
