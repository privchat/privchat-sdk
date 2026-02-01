//! SDK 生命周期管理
//! 
//! 管理 App 前后台切换等一级生命周期事件，统一触发各模块的状态切换。

use crate::error::Result;
use tracing::{info, warn};
use std::sync::Arc;
use async_trait::async_trait;

/// 生命周期回调 Hook
/// 
/// 各模块通过实现此 trait 来响应生命周期变化
#[async_trait]
pub trait LifecycleHook: Send + Sync {
    /// App 切换到后台时调用
    async fn on_background(&self) -> Result<()>;
    
    /// App 切换到前台时调用
    async fn on_foreground(&self) -> Result<()>;
}

/// 生命周期管理器
pub struct LifecycleManager {
    hooks: Vec<Arc<dyn LifecycleHook>>,
}

impl LifecycleManager {
    pub fn new() -> Self {
        Self {
            hooks: Vec::new(),
        }
    }
    
    /// 获取已注册的 Hook 数量（用于检查是否已注册）
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    
    /// 注册生命周期回调 Hook
    pub fn register_hook(&mut self, hook: Arc<dyn LifecycleHook>) {
        self.hooks.push(hook);
        info!("✅ 生命周期 Hook 已注册: 当前共 {} 个", self.hooks.len());
    }
    
    /// 通知所有 Hook：App 切换到后台
    /// 
    /// 按注册顺序执行，如果某个 Hook 失败，会记录错误但继续执行其他 Hook
    pub async fn notify_background(&self) -> Result<()> {
        info!("🔄 通知所有模块：App 切换到后台");
        
        let mut errors = Vec::new();
        
        for (index, hook) in self.hooks.iter().enumerate() {
            if let Err(e) = hook.on_background().await {
                warn!("⚠️ Hook #{} 后台切换失败: {}", index, e);
                errors.push(e);
                // 继续执行其他模块
            }
        }
        
        if !errors.is_empty() {
            warn!("⚠️ {} 个模块后台切换失败，但所有模块都已尝试执行", errors.len());
            // 返回第一个错误，但所有模块都已尝试执行
            // 注意：由于 PrivchatSDKError 可能没有实现 Clone，我们使用第一个错误
            return Err(errors.into_iter().next().unwrap());
        }
        
        info!("✅ 所有模块后台切换完成");
        Ok(())
    }
    
    /// 通知所有 Hook：App 切换到前台
    /// 
    /// 按注册顺序执行，如果某个 Hook 失败，会记录错误但继续执行其他 Hook
    pub async fn notify_foreground(&self) -> Result<()> {
        info!("🔄 通知所有模块：App 切换到前台");
        
        let mut errors = Vec::new();
        
        for (index, hook) in self.hooks.iter().enumerate() {
            if let Err(e) = hook.on_foreground().await {
                warn!("⚠️ Hook #{} 前台切换失败: {}", index, e);
                errors.push(e);
                // 继续执行其他模块
            }
        }
        
        if !errors.is_empty() {
            warn!("⚠️ {} 个模块前台切换失败，但所有模块都已尝试执行", errors.len());
            // 返回第一个错误，但所有模块都已尝试执行
            // 注意：由于 PrivchatSDKError 可能没有实现 Clone，我们使用第一个错误
            return Err(errors.into_iter().next().unwrap());
        }
        
        info!("✅ 所有模块前台切换完成");
        Ok(())
    }
}

impl Default for LifecycleManager {
    fn default() -> Self {
        Self::new()
    }
}

// Push Hook 模块（SDK 内部自动注册）
mod push_hook;
pub use push_hook::PushLifecycleHook;
