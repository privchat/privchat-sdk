/// pts 管理器
/// 
/// 职责：
/// - 存储和更新每个频道的本地 pts
/// - 检测 pts 间隙
/// - 提供 pts 查询接口

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::error::Result;
use crate::storage::StorageManager;

/// pts 管理器
#[derive(Clone)]
pub struct PtsManager {
    /// 存储管理器
    storage: Arc<StorageManager>,
    
    /// 内存缓存：(channel_id, channel_type) -> pts
    cache: Arc<RwLock<HashMap<(u64, u8), u64>>>,
}

impl PtsManager {
    /// 创建 pts 管理器
    pub fn new(storage: Arc<StorageManager>) -> Self {
        Self {
            storage,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 获取频道的本地 pts
    pub async fn get_local_pts(&self, channel_id: u64, channel_type: u8) -> Result<u64> {
        // 先查缓存
        {
            let cache = self.cache.read().await;
            if let Some(&pts) = cache.get(&(channel_id, channel_type)) {
                return Ok(pts);
            }
        }
        
        // 缓存未命中，查询数据库
        let pts = self.get_pts_from_db(channel_id, channel_type).await?;
        
        // 更新缓存
        {
            let mut cache = self.cache.write().await;
            cache.insert((channel_id, channel_type), pts);
        }
        
        Ok(pts)
    }
    
    /// 更新频道的本地 pts
    pub async fn update_local_pts(&self, channel_id: u64, channel_type: u8, new_pts: u64) -> Result<()> {
        debug!("更新本地 pts: channel_id={}, channel_type={}, new_pts={}", 
               channel_id, channel_type, new_pts);
        
        // 更新数据库
        self.update_pts_in_db(channel_id, channel_type, new_pts).await?;
        
        // 更新缓存
        {
            let mut cache = self.cache.write().await;
            cache.insert((channel_id, channel_type), new_pts);
        }
        
        Ok(())
    }
    
    /// 检测是否有间隙
    pub async fn has_gap(&self, channel_id: u64, channel_type: u8, server_pts: u64) -> Result<bool> {
        let local_pts = self.get_local_pts(channel_id, channel_type).await?;
        
        if server_pts > local_pts + 1 {
            warn!("检测到 pts 间隙: channel_id={}, channel_type={}, local_pts={}, server_pts={}", 
                  channel_id, channel_type, local_pts, server_pts);
            return Ok(true);
        }
        
        Ok(false)
    }
    
    /// 获取间隙大小
    pub async fn get_gap_size(&self, channel_id: u64, channel_type: u8, server_pts: u64) -> Result<u64> {
        let local_pts = self.get_local_pts(channel_id, channel_type).await?;
        
        if server_pts > local_pts {
            Ok(server_pts - local_pts)
        } else {
            Ok(0)
        }
    }
    
    /// 批量获取多个频道的本地 pts
    pub async fn batch_get_local_pts(&self, channels: &[(u64, u8)]) -> Result<HashMap<(u64, u8), u64>> {
        let mut result = HashMap::new();
        
        for &(channel_id, channel_type) in channels {
            let pts = self.get_local_pts(channel_id, channel_type).await?;
            result.insert((channel_id, channel_type), pts);
        }
        
        Ok(result)
    }
    
    /// 清理缓存（用于测试或重置）
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }
    
    // ============================================================
    // 私有方法 - 数据库操作
    // ============================================================
    
    /// 从数据库获取 pts
    async fn get_pts_from_db(&self, channel_id: u64, channel_type: u8) -> Result<u64> {
        // 从 channel 表获取 last_msg_pts
        // 如果不存在，返回 0
        match self.storage.get_channel_by_channel(channel_id, channel_type).await {
            Ok(Some(channel)) => Ok(channel.last_msg_pts as u64),
            Ok(None) => {
                debug!("频道不存在，返回 pts=0: channel_id={}, channel_type={}", 
                       channel_id, channel_type);
                Ok(0)
            }
            Err(e) => {
                warn!("获取频道 pts 失败: channel_id={}, channel_type={}, error={:?}", 
                      channel_id, channel_type, e);
                // 如果查询失败，返回 0（保守策略）
                Ok(0)
            }
        }
    }
    
    /// 更新数据库中的 pts
    async fn update_pts_in_db(&self, channel_id: u64, channel_type: u8, new_pts: u64) -> Result<()> {
        // 更新 channel 表的 last_msg_pts
        // 注意：这里只更新 pts，不更新其他字段
        self.storage.update_channel_pts(channel_id, channel_type, new_pts).await?;
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    
    // 测试需要完整的 StorageManager 初始化，暂时跳过
    // 实际测试应该在集成测试中进行
    
    #[tokio::test]
    #[ignore]
    async fn test_pts_manager() {
        // TODO: 添加集成测试
    }
}
