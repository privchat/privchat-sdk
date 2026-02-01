use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use crate::error::{PrivchatSDKError, Result};

/// 发送失败原因分类
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SendFailureReason {
    /// 网络超时 - 可重试
    NetworkTimeout,
    /// 网络不可用 - 等待恢复后重试
    NetworkUnavailable,
    /// 服务端错误 - 根据错误码决定
    ServerError(u16),
    /// 认证失败 - 需要重新登录
    AuthFailure,
    /// 限流 - 延迟重试
    RateLimited,
    /// 消息过大 - 不重试
    MessageTooLarge,
    /// 权限不足 - 不重试
    Forbidden,
    /// 未知错误
    Unknown(String),
}

impl SendFailureReason {
    /// 判断是否可以重试
    pub fn is_retryable(&self) -> bool {
        match self {
            SendFailureReason::NetworkTimeout => true,
            SendFailureReason::NetworkUnavailable => true,
            SendFailureReason::ServerError(code) => {
                // 5xx 服务端错误可重试，4xx 客户端错误不重试
                *code >= 500 && *code < 600
            }
            SendFailureReason::AuthFailure => true, // 重新认证后可重试
            SendFailureReason::RateLimited => true,
            SendFailureReason::MessageTooLarge => false,
            SendFailureReason::Forbidden => false,
            SendFailureReason::Unknown(_) => true, // 保守策略：未知错误可重试
        }
    }

    /// 获取重试延迟倍数
    pub fn get_delay_multiplier(&self) -> f64 {
        match self {
            SendFailureReason::NetworkTimeout => 1.0,
            SendFailureReason::NetworkUnavailable => 2.0,
            SendFailureReason::ServerError(_) => 1.5,
            SendFailureReason::AuthFailure => 0.5, // 认证失败快速重试
            SendFailureReason::RateLimited => 3.0, // 限流需要更长延迟
            _ => 1.0,
        }
    }
}

/// 重试策略配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// 最大重试次数
    pub max_retries: u32,
    /// 基础延迟时间（秒）
    pub base_delay_seconds: u64,
    /// 最大延迟时间（秒）
    pub max_delay_seconds: u64,
    /// 指数退避因子
    pub backoff_factor: f64,
    /// 随机抖动因子 (0.0-1.0)
    pub jitter_factor: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay_seconds: 1,
            max_delay_seconds: 300, // 5分钟
            backoff_factor: 2.0,
            jitter_factor: 0.1,
        }
    }
}

impl RetryPolicy {
    /// 计算下次重试时间
    pub fn calculate_next_retry_time(
        &self,
        retry_count: u32,
        failure_reason: &SendFailureReason,
    ) -> Option<u64> {
        if retry_count >= self.max_retries || !failure_reason.is_retryable() {
            return None;
        }

        // 基础延迟 = base_delay * (backoff_factor ^ retry_count)
        let base_delay = self.base_delay_seconds as f64 
            * self.backoff_factor.powf(retry_count as f64);
        
        // 应用失败原因的延迟倍数
        let adjusted_delay = base_delay * failure_reason.get_delay_multiplier();
        
        // 限制最大延迟
        let capped_delay = adjusted_delay.min(self.max_delay_seconds as f64);
        
        // 添加随机抖动
        let jitter = capped_delay * self.jitter_factor * (rand::random::<f64>() - 0.5);
        let final_delay = (capped_delay + jitter).max(0.0);
        
        // 计算下次重试的时间戳
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        Some(now + final_delay as u64)
    }

    /// 检查是否应该重试
    pub fn should_retry(&self, retry_count: u32, failure_reason: &SendFailureReason) -> bool {
        retry_count < self.max_retries && failure_reason.is_retryable()
    }
}

/// 重试状态管理器
#[derive(Debug, Clone)]
pub struct RetryManager {
    policy: RetryPolicy,
}

impl RetryManager {
    pub fn new(policy: RetryPolicy) -> Self {
        Self { policy }
    }

    /// 处理发送失败，返回是否应该重试和下次重试时间
    pub fn handle_send_failure(
        &self,
        current_retry_count: u32,
        failure_reason: SendFailureReason,
    ) -> Result<Option<u64>> {
        if !self.policy.should_retry(current_retry_count, &failure_reason) {
            return Ok(None);
        }

        match self.policy.calculate_next_retry_time(current_retry_count, &failure_reason) {
            Some(next_retry_time) => Ok(Some(next_retry_time)),
            None => Ok(None),
        }
    }

    /// 检查任务是否可以重试
    pub fn can_retry_now(&self, next_retry_time: u64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        now >= next_retry_time
    }
}

/// 从错误转换为失败原因
impl From<PrivchatSDKError> for SendFailureReason {
    fn from(error: PrivchatSDKError) -> Self {
        match error {
            PrivchatSDKError::Transport(msg) => {
                if msg.contains("timeout") {
                    SendFailureReason::NetworkTimeout
                } else if msg.contains("unavailable") || msg.contains("connection") {
                    SendFailureReason::NetworkUnavailable
                } else {
                    SendFailureReason::Unknown(msg)
                }
            }
            PrivchatSDKError::Auth(_) => SendFailureReason::AuthFailure,
            PrivchatSDKError::NotConnected => SendFailureReason::NetworkUnavailable,
            _ => SendFailureReason::Unknown(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_reason_retryable() {
        assert!(SendFailureReason::NetworkTimeout.is_retryable());
        assert!(SendFailureReason::NetworkUnavailable.is_retryable());
        assert!(SendFailureReason::ServerError(500).is_retryable());
        assert!(!SendFailureReason::ServerError(404).is_retryable());
        assert!(SendFailureReason::AuthFailure.is_retryable());
        assert!(SendFailureReason::RateLimited.is_retryable());
        assert!(!SendFailureReason::MessageTooLarge.is_retryable());
        assert!(!SendFailureReason::Forbidden.is_retryable());
    }

    #[test]
    fn test_retry_policy_calculation() {
        let policy = RetryPolicy::default();
        
        // 第一次重试
        let next_time = policy.calculate_next_retry_time(0, &SendFailureReason::NetworkTimeout);
        assert!(next_time.is_some());
        
        // 超过最大重试次数
        let next_time = policy.calculate_next_retry_time(10, &SendFailureReason::NetworkTimeout);
        assert!(next_time.is_none());
        
        // 不可重试的错误
        let next_time = policy.calculate_next_retry_time(0, &SendFailureReason::MessageTooLarge);
        assert!(next_time.is_none());
    }

    #[test]
    fn test_retry_manager() {
        let manager = RetryManager::new(RetryPolicy::default());
        
        // 可重试的情况
        let result = manager.handle_send_failure(0, SendFailureReason::NetworkTimeout);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
        
        // 不可重试的情况
        let result = manager.handle_send_failure(0, SendFailureReason::MessageTooLarge);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
        
        // 超过最大重试次数
        let result = manager.handle_send_failure(10, SendFailureReason::NetworkTimeout);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_can_retry_now() {
        let manager = RetryManager::new(RetryPolicy::default());
        
        // 过去的时间应该可以重试
        let past_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() - 10;
        assert!(manager.can_retry_now(past_time));
        
        // 未来的时间不应该重试
        let future_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() + 10;
        assert!(!manager.can_retry_now(future_time));
    }
} 