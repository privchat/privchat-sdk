/// pts-Based 同步模块
/// 
/// 职责：
/// - 管理每个频道的本地 pts
/// - 检测间隙（gap detection）
/// - 执行补齐同步（getDifference）
/// - 应用服务器 Commits
/// - 实体状态同步（ENTITY_SYNC_V1，与 PTS 正交）

pub mod pts_manager;
pub mod sync_engine;
pub mod commit_applier;
pub mod entity_sync;
pub mod bootstrap;

pub use pts_manager::PtsManager;
pub use sync_engine::SyncEngine;
pub use commit_applier::CommitApplier;
pub use entity_sync::{EntityType, EntitySyncEngine, SyncCursorStore};
pub use bootstrap::{run_bootstrap_sync, BOOTSTRAP_COMPLETED_KEY, BOOTSTRAP_ENTITY_TYPES};

/// 同步状态
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SyncState {
    /// 已同步
    Synced,
    /// 正在同步
    Syncing,
    /// 有间隙
    HasGap {
        /// 本地 pts
        local_pts: u64,
        /// 服务器 pts
        server_pts: u64,
    },
    /// 同步失败
    Failed {
        /// 错误消息
        error: String,
    },
}

/// 频道同步状态
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChannelSyncState {
    pub channel_id: u64,
    pub channel_type: u8,
    pub local_pts: u64,
    pub server_pts: u64,
    pub state: SyncState,
    pub last_sync_at: i64,
}
