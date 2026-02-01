//! 实体同步引擎 - ENTITY_SYNC_V1 统一入口
//!
//! 统一走 entity/sync_entities 协议 + cursor + applier，不再回退旧接口（contact/friend/list、group/group/list）。
//!
//! ## NOTE: Engine 不做重试
//!
//! EntitySyncEngine does not retry. All retry / backoff / lifecycle policies
//! MUST be implemented in SyncScheduler (or equivalent orchestration layer).
//! Without a Scheduler, the sync system will drift into unrecoverable states in production.

use crate::client::PrivchatClient;
use crate::error::{PrivchatSDKError, Result};
use crate::storage::StorageManager;
use privchat_protocol::rpc::{routes, sync::{SyncEntitiesRequest, SyncEntitiesResponse}};
use tracing::{debug, info};
use super::applier;
use super::cursor_store::SyncCursorStore;
use super::EntityType;

const FRIEND_PAGE_LIMIT: u32 = 100;

/// 实体同步引擎
pub struct EntitySyncEngine {
    cursor_store: SyncCursorStore,
}

impl EntitySyncEngine {
    pub fn new(cursor_store: SyncCursorStore) -> Self {
        Self { cursor_store }
    }

    /// 统一入口：按 entity_type/scope 执行全量或增量同步
    /// 全部走 entity/sync_entities，不再回退旧接口。
    pub async fn run_entity_sync(
        &self,
        client: &mut PrivchatClient,
        storage: &StorageManager,
        entity_type: EntityType,
        scope: Option<&str>,
        force_full_sync: bool,
    ) -> Result<usize> {
        match entity_type {
            EntityType::Friend if scope.is_none() => {
                self.run_friend_sync_via_sync_entities(client, storage, force_full_sync).await
            }
            EntityType::Group if scope.is_none() => {
                self.run_group_sync_via_sync_entities(client, storage, force_full_sync).await
            }
            _ => {
                self.run_sync_via_sync_entities_for_type(client, storage, entity_type, scope, force_full_sync)
                    .await
            }
        }
    }

    /// 通用 entity/sync_entities 分页拉取 + apply + cursor（用于 Channel / GroupMember / User / UserSettings / UserBlock）
    async fn run_sync_via_sync_entities_for_type(
        &self,
        client: &mut PrivchatClient,
        storage: &StorageManager,
        entity_type: EntityType,
        scope: Option<&str>,
        _force_full_sync: bool,
    ) -> Result<usize> {
        let limit = 100;
        let since_version = self
            .cursor_store
            .get(entity_type, scope)
            .await?
            .unwrap_or(0);
        let update_cursor = !entity_type.is_single_entity_refresh_when_scoped(scope);
        let mut pagination_scope: Option<String> = scope.map(String::from);
        let mut total = 0usize;

        loop {
            let response = self
                .fetch_sync_entities_page(
                    client,
                    entity_type,
                    since_version,
                    pagination_scope.as_deref(),
                    limit,
                )
                .await?;
            let count = self
                .apply_sync_entities_page(storage, entity_type, scope, response.clone(), update_cursor)
                .await?;
            total += count;
            debug!(
                "entity_sync {} 已同步本页 {} 条，累计 {}",
                entity_type.as_str(),
                count,
                total
            );
            if !response.has_more {
                break;
            }
            pagination_scope = response.items.last().map(|i| format!("cursor:{}", i.entity_id));
        }

        info!(
            "entity_sync {} 完成（sync_entities）: {} 条",
            entity_type.as_str(),
            total
        );
        Ok(total)
    }

    /// 好友同步：entity/sync_entities 分页拉取 + apply + 写 cursor
    async fn run_friend_sync_via_sync_entities(
        &self,
        client: &mut PrivchatClient,
        storage: &StorageManager,
        _force_full_sync: bool,
    ) -> Result<usize> {
        let limit = FRIEND_PAGE_LIMIT;
        let since_version = self
            .cursor_store
            .get(EntityType::Friend, None)
            .await?
            .unwrap_or(0);
        let mut scope: Option<String> = None;
        let mut total = 0usize;

        loop {
            let response = self
                .fetch_sync_entities_page(client, EntityType::Friend, since_version, scope.as_deref(), limit)
                .await?;
            let count = self
                .apply_sync_entities_page(storage, EntityType::Friend, None, response.clone(), true)
                .await?;
            total += count;
            debug!("entity_sync friend 已同步本页 {} 条，累计 {}", count, total);
            if !response.has_more {
                break;
            }
            scope = response.items.last().map(|i| format!("cursor:{}", i.entity_id));
        }

        info!("entity_sync friend 完成（sync_entities）: {} 条", total);
        Ok(total)
    }

    /// 群组同步：entity/sync_entities 分页拉取 + apply + 写 cursor
    async fn run_group_sync_via_sync_entities(
        &self,
        client: &mut PrivchatClient,
        storage: &StorageManager,
        _force_full_sync: bool,
    ) -> Result<usize> {
        let limit = 100;
        let since_version = self
            .cursor_store
            .get(EntityType::Group, None)
            .await?
            .unwrap_or(0);
        let mut scope: Option<String> = None;
        let mut total = 0usize;

        loop {
            let response = self
                .fetch_sync_entities_page(client, EntityType::Group, since_version, scope.as_deref(), limit)
                .await?;
            let count = self
                .apply_sync_entities_page(storage, EntityType::Group, None, response.clone(), true)
                .await?;
            total += count;
            debug!("entity_sync group 已同步本页 {} 条，累计 {}", count, total);
            if !response.has_more {
                break;
            }
            scope = response.items.last().map(|i| format!("cursor:{}", i.entity_id));
        }

        info!("entity_sync group 完成（sync_entities）: {} 条", total);
        Ok(total)
    }

    /// 拉一页 sync_entities 并应用，返回本页数量；由调用方循环分页与写 cursor
    async fn fetch_sync_entities_page(
        &self,
        client: &mut PrivchatClient,
        entity_type: EntityType,
        since_version: u64,
        scope: Option<&str>,
        limit: u32,
    ) -> Result<SyncEntitiesResponse> {
        let request = SyncEntitiesRequest {
            entity_type: entity_type.as_str().to_string(),
            since_version: if since_version == 0 {
                None
            } else {
                Some(since_version)
            },
            scope: scope.map(String::from),
            limit: Some(limit),
        };
        let response: SyncEntitiesResponse = client
            .call_rpc_typed(routes::entity::SYNC_ENTITIES, request)
            .await
            .map_err(|e| PrivchatSDKError::Other(format!("sync_entities RPC 失败: {}", e)))?;
        Ok(response)
    }

    /// 应用一页 sync_entities 响应并可选更新 cursor（单实体刷新不写）
    pub async fn apply_sync_entities_page(
        &self,
        storage: &StorageManager,
        entity_type: EntityType,
        scope: Option<&str>,
        response: SyncEntitiesResponse,
        update_cursor: bool,
    ) -> Result<usize> {
        for item in &response.items {
            applier::apply(storage, entity_type, scope, item).await?;
        }
        if update_cursor {
            self.cursor_store
                .set(entity_type, scope, response.next_version)
                .await?;
        }
        Ok(response.items.len())
    }

    pub fn cursor_store(&self) -> &SyncCursorStore {
        &self.cursor_store
    }
}
