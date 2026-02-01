//! Push 模块生命周期 Hook
//! 
//! 在 App 前后台切换时，自动更新设备推送状态。

use crate::error::Result;
use crate::lifecycle::LifecycleHook;
use crate::sdk::PrivchatSDK;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

/// Push 模块生命周期 Hook
/// 
/// 在 App 前后台切换时，自动更新设备推送状态。
pub struct PushLifecycleHook {
    sdk: Arc<PrivchatSDK>,
}

impl PushLifecycleHook {
    /// 创建新的 Push 生命周期 Hook
    pub fn new(sdk: Arc<PrivchatSDK>) -> Self {
        Self {
            sdk,
        }
    }
    
    /// 获取当前设备 ID
    async fn get_device_id(&self) -> Option<String> {
        // 通过 SDK 的公开方法获取连接状态
        let state = self.sdk.get_connection_state().await;
        state.user.as_ref().map(|u| u.device_id.clone())
    }
}

#[async_trait]
impl LifecycleHook for PushLifecycleHook {
    /// App 切换到后台时调用
    /// 
    /// 更新设备推送状态：apns_armed = true
    async fn on_background(&self) -> Result<()> {
        // 获取当前设备 ID
        let device_id = match self.get_device_id().await {
            Some(id) => id,
            None => {
                warn!("[Push Hook] 无法获取设备 ID，跳过推送状态更新");
                return Ok(());
            }
        };
        
        info!("[Push Hook] App 切换到后台，更新推送状态: device_id={}, apns_armed=true", device_id);
        
        // 更新设备推送状态
        match self.sdk.update_device_push_state(&device_id, true, None).await {
            Ok(response) => {
                info!("[Push Hook] ✅ 推送状态已更新: apns_armed={}, user_push_enabled={}", 
                      response.apns_armed, response.user_push_enabled);
                Ok(())
            }
            Err(e) => {
                warn!("[Push Hook] ⚠️ 更新推送状态失败: {} (可能未连接)", e);
                // 即使失败也返回 Ok，因为这不是致命错误
                // 连接可能已断开，但状态会在下次连接时同步
                Ok(())
            }
        }
    }
    
    /// App 切换到前台时调用
    /// 
    /// 更新设备推送状态：apns_armed = false
    async fn on_foreground(&self) -> Result<()> {
        // 获取当前设备 ID
        let device_id = match self.get_device_id().await {
            Some(id) => id,
            None => {
                warn!("[Push Hook] 无法获取设备 ID，跳过推送状态更新");
                return Ok(());
            }
        };
        
        info!("[Push Hook] App 切换到前台，更新推送状态: device_id={}, apns_armed=false", device_id);
        
        // 更新设备推送状态
        match self.sdk.update_device_push_state(&device_id, false, None).await {
            Ok(response) => {
                info!("[Push Hook] ✅ 推送状态已更新: apns_armed={}, user_push_enabled={}", 
                      response.apns_armed, response.user_push_enabled);
                Ok(())
            }
            Err(e) => {
                warn!("[Push Hook] ⚠️ 更新推送状态失败: {} (可能未连接)", e);
                // 即使失败也返回 Ok，因为这不是致命错误
                Ok(())
            }
        }
    }
}
