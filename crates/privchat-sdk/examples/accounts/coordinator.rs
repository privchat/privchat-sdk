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
        Self { results: Vec::new() }
    }

    pub async fn run_all(&mut self, manager: &mut MultiAccountManager) -> BoxResult<()> {
        self.run_phase(TestPhases::phase1_auth_and_bootstrap(manager).await).await;
        self.run_phase(TestPhases::phase2_friend_flow(manager).await).await;
        self.run_phase(TestPhases::phase3_direct_chat(manager).await).await;
        self.run_phase(TestPhases::phase4_group_flow(manager).await).await;
        self.run_phase(TestPhases::phase5_reaction_and_read(manager).await).await;
        self.run_phase(TestPhases::phase6_blacklist(manager).await).await;
        self.run_phase(TestPhases::phase7_qrcode(manager).await).await;
        self.run_phase(TestPhases::phase8_presence_and_typing(manager).await).await;
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
