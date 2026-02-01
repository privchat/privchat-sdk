//! 启动同步策略层（Bootstrap Sync）
//!
//! **职责边界**（与 EntitySyncEngine 分离）：
//! - **EntitySyncEngine**：只做「给定 entity_type + scope，读 cursor → RPC → 写库 → 更新 cursor」，无策略、无重试。
//! - **本模块**：决定「连接/恢复后 sync 哪些类型、以什么顺序执行」，属于**生命周期/编排层**。
//!
//! **全量/增量**：由 CursorStore 自然决定（无 cursor → since_version=0 全量；有 cursor → 增量），本层不传 force_full。
//!
//! **失败重试**：本层不做重试（遇错即返）。所有 retry / backoff / lifecycle 策略**必须**由 SyncScheduler 实现；Scheduler 是生产环境必需组件，v1 有意不实现，架构上已锁定其位置。

use crate::error::Result;
use crate::PrivchatSDK;
use super::EntityType;
use tracing::info;

/// KV 中标记「Bootstrap 已完整执行过一次」的 key（按用户维度，用于首次登录强制全量）
pub const BOOTSTRAP_COMPLETED_KEY: &str = "entity_sync:bootstrap_completed";

/// 冷启动 / connect 成功后应同步的实体类型（有序）
///
/// 顺序与 ENTITY_SYNC_V1 设计一致：friends → groups → channels，保证 channel 依赖的 group 已落库；user_settings 最后。
pub const BOOTSTRAP_ENTITY_TYPES: &[EntityType] = &[
    EntityType::Friend,
    EntityType::Group,
    EntityType::Channel,
    EntityType::UserSettings,
];

/// 执行一次完整的启动同步（串行、按 BOOTSTRAP_ENTITY_TYPES 顺序）
///
/// 由**生命周期层**在 connect 成功 / resume / foreground 等节点调用，不应由 Engine 内部调用。
/// 全量/增量由 CursorStore 决定；本函数只负责「按顺序执行各类型一次」。
///
/// 策略：遇错即返，不在此层做重试（重试由外层 SyncScheduler 负责，若实现）。
///
/// 顺序：1) 检测并初始化 db/kv/queue（及发送消费者）→ 2) Friend → Group → Channel → UserSettings → 3) sync_all_channels（频道消息同步）
pub async fn run_bootstrap_sync(sdk: &PrivchatSDK) -> Result<()> {
    // 先检测并初始化当前用户的 db、kv、queue，再启动发送消费者；认证后必须运行本方法，故存储在此处统一初始化
    sdk.ensure_user_storage_initialized().await?;

    for &entity_type in BOOTSTRAP_ENTITY_TYPES {
        info!("🔄 bootstrap sync: {}", entity_type.as_str());
        sdk.sync_entities(entity_type, None).await?;
    }
    // 实体同步完成后，同步各频道的消息（sync/batch_get_channel_pts 需已认证 session，故放在 bootstrap 内）
    sdk.sync_all_channels().await?;
    info!("✅ bootstrap sync 完成");
    Ok(())
}
