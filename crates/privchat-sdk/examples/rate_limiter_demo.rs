//! 客户端限流器演示
//! 
//! 本示例展示了如何使用客户端限流器来保护 IM 系统

use privchat_sdk::rate_limiter::{
    MessageRateLimiter, MessageRateLimiterConfig,
    RpcRateLimiter, RpcRateLimiterConfig,
    RpcRequestKey, RpcRateLimitError,
    ReconnectRateLimiter, ReconnectRateLimiterConfig,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[tokio::main]
async fn main() {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("\n==============================================");
    println!("🔐 客户端限流器演示");
    println!("==============================================\n");

    // 演示 1: 消息发送限流
    demo_message_rate_limiter().await;

    // 演示 2: RPC 去重和限流
    demo_rpc_rate_limiter().await;

    // 演示 3: 重连限流（指数退避）
    demo_reconnect_rate_limiter().await;

    println!("\n==============================================");
    println!("✅ 所有演示完成");
    println!("==============================================\n");
}

/// 演示 1: 消息发送限流
async fn demo_message_rate_limiter() {
    println!("\n📨 演示 1: 消息发送限流");
    println!("------------------------------------------");

    let config = MessageRateLimiterConfig {
        private_message_per_second: 5.0,  // 5 条/秒（演示用，实际推荐 10）
        group_message_per_second: 3.0,    // 3 条/秒（演示用，实际推荐 5）
        burst_multiplier: 2.0,
        min_send_interval_ms: 100,        // 100ms 最小间隔
    };

    let limiter = Arc::new(MessageRateLimiter::new(config));

    println!("\n📌 配置:");
    println!("  - 私聊限制: 5 条/秒");
    println!("  - 群聊限制: 3 条/秒");
    println!("  - 最小间隔: 100ms\n");

    // 场景 1: 正常发送（私聊）
    println!("场景 1: 正常私聊发送（1 条/秒）");
    for i in 1..=3 {
        match limiter.check_send(false) {
            Ok(()) => {
                println!("  ✅ 消息 #{} 发送成功", i);
            }
            Err(wait) => {
                println!("  ⏳ 消息 #{} 需要等待 {}ms", i, wait.as_millis());
                sleep(wait).await;
                println!("  ✅ 消息 #{} 发送成功（延迟后）", i);
            }
        }
        sleep(Duration::from_secs(1)).await;
    }

    // 场景 2: 快速发送（触发限流）
    println!("\n场景 2: 快速私聊发送（10 条，瞬间发送）");
    let start = Instant::now();
    
    for i in 1..=10 {
        match limiter.check_send(false) {
            Ok(()) => {
                println!("  ✅ 消息 #{} 发送成功（{}ms）", i, start.elapsed().as_millis());
            }
            Err(wait) => {
                println!("  ⏳ 消息 #{} 被限流，等待 {}ms", i, wait.as_millis());
                sleep(wait).await;
                println!("  ✅ 消息 #{} 发送成功（{}ms，延迟后）", i, start.elapsed().as_millis());
            }
        }
    }

    println!("\n  总耗时: {}ms", start.elapsed().as_millis());
    println!("  说明: 因为限流，10 条消息被平滑发送，避免了瞬间爆发");

    // 场景 3: 群聊 vs 私聊
    println!("\n场景 3: 对比私聊和群聊限制");
    
    // 重置限流器（等待令牌恢复）
    sleep(Duration::from_secs(2)).await;
    
    println!("  私聊连发 5 条:");
    for i in 1..=5 {
        if limiter.check_send(false).is_ok() {
            println!("    ✅ 私聊消息 #{} 通过", i);
        }
    }
    
    // 重置
    sleep(Duration::from_secs(2)).await;
    
    println!("\n  群聊连发 5 条:");
    for i in 1..=5 {
        match limiter.check_send(true) {
            Ok(()) => {
                println!("    ✅ 群聊消息 #{} 通过", i);
            }
            Err(_) => {
                println!("    ❌ 群聊消息 #{} 被限流", i);
            }
        }
    }

    println!("\n  说明: 群聊限制更严格（考虑 fan-out 成本）");

    // 统计信息
    let stats = limiter.stats();
    println!("\n📊 统计信息:");
    println!("  - 私聊可用令牌: {:.2}", stats.private_available_tokens);
    println!("  - 群聊可用令牌: {:.2}", stats.group_available_tokens);
    if let Some(elapsed) = stats.last_send_elapsed_ms {
        println!("  - 距上次发送: {}ms", elapsed);
    }
}

/// 演示 2: RPC 去重和限流
async fn demo_rpc_rate_limiter() {
    println!("\n\n🔌 演示 2: RPC 去重和限流");
    println!("------------------------------------------");

    let config = RpcRateLimiterConfig {
        global_rpc_per_second: 10.0,  // 10 次/秒（演示用，实际推荐 20）
        burst_multiplier: 2.0,
        request_timeout_seconds: 5,   // 5 秒超时（演示用，实际推荐 30）
        cleanup_interval_seconds: 2,
    };

    let limiter = RpcRateLimiter::new(config);

    println!("\n📌 配置:");
    println!("  - 全局限制: 10 次/秒");
    println!("  - 请求超时: 5 秒\n");

    // 场景 1: 正常 RPC 调用
    println!("场景 1: 正常 RPC 调用");
    let key1 = RpcRequestKey::new("contact.getFriendList", &serde_json::json!({}));
    
    match limiter.check_rpc(&key1) {
        Ok(()) => {
            println!("  ✅ 请求允许发送");
            // 模拟 RPC 调用
            sleep(Duration::from_millis(500)).await;
            limiter.mark_complete(&key1);
            println!("  ✅ 请求完成");
        }
        Err(e) => {
            println!("  ❌ 请求被拒绝: {}", e);
        }
    }

    // 场景 2: 重复请求（去重）
    println!("\n场景 2: 检测重复请求");
    let key2 = RpcRequestKey::new("contact.getFriendList", &serde_json::json!({}));
    
    // 第一次请求
    println!("  第 1 次请求: contact.getFriendList");
    match limiter.check_rpc(&key2) {
        Ok(()) => {
            println!("    ✅ 允许发送（标记为 pending）");
        }
        Err(e) => {
            println!("    ❌ 被拒绝: {}", e);
        }
    }

    // 第二次请求（重复）
    println!("  第 2 次请求: contact.getFriendList（重复）");
    match limiter.check_rpc(&key2) {
        Ok(()) => {
            println!("    ✅ 允许发送");
        }
        Err(RpcRateLimitError::DuplicateRequest { method, pending_since }) => {
            println!("    ❌ 检测到重复请求！");
            println!("       方法: {}", method);
            println!("       已等待: {:?}", pending_since);
        }
        Err(e) => {
            println!("    ❌ 被拒绝: {}", e);
        }
    }

    // 第三次请求（再次重复）
    println!("  第 3 次请求: contact.getFriendList（再次重复）");
    match limiter.check_rpc(&key2) {
        Ok(()) => {
            println!("    ✅ 允许发送");
        }
        Err(RpcRateLimitError::DuplicateRequest { method, pending_since }) => {
            println!("    ❌ 再次检测到重复请求！");
            println!("       方法: {}", method);
            println!("       已等待: {:?}", pending_since);
        }
        Err(e) => {
            println!("    ❌ 被拒绝: {}", e);
        }
    }

    println!("\n  说明: 同一个请求未返回时，不允许重复发送");
    println!("       这可以防止用户因为网络慢而多次点击按钮导致的重复请求");

    // 完成第一个请求
    sleep(Duration::from_millis(500)).await;
    limiter.mark_complete(&key2);
    println!("\n  原始请求完成，清除 pending 状态");

    // 现在可以再次发送
    println!("  第 4 次请求: contact.getFriendList（原始请求已完成）");
    match limiter.check_rpc(&key2) {
        Ok(()) => {
            println!("    ✅ 允许发送（pending 已清除）");
            limiter.mark_complete(&key2);
        }
        Err(e) => {
            println!("    ❌ 被拒绝: {}", e);
        }
    }

    // 场景 3: 频率限制
    println!("\n场景 3: RPC 频率限制（快速调用 15 次）");
    let start = Instant::now();
    
    for i in 1..=15 {
        let key = RpcRequestKey::new(
            format!("test.method_{}", i),
            &serde_json::json!({"index": i})
        );
        
        match limiter.check_rpc(&key) {
            Ok(()) => {
                println!("  ✅ RPC #{:2} 通过（{}ms）", i, start.elapsed().as_millis());
                // 立即标记完成
                limiter.mark_complete(&key);
            }
            Err(RpcRateLimitError::RateLimitExceeded { wait_duration, .. }) => {
                println!("  ⏳ RPC #{:2} 超限，等待 {}ms", i, wait_duration.as_millis());
                sleep(wait_duration).await;
                
                // 重试
                if limiter.check_rpc(&key).is_ok() {
                    println!("  ✅ RPC #{:2} 通过（{}ms，延迟后）", i, start.elapsed().as_millis());
                    limiter.mark_complete(&key);
                }
            }
            Err(e) => {
                println!("  ❌ RPC #{:2} 被拒绝: {}", i, e);
            }
        }
    }

    println!("\n  总耗时: {}ms", start.elapsed().as_millis());

    // 统计信息
    let stats = limiter.stats();
    println!("\n📊 统计信息:");
    println!("  - 总请求数: {}", stats.total_requests);
    println!("  - 完成请求数: {}", stats.completed_requests);
    println!("  - 拦截重复请求: {}", stats.duplicate_request_blocked);
    println!("  - 拦截超限请求: {}", stats.rate_limit_blocked);
    println!("  - 超时清理: {}", stats.timeout_cleaned);
    println!("  - 当前 Pending: {}", limiter.pending_count());
}

/// 演示 3: 重连限流（指数退避）
async fn demo_reconnect_rate_limiter() {
    println!("\n\n🔄 演示 3: 重连限流（指数退避）");
    println!("------------------------------------------");

    let config = ReconnectRateLimiterConfig {
        initial_interval_seconds: 1.0,
        max_interval_seconds: 16.0,  // 演示用（实际推荐 60）
        backoff_multiplier: 2.0,
        reset_after_success_seconds: 5,  // 演示用（实际推荐 60）
    };

    let limiter = ReconnectRateLimiter::new(config);

    println!("\n📌 配置:");
    println!("  - 初始间隔: 1 秒");
    println!("  - 最大间隔: 16 秒");
    println!("  - 退避倍数: 2.0（指数退避）");
    println!("  - 成功后重置: 5 秒\n");

    // 场景 1: 连续重连失败（触发指数退避）
    println!("场景 1: 连续重连失败（触发指数退避）");
    
    for i in 1..=6 {
        let start = Instant::now();
        
        match limiter.check_reconnect() {
            Ok(()) => {
                let stats = limiter.stats();
                println!(
                    "  尝试 #{}: ✅ 允许重连（下次间隔: {:.1}s）",
                    i,
                    stats.current_interval_seconds
                );
                
                // 模拟连接失败
                sleep(Duration::from_millis(100)).await;
                println!("    ❌ 连接失败");
            }
            Err(wait) => {
                println!("  尝试 #{}: ⏳ 需要等待 {:.1}s", i, wait.as_secs_f64());
                sleep(wait).await;
                
                // 重试
                if limiter.check_reconnect().is_ok() {
                    let stats = limiter.stats();
                    println!(
                        "    ✅ 允许重连（下次间隔: {:.1}s）",
                        stats.current_interval_seconds
                    );
                    
                    // 模拟连接失败
                    sleep(Duration::from_millis(100)).await;
                    println!("    ❌ 连接失败");
                }
            }
        }
        
        println!("      耗时: {:.1}s\n", start.elapsed().as_secs_f64());
    }

    let stats = limiter.stats();
    println!("📊 当前状态:");
    println!("  - 重连次数: {}", stats.reconnect_count);
    println!("  - 当前间隔: {:.1}s（已达到指数退避上限）", stats.current_interval_seconds);

    // 场景 2: 连接成功（重置间隔）
    println!("\n场景 2: 连接成功（重置间隔）");
    println!("  模拟: 等待一段时间后连接成功...");
    sleep(Duration::from_secs(2)).await;
    
    // 标记成功
    limiter.mark_success();
    println!("  ✅ 连接成功！重置重连间隔");

    let stats = limiter.stats();
    println!("\n📊 重置后状态:");
    println!("  - 重连次数: {} → 0", stats.reconnect_count);
    println!("  - 当前间隔: {:.1}s → 1.0s", stats.current_interval_seconds);

    // 场景 3: 成功后再次失败
    println!("\n场景 3: 成功后再次失败（从 1 秒重新开始退避）");
    
    // 等待一段时间（模拟稳定运行）
    sleep(Duration::from_secs(1)).await;
    
    // 再次失败
    println!("  模拟: 网络再次中断...");
    for i in 1..=3 {
        match limiter.check_reconnect() {
            Ok(()) => {
                let stats = limiter.stats();
                println!(
                    "  尝试 #{}: ✅ 允许重连（下次间隔: {:.1}s）",
                    i,
                    stats.current_interval_seconds
                );
                
                // 模拟连接失败
                sleep(Duration::from_millis(100)).await;
                println!("    ❌ 连接失败");
            }
            Err(wait) => {
                println!("  尝试 #{}: ⏳ 需要等待 {:.1}s", i, wait.as_secs_f64());
                sleep(wait).await;
                
                if limiter.check_reconnect().is_ok() {
                    let stats = limiter.stats();
                    println!(
                        "    ✅ 允许重连（下次间隔: {:.1}s）",
                        stats.current_interval_seconds
                    );
                    
                    sleep(Duration::from_millis(100)).await;
                    println!("    ❌ 连接失败");
                }
            }
        }
        println!();
    }

    println!("  说明: 指数退避从 1s 重新开始，避免了重连风暴");

    let final_stats = limiter.stats();
    println!("\n📊 最终统计:");
    println!("  - 总重连次数: {}", final_stats.reconnect_count);
    println!("  - 当前间隔: {:.1}s", final_stats.current_interval_seconds);
}
