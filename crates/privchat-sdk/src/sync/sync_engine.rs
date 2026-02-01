/// 同步引擎
/// 
/// 职责：
/// - 执行 getDifference 同步
/// - 管理同步状态
/// - 批量拉取和应用 Commits

use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error, info, warn};

use crate::error::{PrivchatSDKError, Result};
use crate::sync::pts_manager::PtsManager;
use crate::sync::commit_applier::CommitApplier;
use crate::sync::{SyncState, ChannelSyncState};
use privchat_protocol::rpc::sync::{
    GetDifferenceRequest, GetDifferenceResponse, GetChannelPtsRequest,
};

/// 同步引擎
pub struct SyncEngine {
    /// RPC 客户端  
    client: Arc<RwLock<Option<crate::client::PrivchatClient>>>,
    
    /// pts 管理器
    pts_manager: Arc<PtsManager>,
    
    /// Commit 应用器
    commit_applier: Arc<CommitApplier>,
    
    /// 同步锁（每个频道一个锁，防止并发同步）
    sync_locks: Arc<Mutex<std::collections::HashMap<(u64, u8), Arc<Mutex<()>>>>>,
}

impl SyncEngine {
    /// 创建同步引擎
    pub fn new(
        client: Arc<RwLock<Option<crate::client::PrivchatClient>>>,
        pts_manager: Arc<PtsManager>,
        commit_applier: Arc<CommitApplier>,
    ) -> Self {
        Self {
            client,
            pts_manager,
            commit_applier,
            sync_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
    
    /// 同步单个频道
    /// 
    /// 返回同步状态
    pub async fn sync_channel(&self, channel_id: u64, channel_type: u8) -> Result<ChannelSyncState> {
        // 获取同步锁
        let lock = self.get_sync_lock(channel_id, channel_type).await;
        let _guard = lock.lock().await;
        
        info!("开始同步频道: channel_id={}, channel_type={}", channel_id, channel_type);
        
        // 1. 获取本地 pts
        let local_pts = self.pts_manager.get_local_pts(channel_id, channel_type).await?;
        
        // 2. 获取服务器 pts
        let server_pts = self.get_server_pts(channel_id, channel_type).await?;
        
        // 3. 检查是否需要同步
        if local_pts >= server_pts {
            debug!("无需同步: channel_id={}, local_pts={}, server_pts={}", 
                   channel_id, local_pts, server_pts);
            return Ok(ChannelSyncState {
                channel_id,
                channel_type,
                local_pts,
                server_pts,
                state: SyncState::Synced,
                last_sync_at: chrono::Utc::now().timestamp_millis(),
            });
        }
        
        // 4. 执行补齐同步
        let gap_size = server_pts - local_pts;
        info!("检测到间隙，开始补齐: channel_id={}, gap_size={}", channel_id, gap_size);
        
        match self.fetch_and_apply_difference(channel_id, channel_type, local_pts, server_pts).await {
            Ok(()) => {
                info!("同步完成: channel_id={}, new_pts={}", channel_id, server_pts);
                Ok(ChannelSyncState {
                    channel_id,
                    channel_type,
                    local_pts: server_pts,
                    server_pts,
                    state: SyncState::Synced,
                    last_sync_at: chrono::Utc::now().timestamp_millis(),
                })
            }
            Err(e) => {
                error!("同步失败: channel_id={}, error={:?}", channel_id, e);
                Ok(ChannelSyncState {
                    channel_id,
                    channel_type,
                    local_pts,
                    server_pts,
                    state: SyncState::Failed {
                        error: e.to_string(),
                    },
                    last_sync_at: chrono::Utc::now().timestamp_millis(),
                })
            }
        }
    }
    
    /// 批量同步多个频道
    /// 
    /// 优化：使用 batch_get_channel_pts 一次性获取所有频道的 pts
    pub async fn batch_sync_channels(&self, channels: &[(u64, u8)]) -> Result<Vec<ChannelSyncState>> {
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        
        // 1. 批量获取所有频道的服务器 pts（优化：一次 RPC 调用）
        let channel_identifiers: Vec<privchat_protocol::rpc::sync::ChannelIdentifier> = channels
            .iter()
            .map(|&(channel_id, channel_type)| privchat_protocol::rpc::sync::ChannelIdentifier {
                channel_id,
                channel_type,
            })
            .collect();
        
        let batch_request = privchat_protocol::rpc::sync::BatchGetChannelPtsRequest {
            channels: channel_identifiers,
        };
        
        let request_value = serde_json::to_value(&batch_request)
            .map_err(|e| crate::error::PrivchatSDKError::JsonError(e.to_string()))?;
        
        let mut client_guard = self.client.write().await;
        let client = client_guard.as_mut()
            .ok_or(crate::error::PrivchatSDKError::NotConnected)?;
        
        let response_value = client
            .call_rpc("sync/batch_get_channel_pts", request_value)
            .await?;
        
        drop(client_guard);
        
        let batch_response: privchat_protocol::rpc::sync::BatchGetChannelPtsResponse = 
            serde_json::from_value(response_value)
                .map_err(|e| crate::error::PrivchatSDKError::JsonError(e.to_string()))?;
        
        // 2. 构建 channel_id -> server_pts 映射
        let mut server_pts_map = std::collections::HashMap::new();
        for pts_info in &batch_response.channel_pts_map {
            server_pts_map.insert((pts_info.channel_id, pts_info.channel_type), pts_info.current_pts);
        }
        
        // 3. 并行同步每个频道（使用已获取的 server_pts）
        let mut results = Vec::new();
        
        for &(channel_id, channel_type) in channels {
            // 获取服务器 pts（从批量查询结果中）
            let server_pts = server_pts_map.get(&(channel_id, channel_type))
                .copied()
                .unwrap_or(0);
            
            // 获取本地 pts
            let local_pts = match self.pts_manager.get_local_pts(channel_id, channel_type).await {
                Ok(pts) => pts,
                Err(e) => {
                    warn!("获取本地 pts 失败: channel_id={}, error={:?}", channel_id, e);
                    results.push(ChannelSyncState {
                        channel_id,
                        channel_type,
                        local_pts: 0,
                        server_pts,
                        state: SyncState::Failed {
                            error: e.to_string(),
                        },
                        last_sync_at: chrono::Utc::now().timestamp_millis(),
                    });
                    continue;
                }
            };
            
            // 检查是否需要同步
            if local_pts >= server_pts {
                results.push(ChannelSyncState {
                    channel_id,
                    channel_type,
                    local_pts,
                    server_pts,
                    state: SyncState::Synced,
                    last_sync_at: chrono::Utc::now().timestamp_millis(),
                });
                continue;
            }
            
            // 执行补齐同步
            match self.fetch_and_apply_difference(channel_id, channel_type, local_pts, server_pts).await {
                Ok(()) => {
                    results.push(ChannelSyncState {
                        channel_id,
                        channel_type,
                        local_pts: server_pts,
                        server_pts,
                        state: SyncState::Synced,
                        last_sync_at: chrono::Utc::now().timestamp_millis(),
                    });
                }
                Err(e) => {
                    warn!("同步频道失败: channel_id={}, channel_type={}, error={:?}", 
                          channel_id, channel_type, e);
                    results.push(ChannelSyncState {
                        channel_id,
                        channel_type,
                        local_pts,
                        server_pts,
                        state: SyncState::Failed {
                            error: e.to_string(),
                        },
                        last_sync_at: chrono::Utc::now().timestamp_millis(),
                    });
                }
            }
        }
        
        Ok(results)
    }
    
    // ============================================================
    // 私有方法
    // ============================================================
    
    /// 获取同步锁
    async fn get_sync_lock(&self, channel_id: u64, channel_type: u8) -> Arc<Mutex<()>> {
        let mut locks = self.sync_locks.lock().await;
        locks.entry((channel_id, channel_type))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    
    /// 获取服务器当前 pts
    async fn get_server_pts(&self, channel_id: u64, channel_type: u8) -> Result<u64> {
        let request = GetChannelPtsRequest {
            channel_id,
            channel_type,
        };
        
        // 获取 client 并调用 RPC
        let mut client_guard = self.client.write().await;
        let client = client_guard.as_mut()
            .ok_or(PrivchatSDKError::NotConnected)?;
        
        let request_value = serde_json::to_value(&request)
            .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
        
        let response_value = client
            .call_rpc("sync/get_channel_pts", request_value)
            .await?;
        
        drop(client_guard); // 释放锁
        
        let response: privchat_protocol::rpc::sync::GetChannelPtsResponse = serde_json::from_value(response_value)
            .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
        
        // GetChannelPtsResponse 现在只包含 current_pts: u64
        // 成功/失败由协议层的 code 字段处理，这里直接使用 current_pts
        Ok(response.current_pts)
    }
    
    /// 拉取并应用差异
    async fn fetch_and_apply_difference(
        &self,
        channel_id: u64,
        channel_type: u8,
        mut current_pts: u64,
        target_pts: u64,
    ) -> Result<()> {
        const BATCH_SIZE: u32 = 100; // 每批拉取 100 条
        
        while current_pts < target_pts {
            debug!("拉取差异: channel_id={}, current_pts={}, target_pts={}", 
                   channel_id, current_pts, target_pts);
            
            // 构造请求
            let request = GetDifferenceRequest {
                channel_id,
                channel_type,
                last_pts: current_pts,
                limit: Some(BATCH_SIZE),
            };
            
            // 发送请求
            let mut client_guard = self.client.write().await;
            let client = client_guard.as_mut()
                .ok_or(PrivchatSDKError::NotConnected)?;
            
            let request_value = serde_json::to_value(&request)
                .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
            
            let response_value = client
                .call_rpc("sync/get_difference", request_value)
                .await?;
            
            drop(client_guard); // 释放锁
            
            let response: GetDifferenceResponse = serde_json::from_value(response_value)
                .map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
            
            // GetDifferenceResponse 现在只包含 commits: Vec<ServerCommit>, current_pts, has_more
            // 成功/失败由协议层的 code 字段处理，这里直接使用数据
            let commits = response.commits;
            
            if commits.is_empty() {
                warn!("服务器返回空 commits，但 has_more={}", response.has_more);
                break;
            }
            
            // 应用 commits
            debug!("应用 {} 条 commits", commits.len());
            self.commit_applier.apply_commits(&commits).await?;
            
            // 更新本地 pts
            if let Some(last_commit) = commits.last() {
                current_pts = last_commit.pts;
                self.pts_manager.update_local_pts(channel_id, channel_type, current_pts).await?;
                debug!("更新本地 pts: channel_id={}, new_pts={}", channel_id, current_pts);
            }
            
            // 检查是否还有更多
            if !response.has_more {
                debug!("所有 commits 已拉取完毕");
                break;
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    #[ignore]
    async fn test_sync_engine() {
        // TODO: 添加集成测试
    }
}
