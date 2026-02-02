//! 类型定义和数据结构

use serde::Deserialize;
use std::time::Duration;

// ========== RPC 响应结构体 ==========

#[derive(Debug, Deserialize)]
pub struct UserInfo {
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub id: u64,
    pub username: String,
    pub avatar_url: Option<String>,
    pub email: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginResponse {
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub user_id: u64,
    pub token: String,
    pub expires_at: String,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GroupInfo {
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub group_id: u64,
    pub name: String,
    pub description: Option<String>,
    pub member_count: u32,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct FriendInfo {
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub user_id: u64,
    pub username: String,
    pub status: String,
    pub added_at: String,
}

#[derive(Debug, Deserialize)]
pub struct MessageHistory {
    pub messages: Vec<HistoryMessage>,
    pub total_count: u64,
}

#[derive(Debug, Deserialize)]
pub struct HistoryMessage {
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub message_id: u64,
    #[serde(deserialize_with = "deserialize_u64_from_string")]
    pub sender: u64,
    pub content: String,
    pub timestamp: String,
    pub message_type: u8,
}

// 辅助函数：从字符串反序列化为 u64
fn deserialize_u64_from_string<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    s.parse::<u64>().map_err(serde::de::Error::custom)
}

// ========== 测试结果和报告 ==========

#[derive(Debug, Default)]
pub struct TestResults {
    pub total_phases: usize,
    pub successful_phases: usize,
    pub failed_phases: usize,
    pub total_duration: Duration,
    pub phase_results: Vec<PhaseResult>,
}

#[derive(Debug, Clone)]
pub struct PhaseResult {
    pub phase_name: String,
    pub success: bool,
    pub duration: Duration,
    pub details: String,
    pub metrics: PhaseMetrics,
}

#[derive(Debug, Default, Clone)]
pub struct PhaseMetrics {
    pub messages_sent: u32,
    pub messages_received: u32,
    pub rpc_calls: u32,
    pub rpc_successes: u32,
    pub errors: Vec<String>,
}

// ========== 账号配置 ==========

#[derive(Debug, Clone)]
pub struct AccountConfig {
    pub name: String,
    pub user_id: u64,
    pub token: String,
    pub data_dir: String,
    pub password: Option<String>, // 保存密码用于登录测试
    pub full_username: Option<String>, // 保存完整用户名（包含随机后缀）
    pub device_id: Option<String>, // 保存设备ID，确保注册和登录使用相同的设备ID
}

impl AccountConfig {
    /// 创建新的账号配置（使用临时目录）
    /// 注意：user_id 现在使用 u64 格式
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            user_id: 0, // 将由服务器分配
            token: format!("{}_token", name),
            data_dir: format!("./demo_data_multi/{}", name),
            password: None,
            full_username: None,
            device_id: None,
        }
    }

    /// 使用指定目录创建账号配置
    /// 注意：user_id 现在使用 u64 格式
    pub fn new_with_dir(name: &str, base_dir: &std::path::Path) -> Self {
        Self {
            name: name.to_string(),
            user_id: 0, // 将由服务器分配
            token: format!("{}_token", name),
            data_dir: base_dir.join(name).to_string_lossy().to_string(),
            password: None,
            full_username: None,
            device_id: None,
        }
    }
    
    /// 使用固定的 user_id 创建账号配置（用于测试，确保每次运行使用相同的 user_id）
    /// 注意：这仅用于测试目的，生产环境应该使用服务器分配的 user_id
    pub fn new_with_fixed_uuid(name: &str, user_id: u64) -> Self {
        Self {
            name: name.to_string(),
            user_id,
            token: format!("{}_token", name),
            data_dir: format!("./demo_data_multi/{}", name),
            password: None,
            full_username: None,
            device_id: None,
        }
    }
    
    /// 使用固定的 user_id 和指定目录创建账号配置
    pub fn new_with_fixed_uuid_and_dir(name: &str, user_id: u64, base_dir: &std::path::Path) -> Self {
        Self {
            name: name.to_string(),
            user_id,
            token: format!("{}_token", name),
            data_dir: base_dir.join(name).to_string_lossy().to_string(),
            password: None,
            full_username: None,
            device_id: None,
        }
    }
}

// ========== 测试配置 ==========

#[derive(Debug, Clone)]
pub struct TestConfig {
    pub phase_delay: Duration,
    pub message_delay: Duration,
    pub rpc_timeout: Duration,
    pub max_retries: u32,
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            phase_delay: Duration::from_millis(500),
            message_delay: Duration::from_millis(100),
            rpc_timeout: Duration::from_secs(5),
            max_retries: 3,
        }
    }
}