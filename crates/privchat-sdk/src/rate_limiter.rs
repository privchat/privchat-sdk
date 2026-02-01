//! 客户端限流模块
//! 
//! 本模块提供客户端侧的限流能力，是服务端安全防护的重要补充
//! 
//! ## 核心功能
//! 
//! 1. **消息发送频率限制** - 防止客户端疯狂发送消息
//! 2. **RPC 请求去重** - 同一请求未返回时不允许重复发送
//! 3. **RPC 频率限制** - 控制 RPC 调用频率
//! 4. **服务端限流响应处理** - 收到 429 后自动退避
//! 5. **重连频率限制** - 防止重连风暴
//! 
//! ## 设计理念
//! 
//! - **客户端限流 ≠ 不信任用户**，而是保护用户体验
//! - 防止客户端 bug 导致的"自我攻击"
//! - 减少无效请求，节省流量和电量
//! - 与服务端限流配合，形成完整防护体系
//! 
//! ## 限流参数（推荐值）
//! 
//! | 类型 | 限制 | 说明 |
//! |------|------|------|
//! | 消息发送 | 8-10 条/秒 | 私聊正常打字速度 |
//! | 消息发送（群聊）| 3-5 条/秒 | 群聊需考虑 fan-out |
//! | RPC 调用 | 20 次/秒 | 全局限制 |
//! | 重连尝试 | 1 次/秒 | 指数退避 |
//! | 批量 ACK | 50 条/批 | 减少请求数 |

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn, info};

/// 令牌桶限流器（线程安全）
#[derive(Debug)]
pub struct TokenBucket {
    /// 令牌容量
    capacity: f64,
    /// 当前令牌数
    tokens: f64,
    /// 每秒补充的令牌数
    refill_rate: f64,
    /// 上次补充时间
    last_refill: Instant,
}

impl TokenBucket {
    /// 创建新的令牌桶
    pub fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            capacity,
            tokens: capacity, // 初始满令牌
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// 尝试消耗指定数量的令牌
    /// 
    /// 返回：(是否成功, 需要等待的时间)
    pub fn try_consume(&mut self, tokens_needed: f64) -> (bool, Option<Duration>) {
        // 先补充令牌
        self.refill();

        if self.tokens >= tokens_needed {
            self.tokens -= tokens_needed;
            (true, None)
        } else {
            // 计算需要等待的时间
            let tokens_deficit = tokens_needed - self.tokens;
            let wait_duration = Duration::from_secs_f64(tokens_deficit / self.refill_rate);
            (false, Some(wait_duration))
        }
    }

    /// 补充令牌
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        
        if elapsed > 0.0 {
            let tokens_to_add = elapsed * self.refill_rate;
            self.tokens = (self.tokens + tokens_to_add).min(self.capacity);
            self.last_refill = now;
        }
    }

    /// 获取当前令牌数
    pub fn available_tokens(&mut self) -> f64 {
        self.refill();
        self.tokens
    }
}

/// 消息发送限流器配置
#[derive(Debug, Clone)]
pub struct MessageRateLimiterConfig {
    /// 私聊消息限制（条/秒）
    pub private_message_per_second: f64,
    /// 群聊消息限制（条/秒）
    pub group_message_per_second: f64,
    /// 突发容量倍数
    pub burst_multiplier: f64,
    /// 最小发送间隔（毫秒）- 防止过快发送
    pub min_send_interval_ms: u64,
}

impl Default for MessageRateLimiterConfig {
    fn default() -> Self {
        Self {
            private_message_per_second: 10.0,  // 私聊 10 条/秒
            group_message_per_second: 5.0,     // 群聊 5 条/秒
            burst_multiplier: 2.0,              // 允许 2 倍突发
            min_send_interval_ms: 50,           // 最小间隔 50ms（防止瞬间发送）
        }
    }
}

/// 消息发送限流器
/// 
/// 功能：
/// 1. 控制消息发送频率（私聊/群聊分别限制）
/// 2. 强制最小发送间隔（防止瞬间发送多条）
/// 3. 跟踪上次发送时间
#[derive(Debug)]
pub struct MessageRateLimiter {
    config: MessageRateLimiterConfig,
    /// 私聊令牌桶
    private_bucket: RwLock<TokenBucket>,
    /// 群聊令牌桶
    group_bucket: RwLock<TokenBucket>,
    /// 上次发送时间（用于强制最小间隔）
    last_send_time: RwLock<Option<Instant>>,
}

impl MessageRateLimiter {
    pub fn new(config: MessageRateLimiterConfig) -> Self {
        let private_capacity = config.private_message_per_second * config.burst_multiplier;
        let group_capacity = config.group_message_per_second * config.burst_multiplier;

        Self {
            private_bucket: RwLock::new(TokenBucket::new(
                private_capacity,
                config.private_message_per_second,
            )),
            group_bucket: RwLock::new(TokenBucket::new(
                group_capacity,
                config.group_message_per_second,
            )),
            last_send_time: RwLock::new(None),
            config,
        }
    }

    /// 检查是否可以发送消息
    /// 
    /// 参数：
    /// - is_group: 是否是群聊消息
    /// 
    /// 返回：
    /// - Ok(()) - 可以发送
    /// - Err(Duration) - 需要等待的时间
    pub fn check_send(&self, is_group: bool) -> Result<(), Duration> {
        // 1. 检查最小发送间隔
        if let Some(last_time) = *self.last_send_time.read() {
            let elapsed = Instant::now().duration_since(last_time);
            let min_interval = Duration::from_millis(self.config.min_send_interval_ms);
            
            if elapsed < min_interval {
                let wait_time = min_interval - elapsed;
                debug!(
                    "消息发送过快，需要等待 {}ms",
                    wait_time.as_millis()
                );
                return Err(wait_time);
            }
        }

        // 2. 检查令牌桶
        let bucket = if is_group {
            &self.group_bucket
        } else {
            &self.private_bucket
        };

        let (success, wait_duration) = bucket.write().try_consume(1.0);

        if !success {
            if let Some(wait) = wait_duration {
                warn!(
                    "消息发送超限（{}），需要等待 {}ms",
                    if is_group { "群聊" } else { "私聊" },
                    wait.as_millis()
                );
                return Err(wait);
            }
        }

        // 3. 更新上次发送时间
        *self.last_send_time.write() = Some(Instant::now());

        Ok(())
    }

    /// 记录发送成功（当前实现中在 check_send 时已更新，此方法为扩展预留）
    pub fn record_send_success(&self) {
        *self.last_send_time.write() = Some(Instant::now());
    }

    /// 获取统计信息
    pub fn stats(&self) -> MessageRateLimiterStats {
        MessageRateLimiterStats {
            private_available_tokens: self.private_bucket.write().available_tokens(),
            group_available_tokens: self.group_bucket.write().available_tokens(),
            last_send_elapsed_ms: self.last_send_time.read()
                .map(|t| Instant::now().duration_since(t).as_millis() as u64),
        }
    }
}

/// 消息限流器统计信息
#[derive(Debug, Clone)]
pub struct MessageRateLimiterStats {
    pub private_available_tokens: f64,
    pub group_available_tokens: f64,
    pub last_send_elapsed_ms: Option<u64>,
}

/// RPC 请求唯一标识符
/// 
/// 用于去重：同一个 RPC 方法 + 参数的组合视为同一个请求
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct RpcRequestKey {
    pub method: String,
    pub params_hash: u64,  // 参数的 hash 值
}

impl RpcRequestKey {
    pub fn new(method: impl Into<String>, params: &serde_json::Value) -> Self {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let method = method.into();
        let params_str = params.to_string();
        
        let mut hasher = DefaultHasher::new();
        params_str.hash(&mut hasher);
        let params_hash = hasher.finish();

        Self { method, params_hash }
    }
}

/// Pending RPC 请求记录
#[derive(Debug, Clone)]
struct PendingRequest {
    started_at: Instant,
    timeout: Duration,
}

/// RPC 限流器配置
#[derive(Debug, Clone)]
pub struct RpcRateLimiterConfig {
    /// 全局 RPC 限制（次/秒）
    pub global_rpc_per_second: f64,
    /// 突发容量倍数
    pub burst_multiplier: f64,
    /// 请求超时时间（秒）- 超时后自动清理 pending 状态
    pub request_timeout_seconds: u64,
    /// Pending 请求清理间隔（秒）
    pub cleanup_interval_seconds: u64,
}

impl Default for RpcRateLimiterConfig {
    fn default() -> Self {
        Self {
            global_rpc_per_second: 20.0,   // 全局 20 次/秒
            burst_multiplier: 2.0,
            request_timeout_seconds: 30,   // 30 秒超时
            cleanup_interval_seconds: 10,  // 10 秒清理一次
        }
    }
}

/// RPC 限流器
/// 
/// 功能：
/// 1. 控制 RPC 调用频率（全局限制）
/// 2. 防止重复请求（同一请求 pending 时不允许再次发送）
/// 3. 自动清理超时的 pending 请求
#[derive(Debug)]
pub struct RpcRateLimiter {
    config: RpcRateLimiterConfig,
    /// 全局令牌桶
    global_bucket: RwLock<TokenBucket>,
    /// Pending 请求记录（用于去重）
    pending_requests: RwLock<HashMap<RpcRequestKey, PendingRequest>>,
    /// 统计信息
    stats: RwLock<RpcRateLimiterStats>,
}

impl RpcRateLimiter {
    pub fn new(config: RpcRateLimiterConfig) -> Arc<Self> {
        let capacity = config.global_rpc_per_second * config.burst_multiplier;

        let limiter = Arc::new(Self {
            global_bucket: RwLock::new(TokenBucket::new(
                capacity,
                config.global_rpc_per_second,
            )),
            pending_requests: RwLock::new(HashMap::new()),
            stats: RwLock::new(RpcRateLimiterStats::default()),
            config,
        });

        // 启动清理任务
        let limiter_clone = limiter.clone();
        tokio::spawn(async move {
            limiter_clone.cleanup_loop().await;
        });

        limiter
    }

    /// 检查是否可以发送 RPC 请求
    /// 
    /// 参数：
    /// - request_key: 请求唯一标识
    /// 
    /// 返回：
    /// - Ok(()) - 可以发送
    /// - Err(RpcRateLimitError) - 被限流或重复请求
    pub fn check_rpc(&self, request_key: &RpcRequestKey) -> Result<(), RpcRateLimitError> {
        // 1. 检查是否重复请求
        {
            let pending = self.pending_requests.read();
            if let Some(pending_req) = pending.get(request_key) {
                let elapsed = Instant::now().duration_since(pending_req.started_at);
                
                // 如果请求还在有效期内，拒绝重复请求
                if elapsed < pending_req.timeout {
                    self.stats.write().duplicate_request_blocked += 1;
                    
                    warn!(
                        "检测到重复 RPC 请求: method={}, 已等待 {}ms",
                        request_key.method,
                        elapsed.as_millis()
                    );
                    
                    return Err(RpcRateLimitError::DuplicateRequest {
                        method: request_key.method.clone(),
                        pending_since: elapsed,
                    });
                }
            }
        }

        // 2. 检查令牌桶
        let (success, wait_duration) = self.global_bucket.write().try_consume(1.0);

        if !success {
            self.stats.write().rate_limit_blocked += 1;
            
            if let Some(wait) = wait_duration {
                warn!(
                    "RPC 请求超限: method={}, 需要等待 {}ms",
                    request_key.method,
                    wait.as_millis()
                );
                
                return Err(RpcRateLimitError::RateLimitExceeded {
                    method: request_key.method.clone(),
                    wait_duration: wait,
                });
            }
        }

        // 3. 记录为 pending
        self.pending_requests.write().insert(
            request_key.clone(),
            PendingRequest {
                started_at: Instant::now(),
                timeout: Duration::from_secs(self.config.request_timeout_seconds),
            },
        );

        self.stats.write().total_requests += 1;

        Ok(())
    }

    /// 标记 RPC 请求完成（成功或失败都要调用）
    pub fn mark_complete(&self, request_key: &RpcRequestKey) {
        if self.pending_requests.write().remove(request_key).is_some() {
            self.stats.write().completed_requests += 1;
            
            debug!(
                "RPC 请求完成: method={}",
                request_key.method
            );
        }
    }

    /// 清理超时的 pending 请求
    fn cleanup_expired(&self) {
        let mut pending = self.pending_requests.write();
        let now = Instant::now();
        
        let before_count = pending.len();
        
        pending.retain(|key, req| {
            let elapsed = now.duration_since(req.started_at);
            let should_keep = elapsed < req.timeout;
            
            if !should_keep {
                warn!(
                    "清理超时的 RPC 请求: method={}, 超时时间={}s",
                    key.method,
                    elapsed.as_secs()
                );
            }
            
            should_keep
        });
        
        let cleaned = before_count - pending.len();
        if cleaned > 0 {
            info!("清理了 {} 个超时的 pending RPC 请求", cleaned);
            self.stats.write().timeout_cleaned += cleaned as u64;
        }
    }

    /// 清理循环（后台任务）
    async fn cleanup_loop(&self) {
        let interval = Duration::from_secs(self.config.cleanup_interval_seconds);
        
        loop {
            tokio::time::sleep(interval).await;
            self.cleanup_expired();
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> RpcRateLimiterStats {
        self.stats.read().clone()
    }

    /// 获取当前 pending 请求数
    pub fn pending_count(&self) -> usize {
        self.pending_requests.read().len()
    }
}

/// RPC 限流错误
#[derive(Debug, Clone, thiserror::Error)]
pub enum RpcRateLimitError {
    #[error("重复请求: {method}, 已等待 {pending_since:?}")]
    DuplicateRequest {
        method: String,
        pending_since: Duration,
    },
    
    #[error("请求超限: {method}, 需要等待 {wait_duration:?}")]
    RateLimitExceeded {
        method: String,
        wait_duration: Duration,
    },
}

/// RPC 限流器统计信息
#[derive(Debug, Clone, Default)]
pub struct RpcRateLimiterStats {
    pub total_requests: u64,
    pub completed_requests: u64,
    pub duplicate_request_blocked: u64,
    pub rate_limit_blocked: u64,
    pub timeout_cleaned: u64,
}

/// 重连限流器配置
#[derive(Debug, Clone)]
pub struct ReconnectRateLimiterConfig {
    /// 初始重连间隔（秒）
    pub initial_interval_seconds: f64,
    /// 最大重连间隔（秒）
    pub max_interval_seconds: f64,
    /// 退避倍数
    pub backoff_multiplier: f64,
    /// 成功后重置等待时间（秒）
    pub reset_after_success_seconds: u64,
}

impl Default for ReconnectRateLimiterConfig {
    fn default() -> Self {
        Self {
            initial_interval_seconds: 1.0,  // 初始 1 秒
            max_interval_seconds: 15.0,     // 最大 15 秒，便于多轮重试（4×15s 比 1×60s 更易在网络恢复时连上）
            backoff_multiplier: 2.0,        // 指数退避：1s → 2s → 4s → 8s → 15s 封顶
            reset_after_success_seconds: 60, // 成功后 60 秒重置
        }
    }
}

/// 重连限流器
/// 
/// 功能：
/// 1. 控制重连频率（指数退避）
/// 2. 防止重连风暴
/// 3. 成功后自动重置等待时间
#[derive(Debug)]
pub struct ReconnectRateLimiter {
    config: ReconnectRateLimiterConfig,
    /// 当前重连间隔
    current_interval: RwLock<Duration>,
    /// 上次重连时间
    last_reconnect: RwLock<Option<Instant>>,
    /// 上次成功时间
    last_success: RwLock<Option<Instant>>,
    /// 重连次数
    reconnect_count: RwLock<u64>,
}

impl ReconnectRateLimiter {
    pub fn new(config: ReconnectRateLimiterConfig) -> Self {
        Self {
            current_interval: RwLock::new(Duration::from_secs_f64(config.initial_interval_seconds)),
            last_reconnect: RwLock::new(None),
            last_success: RwLock::new(None),
            reconnect_count: RwLock::new(0),
            config,
        }
    }

    /// 检查是否可以重连
    /// 
    /// 返回：
    /// - Ok(()) - 可以重连
    /// - Err(Duration) - 需要等待的时间
    pub fn check_reconnect(&self) -> Result<(), Duration> {
        // 检查是否需要重置（成功后一段时间自动重置）
        if let Some(last_success) = *self.last_success.read() {
            let elapsed = Instant::now().duration_since(last_success);
            let reset_duration = Duration::from_secs(self.config.reset_after_success_seconds);
            
            if elapsed >= reset_duration {
                info!("重连间隔已重置（距上次成功 {}s）", elapsed.as_secs());
                *self.current_interval.write() = Duration::from_secs_f64(self.config.initial_interval_seconds);
                *self.reconnect_count.write() = 0;
            }
        }

        // 检查是否需要等待
        if let Some(last_reconnect) = *self.last_reconnect.read() {
            let elapsed = Instant::now().duration_since(last_reconnect);
            let current_interval = *self.current_interval.read();
            
            if elapsed < current_interval {
                let wait_time = current_interval - elapsed;
                debug!(
                    "重连过快，需要等待 {}ms（当前退避间隔: {}s）",
                    wait_time.as_millis(),
                    current_interval.as_secs()
                );
                return Err(wait_time);
            }
        }

        // 允许重连
        *self.last_reconnect.write() = Some(Instant::now());
        *self.reconnect_count.write() += 1;

        // 增加退避时间（指数退避）
        let mut current = self.current_interval.write();
        let new_interval = Duration::from_secs_f64(
            current.as_secs_f64() * self.config.backoff_multiplier
        ).min(Duration::from_secs_f64(self.config.max_interval_seconds));
        
        info!(
            "重连尝试 #{}, 下次间隔: {}s",
            *self.reconnect_count.read(),
            new_interval.as_secs()
        );
        
        *current = new_interval;

        Ok(())
    }

    /// 标记重连成功
    pub fn mark_success(&self) {
        info!(
            "连接成功！重置重连计数器（之前尝试了 {} 次）",
            *self.reconnect_count.read()
        );
        
        *self.last_success.write() = Some(Instant::now());
        *self.reconnect_count.write() = 0;
        *self.current_interval.write() = Duration::from_secs_f64(self.config.initial_interval_seconds);
    }

    /// 获取统计信息
    pub fn stats(&self) -> ReconnectRateLimiterStats {
        ReconnectRateLimiterStats {
            reconnect_count: *self.reconnect_count.read(),
            current_interval_seconds: self.current_interval.read().as_secs_f64(),
            last_reconnect_elapsed_ms: self.last_reconnect.read()
                .map(|t| Instant::now().duration_since(t).as_millis() as u64),
        }
    }
}

/// 重连限流器统计信息
#[derive(Debug, Clone)]
pub struct ReconnectRateLimiterStats {
    pub reconnect_count: u64,
    pub current_interval_seconds: f64,
    pub last_reconnect_elapsed_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_bucket() {
        let mut bucket = TokenBucket::new(10.0, 5.0); // 容量10，每秒补充5个
        
        // 初始应该有 10 个令牌
        assert_eq!(bucket.available_tokens(), 10.0);
        
        // 消耗 5 个
        let (success, _) = bucket.try_consume(5.0);
        assert!(success);
        assert_eq!(bucket.available_tokens(), 5.0);
        
        // 再消耗 5 个
        let (success, _) = bucket.try_consume(5.0);
        assert!(success);
        assert_eq!(bucket.available_tokens(), 0.0);
        
        // 再消耗应该失败
        let (success, wait) = bucket.try_consume(1.0);
        assert!(!success);
        assert!(wait.is_some());
    }

    #[test]
    fn test_message_rate_limiter() {
        let config = MessageRateLimiterConfig {
            private_message_per_second: 10.0,
            group_message_per_second: 5.0,
            burst_multiplier: 2.0,
            min_send_interval_ms: 50,
        };
        
        let limiter = MessageRateLimiter::new(config);
        
        // 第一次发送应该成功
        assert!(limiter.check_send(false).is_ok());
        
        // 立即再次发送应该被最小间隔限制
        assert!(limiter.check_send(false).is_err());
    }

    #[tokio::test]
    async fn test_rpc_rate_limiter() {
        let config = RpcRateLimiterConfig::default();
        let limiter = RpcRateLimiter::new(config);
        
        let key = RpcRequestKey::new("test.method", &serde_json::json!({"param": "value"}));
        
        // 第一次请求应该成功
        assert!(limiter.check_rpc(&key).is_ok());
        
        // 立即重复请求应该被拒绝
        let result = limiter.check_rpc(&key);
        assert!(result.is_err());
        assert!(matches!(result, Err(RpcRateLimitError::DuplicateRequest { .. })));
        
        // 标记完成后可以再次请求
        limiter.mark_complete(&key);
        assert!(limiter.check_rpc(&key).is_ok());
    }
}
