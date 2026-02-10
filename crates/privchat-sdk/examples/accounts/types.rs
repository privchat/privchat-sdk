use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub key: String,
    pub username: String,
    pub password: String,
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
    pub data_dir: String,
}

#[derive(Debug, Default, Clone)]
pub struct PhaseMetrics {
    pub rpc_calls: u32,
    pub rpc_successes: u32,
    pub messages_sent: u32,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PhaseResult {
    pub phase_name: String,
    pub success: bool,
    pub duration: Duration,
    pub details: String,
    pub metrics: PhaseMetrics,
}

#[derive(Debug, Default)]
pub struct TestSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub duration: Duration,
    pub results: Vec<PhaseResult>,
}
