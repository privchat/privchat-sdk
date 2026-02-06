//! 存储模块 - 现代化 IM SDK 的数据持久化层
//!
//! 采用分层架构设计：
//! - StorageManager: 统一的存储管理器，提供高级 API
//! - DAO Layer: 数据访问层，每张表一个专门的操作模块
//! - Entities: 数据实体定义，类型安全的数据传输
//! - 支持多用户、事务管理、数据迁移

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing;

use crate::error::{PrivchatSDKError, Result};

pub mod advanced_features;
pub mod advanced_features_integration;
pub mod dao;
pub mod db_actor;
pub mod deduplication;
pub mod entities;
pub mod kv;
pub mod media;
pub mod media_preprocess;
pub mod message_state;
pub mod migrate;
pub mod migration;
pub mod queue;
pub mod reaction;
pub mod sqlite;
pub mod typing;

// 重新导出核心类型
pub use advanced_features::{
    AdvancedFeaturesManager, ChannelReadState, MessageEditEvent, MessageRevokeEvent,
    ReadReceiptEvent,
};
pub use advanced_features_integration::AdvancedFeaturesIntegration;
pub use dao::migration::MigrationDao;
pub use dao::{DaoFactory, TransactionManager};
pub use entities::*;

/// SDK 版本号 - 来自 Cargo.toml（参见 crate::version）
pub use crate::version::SDK_VERSION;

/// Assets 文件缓存信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetsCache {
    /// SDK 版本号
    pub sdk_version: String,
    /// Assets 目录路径
    pub assets_path: String,
    /// 文件时间戳列表 (文件名 -> 时间戳)
    pub file_timestamps: std::collections::BTreeMap<String, u64>,
    /// 缓存创建时间
    pub cached_at: u64,
    /// 最后一次数据库版本号
    pub last_db_version: String,
}

/// 缓存键常量
pub mod cache_keys {
    /// Assets 文件缓存键
    pub const ASSETS_CACHE: &str = "assets_cache";
    /// 数据库版本缓存键
    pub const DB_VERSION_CACHE: &str = "db_version_cache";
    /// 最后迁移时间戳
    pub const LAST_MIGRATION_TIME: &str = "last_migration_time";
}

/// SQLite 存储统计信息
#[derive(Debug, Clone)]
pub struct SqliteStats {
    pub database_size: u64,
    pub message_count: u64,
    pub user_count: u64,
    pub channel_count: u64,
    pub table_count: u32,
    pub total_records: u64,
}

/// KV 存储统计信息
#[derive(Debug, Clone)]
pub struct KvStats {
    pub tree_size: u64,
    pub key_count: u64,
    pub total_keys: u64,
    pub storage_size: u64,
}

/// 队列统计信息
#[derive(Debug, Clone)]
pub struct QueueStats {
    pub pending_tasks: u64,
    pub completed_tasks: u64,
    pub failed_tasks: u64,
    pub pending_count: u64,
    pub processed_count: u64,
}

/// 媒体存储统计信息
#[derive(Debug, Clone)]
pub struct MediaStats {
    pub total_files: u64,
    pub total_size: u64,
    pub image_count: u64,
    pub video_count: u64,
    pub audio_count: u64,
    pub document_count: u64,
}

/// 现代化存储管理器 - 统一的数据访问接口
///
/// 功能特性：
/// - 完全控制所有数据库操作，外部无法直接访问 SQLite
/// - 提供领域 API，而非裸 SQL 操作
/// - 支持多用户数据隔离
/// - 自动数据库迁移和版本管理
/// - 事务安全和数据一致性保障
#[derive(Debug)]
pub struct StorageManager {
    base_path: PathBuf,
    /// 应用内 assets 目录路径，存放 SQL 迁移文件
    assets_path: Option<PathBuf>,
    /// 数据库 Actor 句柄（单线程数据库访问）
    db_actor: db_actor::DbActorHandle,
    /// 当前活跃用户
    current_user: Arc<RwLock<Option<String>>>,
    /// 每用户 KV 存储（路径为 users/{uid}/kv），不共享
    user_kv_stores: Arc<RwLock<HashMap<String, Arc<crate::storage::kv::KvStore>>>>,
    /// 每用户队列（路径为 users/{uid}/kv 内持久化），不共享
    user_queue_managers:
        Arc<RwLock<HashMap<String, Arc<dyn crate::storage::queue::TaskQueueTrait + Send + Sync>>>>,
    /// 媒体索引管理器
    media_manager: Option<Arc<crate::storage::media::MediaIndex>>,
}

impl StorageManager {
    /// 创建新的存储管理器
    ///
    /// # 参数
    /// - `base_path`: 用户数据存储的基础路径
    /// - `assets_path`: 可选的 assets 目录路径，存放 SQL 迁移文件
    pub async fn new(base_path: &Path, assets_path: Option<&Path>) -> Result<Self> {
        // 确保基础目录存在
        tokio::fs::create_dir_all(base_path)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建存储目录失败: {}", e)))?;

        // 如果提供了 assets 目录，确保它存在
        if let Some(assets_path) = assets_path {
            tokio::fs::create_dir_all(assets_path)
                .await
                .map_err(|e| PrivchatSDKError::IO(format!("创建 assets 目录失败: {}", e)))?;
        }

        // KV 与队列按用户创建，在 init_user(uid) 时创建 users/{uid}/kv 与队列，不在此处创建共享实例
        let db_actor = db_actor::DbActorHandle::spawn(assets_path.map(|p| p.to_path_buf()));
        tracing::info!(
            "✅ DB Actor 已启动（单线程模型，assets_path={:?}）",
            assets_path
        );

        Ok(Self {
            base_path: base_path.to_path_buf(),
            assets_path: assets_path.map(|p| p.to_path_buf()),
            db_actor,
            current_user: Arc::new(RwLock::new(None)),
            user_kv_stores: Arc::new(RwLock::new(HashMap::new())),
            user_queue_managers: Arc::new(RwLock::new(HashMap::new())),
            media_manager: None,
        })
    }

    /// 创建新的存储管理器（仅用户数据目录）
    pub async fn new_simple(base_path: &Path) -> Result<Self> {
        Self::new(base_path, None).await
    }

    /// 创建新的存储管理器（包含 assets 目录）
    pub async fn new_with_assets(base_path: &Path, assets_path: &Path) -> Result<Self> {
        Self::new(base_path, Some(assets_path)).await
    }

    /// 检查用户是否已初始化（Actor 模型下始终返回 false，依赖 init_user 的幂等性）
    pub async fn is_user_initialized(&self, _uid: &str) -> bool {
        // 在 Actor 模型下，init_user 是幂等的，可以安全地重复调用
        false
    }

    /// 初始化用户数据库（使用嵌入式 SQL）
    /// 用户目录为 {base_path}/users/{uid}/，其下含 messages.db、kv/、队列持久化等，每用户独立
    /// 幂等：若该用户已初始化则仅切换 current_user 并返回。
    pub async fn init_user(&self, uid: &str) -> Result<()> {
        {
            let stores = self.user_kv_stores.read().await;
            if stores.contains_key(uid) {
                drop(stores);
                self.switch_user(uid).await?;
                tracing::debug!("用户已初始化，仅切换 current_user: {}", uid);
                return Ok(());
            }
        }
        let user_dir = self.user_dir(uid);
        tokio::fs::create_dir_all(&user_dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;

        // 每用户独立 KV：users/{uid}/kv
        let kv_store = Arc::new(crate::storage::kv::KvStore::new(&user_dir).await?);
        kv_store.init_user_tree(uid).await?;
        kv_store.switch_user(uid).await?;
        {
            let mut stores = self.user_kv_stores.write().await;
            stores.insert(uid.to_string(), kv_store.clone());
        }

        // 每用户独立队列（持久化在该用户的 KvStore 内）
        let queue = Arc::new(crate::storage::queue::TaskQueue::Persistent(
            crate::storage::queue::PersistentTaskQueue::new(kv_store, uid.to_string()),
        )) as Arc<dyn crate::storage::queue::TaskQueueTrait + Send + Sync>;
        {
            let mut queues = self.user_queue_managers.write().await;
            queues.insert(uid.to_string(), queue);
        }

        let db_path = user_dir.join("messages.db");
        tracing::info!(
            "🔧 正在初始化用户数据库: uid={}, path={}",
            uid,
            db_path.display()
        );

        self.db_actor.init_user(uid.to_string(), db_path).await?;

        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());

        tracing::info!("✅ 用户数据库初始化完成: {} (Actor模型)", uid);

        Ok(())
    }

    /// 初始化用户数据库（使用外部 assets 目录）
    pub async fn init_user_with_assets(&self, uid: &str) -> Result<()> {
        let assets_path = self
            .assets_path
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("未设置 assets 目录路径".to_string()))?;

        self.init_user_with_smart_migration(uid, assets_path).await
    }

    /// 初始化用户数据库（使用自定义 assets 目录）
    pub async fn init_user_with_custom_assets(&self, uid: &str, assets_path: &Path) -> Result<()> {
        self.init_user_with_smart_migration(uid, assets_path).await
    }

    /// 智能迁移初始化 - 使用缓存优化
    async fn init_user_with_smart_migration(&self, uid: &str, assets_path: &Path) -> Result<()> {
        let user_dir = self.user_dir(uid);
        tokio::fs::create_dir_all(&user_dir)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;

        // 确保该用户的 KV 与队列已存在（check_need_migration 需要当前用户的 KvStore）
        let kv_store = Arc::new(crate::storage::kv::KvStore::new(&user_dir).await?);
        kv_store.init_user_tree(uid).await?;
        kv_store.switch_user(uid).await?;
        {
            let mut stores = self.user_kv_stores.write().await;
            stores.insert(uid.to_string(), kv_store.clone());
        }
        let queue = Arc::new(crate::storage::queue::TaskQueue::Persistent(
            crate::storage::queue::PersistentTaskQueue::new(kv_store, uid.to_string()),
        )) as Arc<dyn crate::storage::queue::TaskQueueTrait + Send + Sync>;
        {
            let mut queues = self.user_queue_managers.write().await;
            queues.insert(uid.to_string(), queue);
        }
        {
            let mut cur = self.current_user.write().await;
            *cur = Some(uid.to_string());
        }

        let db_path = user_dir.join("messages.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;

        let need_migration = self.check_need_migration(assets_path).await?;

        if need_migration {
            tracing::info!("检测到 assets 文件变化，执行数据库迁移");

            // 执行迁移
            let migration_dao = dao::MigrationDao::new(&conn);
            migration_dao.migrate_from_assets(assets_path)?;

            // 更新缓存
            self.update_assets_cache(assets_path).await?;
        } else {
            tracing::info!("assets 文件未变化，跳过迁移扫描");
        }

        // 验证数据库结构
        let migration_dao = dao::MigrationDao::new(&conn);
        if !migration_dao.validate_schema()? {
            return Err(PrivchatSDKError::Database("数据库结构验证失败".to_string()));
        }

        // Actor 模型下不需要手动管理连接
        tracing::info!("用户数据库初始化完成: {} (使用智能迁移 + Actor模型)", uid);

        Ok(())
    }

    /// 检查是否需要重新扫描 assets 目录执行迁移
    async fn check_need_migration(&self, assets_path: &Path) -> Result<bool> {
        let kv_store = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化（当前用户）".to_string()))?;

        let cached_assets: Option<AssetsCache> = kv_store.get(cache_keys::ASSETS_CACHE).await?;

        // 如果没有缓存，需要执行迁移
        let Some(cache) = cached_assets else {
            tracing::info!("首次运行，需要执行迁移");
            return Ok(true);
        };

        // 检查 SDK 版本是否变化
        if cache.sdk_version != SDK_VERSION {
            tracing::info!("SDK 版本变化: {} -> {}", cache.sdk_version, SDK_VERSION);
            return Ok(true);
        }

        // 检查 assets 目录路径是否变化
        if cache.assets_path != assets_path.to_string_lossy() {
            tracing::info!(
                "assets 目录路径变化: {} -> {}",
                cache.assets_path,
                assets_path.display()
            );
            return Ok(true);
        }

        // 检查 assets 目录中的文件是否有变化
        let current_timestamps = self.scan_assets_timestamps(assets_path).await?;

        // 比较文件时间戳
        if current_timestamps != cache.file_timestamps {
            tracing::info!("assets 文件时间戳变化，需要重新扫描");
            return Ok(true);
        }

        tracing::debug!("assets 文件未变化，跳过迁移");
        Ok(false)
    }

    /// 扫描 assets 目录获取文件时间戳
    async fn scan_assets_timestamps(
        &self,
        assets_path: &Path,
    ) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut file_timestamps = std::collections::BTreeMap::new();

        if !assets_path.exists() {
            return Ok(file_timestamps);
        }

        let mut entries = tokio::fs::read_dir(assets_path)
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("读取 assets 目录失败: {}", e)))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| PrivchatSDKError::IO(format!("遍历 assets 目录失败: {}", e)))?
        {
            let path = entry.path();
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if file_name.ends_with(".sql") {
                        let metadata = entry.metadata().await.map_err(|e| {
                            PrivchatSDKError::IO(format!("获取文件元数据失败: {}", e))
                        })?;

                        let timestamp = metadata
                            .modified()
                            .map_err(|e| {
                                PrivchatSDKError::IO(format!("获取文件修改时间失败: {}", e))
                            })?
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_err(|e| PrivchatSDKError::IO(format!("转换时间戳失败: {}", e)))?
                            .as_secs();

                        file_timestamps.insert(file_name.to_string(), timestamp);
                    }
                }
            }
        }

        Ok(file_timestamps)
    }

    /// 更新 assets 缓存
    async fn update_assets_cache(&self, assets_path: &Path) -> Result<()> {
        let kv_store = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化（当前用户）".to_string()))?;

        let file_timestamps = self.scan_assets_timestamps(assets_path).await?;

        let cache = AssetsCache {
            sdk_version: SDK_VERSION.to_string(),
            assets_path: assets_path.to_string_lossy().to_string(),
            file_timestamps,
            cached_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            last_db_version: "".to_string(), // 可以后续添加数据库版本信息
        };

        kv_store.set(cache_keys::ASSETS_CACHE, &cache).await?;

        tracing::info!("更新 assets 缓存完成");
        Ok(())
    }

    /// 获取 assets 目录路径
    pub fn assets_path(&self) -> Option<&Path> {
        self.assets_path.as_deref()
    }

    /// 获取用户数据目录。
    /// 路径为 `{data_dir}/users/{uid}/`，其中 `data_dir` 为 SDK 初始化时指定的 `data_dir`，
    /// 每个用户拥有独立的 `messages.db` 及媒体等，不同用户不会共用同一数据库。
    pub fn user_dir(&self, uid: &str) -> PathBuf {
        self.base_path.join("users").join(uid)
    }

    /// 获取基础数据目录
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// 清理用户的 assets 缓存
    pub async fn clear_assets_cache(&self) -> Result<()> {
        let kv_store = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化（当前用户）".to_string()))?;

        kv_store.delete(cache_keys::ASSETS_CACHE).await?;

        tracing::info!("assets 缓存已清理");
        Ok(())
    }

    /// 强制刷新 assets 缓存
    pub async fn refresh_assets_cache(&self, assets_path: &Path) -> Result<()> {
        self.clear_assets_cache().await?;
        self.update_assets_cache(assets_path).await?;

        tracing::info!("assets 缓存已刷新");
        Ok(())
    }

    /// 获取当前的 assets 缓存信息
    pub async fn get_assets_cache_info(&self) -> Result<Option<AssetsCache>> {
        let kv_store = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化（当前用户）".to_string()))?;

        let cache: Option<AssetsCache> = kv_store.get(cache_keys::ASSETS_CACHE).await?;
        Ok(cache)
    }

    /// 获取当前用户的 KV 存储（每用户独立，路径为 users/{uid}/kv）
    pub async fn kv_store(&self) -> Option<Arc<crate::storage::kv::KvStore>> {
        let uid = self.current_user.read().await.clone()?;
        self.user_kv_stores.read().await.get(&uid).cloned()
    }

    /// 获取单条用户设置（Entity Sync 落库后的唯一读入口，符合「只从 DB 读」）
    /// key 即 entity_type=user_settings 时的 entity_id（setting_key），如 "theme", "notification_enabled"
    pub async fn get_user_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let kv = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("KV 未初始化（当前用户）".to_string()))?;
        let storage_key = format!("entity_sync:user_settings:{}", key);
        kv.get(storage_key.as_str()).await
    }

    /// 获取当前用户全部设置（用于设置页展示，只读 DB）
    pub async fn get_all_user_settings(
        &self,
    ) -> Result<std::collections::HashMap<String, serde_json::Value>> {
        let kv = self
            .kv_store()
            .await
            .ok_or_else(|| PrivchatSDKError::Other("KV 未初始化（当前用户）".to_string()))?;
        const PREFIX: &str = "entity_sync:user_settings:";
        let pairs = kv
            .scan_prefix::<serde_json::Value>(PREFIX.as_bytes())
            .await?;
        let mut out = HashMap::new();
        for (k, v) in pairs {
            if let Ok(s) = std::str::from_utf8(&k) {
                if let Some(suffix) = s.strip_prefix(PREFIX) {
                    out.insert(suffix.to_string(), v);
                }
            }
        }
        Ok(out)
    }

    /// 获取媒体索引管理器实例的引用
    pub fn media_index(&self) -> Option<&Arc<crate::storage::media::MediaIndex>> {
        self.media_manager.as_ref()
    }

    /// 切换当前用户（KV/队列按用户独立，仅切换 current_user 指针）
    pub async fn switch_user(&self, uid: &str) -> Result<()> {
        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());
        tracing::info!("用户切换完成: {} (Actor模型)", uid);
        Ok(())
    }

    /// 更新消息状态（按 message.id）
    pub async fn update_message_status(&self, id: i64, status: i32) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        tracing::debug!(
            "🔍 [StorageManager] update_message_status: uid={}, id={}, status={}",
            uid,
            id,
            status
        );
        self.db_actor
            .update_message_status(uid.clone(), id, status)
            .await
            .map_err(|e| {
                tracing::error!(
                    "❌ [StorageManager] update_message_status 失败: uid={}, error={}",
                    uid,
                    e
                );
                e
            })
    }

    /// 更新消息的服务端 message_id（按 message.id，仅协议层写入）
    pub async fn update_message_server_id(&self, id: i64, server_message_id: u64) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor
            .update_message_server_id(uid, id, server_message_id)
            .await
    }

    // ===== 消息相关的高级 API =====

    /// 发送消息 - 返回 message.id
    pub async fn send_message(
        &self,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: &str,
        message_type: i32,
    ) -> Result<i64> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor
            .send_message(
                uid,
                channel_id,
                channel_type,
                from_uid,
                content.to_string(),
                message_type,
            )
            .await
    }

    /// 获取当前登录用户 ID（用于收到消息后下载缩略图等）
    pub async fn get_current_user_id(&self) -> Option<String> {
        self.current_user.read().await.clone()
    }

    /// 保存消息（接收或发送）
    ///
    /// # 参数
    /// - `message`: 消息实体
    /// - `is_outgoing`: true=自己发送的消息（不增加 unread_count），false=接收的消息
    pub async fn save_received_message(&self, message: &Message, is_outgoing: bool) -> Result<i64> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        // 通过 DB Actor 保存（单线程安全）
        let row_id = self
            .db_actor
            .save_received_message(uid, message.clone(), is_outgoing)
            .await?;

        Ok(row_id)
    }

    /// 保存消息（通用方法，用于同步等场景，默认视为接收的消息）
    /// 返回插入的 message.id（row_id），便于调用方触发缩略图下载等
    pub async fn save_message(&self, message: &Message) -> Result<i64> {
        let row_id = self.save_received_message(message, false).await?;
        Ok(row_id)
    }

    /// 撤回消息（按 message.id）
    pub async fn revoke_message(&self, id: i64) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor.revoke_message(uid, id).await
    }

    /// 撤回消息（按 message.id，指定 revoker_id）
    pub async fn revoke_message_by(&self, id: i64, revoker_id: u64) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor
            .revoke_message_with_revoker(uid, id, revoker_id)
            .await
    }

    /// 删除消息（按 message.id）
    pub async fn delete_message(&self, id: i64) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor.delete_message(uid, id).await
    }

    /// 更新消息内容（按 message.id）
    pub async fn update_message_content(&self, id: i64, new_content: &str) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor
            .update_message_content(uid, id, new_content.to_string())
            .await
    }

    /// 添加消息反应（按 message.id）
    pub async fn add_message_reaction(&self, id: i64, user_id: u64, reaction: &str) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor
            .add_message_reaction(uid, id, user_id, reaction.to_string())
            .await
    }

    /// 编辑消息（按 message.id）
    pub async fn edit_message(&self, id: i64, new_content: &str) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor
            .update_message_content(uid, id, new_content.to_string())
            .await
    }

    /// 根据 message.id 获取消息
    pub async fn get_message_by_id(&self, id: i64) -> Result<Option<Message>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("未登录".to_string()))?;
        self.db_actor.get_message_by_id(uid, id).await
    }

    /// 获取消息的 channel_id（按 message.id）
    pub async fn get_message_channel_id(&self, id: i64) -> Result<Option<u64>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("未登录".to_string()))?;
        self.db_actor.get_message_channel_id(uid, id).await
    }

    /// 查询消息（请通过 db_actor 或 SDK 高层 API 使用）
    pub async fn query_messages(&self, _query: &MessageQuery) -> Result<PageResult<Message>> {
        Err(PrivchatSDKError::Other(
            "query_messages 已移除直接连接，请使用 db_actor 或 get_messages_before/get_messages_after".to_string()
        ))
    }

    /// 搜索消息（请通过 db_actor 使用）
    pub async fn search_messages(
        &self,
        _channel_id: u64,
        _channel_type: i32,
        _keyword: &str,
        _limit: Option<u32>,
    ) -> Result<Vec<Message>> {
        Err(PrivchatSDKError::Other(
            "search_messages 已移除直接连接，请使用 db_actor".to_string(),
        ))
    }

    // ===== 会话相关的高级 API =====

    /// 获取会话列表
    pub async fn get_channels(&self, _query: &ChannelQuery) -> Result<Vec<Channel>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        // 通过 DB Actor 获取会话列表
        self.db_actor.get_channels(uid).await
    }

    /// 获取消息列表（请使用 get_messages_before / get_messages_after）
    pub async fn get_messages(&self, _query: &MessageQuery) -> Result<Vec<Message>> {
        Err(PrivchatSDKError::Other(
            "get_messages 已移除直接连接，请使用 get_messages_before 或 get_messages_after"
                .to_string(),
        ))
    }

    /// 获取指定 message.id 之前的消息（使用 Actor 模型，向后分页，游标为客户端 id）
    pub async fn get_messages_before(
        &self,
        channel_id: u64,
        before_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let uid = self
            .current_user
            .read()
            .await
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::Other("未设置当前用户".to_string()))?
            .clone();

        self.db_actor
            .get_messages_before(uid, channel_id, before_id, limit)
            .await
    }

    /// 获取频道当前最小的 message.id（用于「加载更早」分页游标）
    pub async fn get_earliest_id(&self, channel_id: u64) -> Result<Option<u64>> {
        let uid = self
            .current_user
            .read()
            .await
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::Other("未设置当前用户".to_string()))?
            .clone();

        self.db_actor.get_earliest_id(uid, channel_id).await
    }

    /// 获取指定 message.id 之后的消息（使用 Actor 模型，向前分页）
    pub async fn get_messages_after(
        &self,
        channel_id: u64,
        after_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let uid = self
            .current_user
            .read()
            .await
            .as_ref()
            .ok_or_else(|| PrivchatSDKError::Other("未设置当前用户".to_string()))?
            .clone();

        self.db_actor
            .get_messages_after(uid, channel_id, after_id, limit)
            .await
    }

    /// 标记频道为已读（请通过 db_actor 使用）
    pub async fn mark_channel_read(&self, _channel_id: u64, _channel_type: i32) -> Result<()> {
        Err(PrivchatSDKError::Other(
            "mark_channel_read 已移除直接连接，请使用 db_actor".to_string(),
        ))
    }

    /// 根据 channel_id 和 channel_type 获取会话
    pub async fn get_channel_by_channel(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<Option<Channel>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        // 通过 DB Actor 获取会话
        self.db_actor
            .get_channel_by_channel(uid, channel_id, channel_type)
            .await
    }

    /// 按 channel_id 查询私聊会话（channel_type 0 或 1 均视为同一私聊，用于避免重复插入导致列表两条）
    pub async fn get_direct_channel_by_id(&self, channel_id: u64) -> Result<Option<Channel>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        self.db_actor
            .get_direct_channel_by_id(uid, channel_id)
            .await
    }

    /// 更新会话的 pts（用于同步）
    pub async fn update_channel_pts(
        &self,
        channel_id: u64,
        channel_type: u8,
        new_pts: u64,
    ) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;

        // 通过 DB Actor 更新 pts
        self.db_actor
            .update_channel_pts(uid, channel_id, channel_type, new_pts)
            .await
    }

    /// 获取总未读消息数（所有会话未读数的和）
    ///
    /// 这是 Telegram、微信等主流 IM SDK 的标准设计：
    /// - 总未读数 = 所有会话未读数的和
    /// - 通常排除免打扰的会话
    ///
    /// # 参数
    /// - `exclude_muted`: 是否排除免打扰的会话（默认 true，与主流 SDK 一致）
    ///
    /// # 返回
    /// 总未读消息数
    pub async fn get_total_unread_count(&self, _exclude_muted: bool) -> Result<i32> {
        Err(PrivchatSDKError::Other(
            "get_total_unread_count 已移除直接连接，请使用 db_actor 或 AdvancedFeaturesIntegration"
                .to_string(),
        ))
    }

    /// 验证总未读数是否等于会话列表未读数的和（请通过 db_actor 使用）
    pub async fn verify_total_unread_count(
        &self,
        _exclude_muted: bool,
    ) -> Result<(i32, i32, bool)> {
        Err(PrivchatSDKError::Other(
            "verify_total_unread_count 已移除直接连接，请使用 db_actor 或 AdvancedFeaturesIntegration".to_string()
        ))
    }

    // ===== 频道相关的高级 API =====

    /// 获取频道信息
    pub async fn get_channel(&self, channel_id: u64, channel_type: i32) -> Result<Option<Channel>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        self.db_actor
            .get_channel_by_channel(uid, channel_id, channel_type as u8)
            .await
    }

    // ===== 事务管理 =====

    /// 执行事务操作（已移除直接连接，请使用 db_actor.execute / query）
    pub async fn execute_transaction<F, R>(&self, _f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R>,
    {
        Err(PrivchatSDKError::Other(
            "execute_transaction 已移除直接连接，请使用 db_actor.execute 或 db_actor.query"
                .to_string(),
        ))
    }

    // ===== 内部辅助方法 =====

    /// 更新会话信息（在发送消息后）
    #[allow(dead_code)]
    fn update_channel_after_message(&self, conn: &Connection, message: &Message) -> Result<()> {
        let channel_dao = dao::ChannelDao::new(conn);

        // 查找现有会话
        if let Some(mut channel) =
            channel_dao.get_by_channel(message.channel_id, message.channel_type)?
        {
            // 更新会话信息
            channel.last_local_message_id = message.local_message_id;
            channel.last_msg_timestamp = message.timestamp;
            channel.last_msg_content = message.content.clone();
            channel.last_msg_pts = message.pts;
            channel.version += 1;

            channel_dao.upsert(&channel)?;
        } else {
            // 创建新会话
            let now = chrono::Utc::now().timestamp_millis();
            let new_channel = Channel {
                id: None,
                channel_id: message.channel_id,
                channel_type: message.channel_type,
                // 会话列表相关字段（只使用 message.id）
                last_local_message_id: message.id.unwrap_or(0) as u64,
                last_msg_timestamp: message.timestamp,
                last_msg_content: message.content.clone(),
                unread_count: 0,
                last_msg_pts: message.pts,
                // 频道信息字段（使用默认值）
                show_nick: 0,
                username: String::new(),
                channel_name: String::new(),
                channel_remark: String::new(),
                top: 0,
                mute: 0,
                save: 0,
                forbidden: 0,
                follow: 0,
                is_deleted: 0,
                receipt: 0,
                status: 1,
                invite: 0,
                robot: 0,
                version: 1,
                online: 0,
                last_offline: 0,
                avatar: String::new(),
                category: String::new(),
                extra: "{}".to_string(),
                created_at: now,
                updated_at: now,
                avatar_cache_key: String::new(),
                remote_extra: None,
                flame: 0,
                flame_second: 0,
                device_flag: 0,
                parent_channel_id: 0,
                parent_channel_type: 0,
            };

            channel_dao.upsert(&new_channel)?;
        }

        Ok(())
    }

    /// 保存 Channel 到数据库
    pub async fn save_channel(&self, channel: &entities::Channel) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.save_channel(uid, channel.clone()).await
    }

    // ========== User（Entity Model V1）==========

    pub async fn save_user(&self, user: &entities::User) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.save_user(uid, user.clone()).await
    }

    pub async fn save_users(&self, users: Vec<entities::User>) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.save_users(uid, users).await
    }

    pub async fn get_user(&self, user_id: u64) -> Result<Option<entities::User>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.get_user(uid, user_id).await
    }

    pub async fn get_users_by_ids(&self, ids: Vec<u64>) -> Result<Vec<entities::User>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.get_users_by_ids(uid, ids).await
    }

    // ========== 好友管理方法（Local-first）==========

    /// 保存单个好友到数据库
    pub async fn save_friend(&self, friend: &entities::Friend) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.save_friend(uid, friend.clone()).await
    }

    /// 批量保存好友（提升性能）
    pub async fn save_friends(&self, friends: Vec<entities::Friend>) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.save_friends(uid, friends).await
    }

    /// 从本地数据库获取好友列表（分页，含 User 展示信息，Entity Model V1）
    /// 对外唯一入口：方法名保持 get_friends，内部做 friend + user 关联。
    pub async fn get_friends(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<(entities::Friend, entities::User)>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        let friends = self
            .db_actor
            .get_friends(uid.clone(), limit, offset)
            .await?;
        if friends.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<u64> = friends.iter().map(|f| f.user_id).collect();
        let users = self.db_actor.get_users_by_ids(uid, ids).await?;
        let user_map: std::collections::HashMap<u64, entities::User> =
            users.into_iter().map(|u| (u.user_id, u)).collect();
        let out: Vec<_> = friends
            .into_iter()
            .map(|f| {
                let u = user_map
                    .get(&f.user_id)
                    .cloned()
                    .unwrap_or_else(|| entities::User {
                        user_id: f.user_id,
                        username: None,
                        nickname: None,
                        alias: None,
                        avatar: String::new(),
                        user_type: 0,
                        is_deleted: false,
                        channel_id: String::new(),
                        updated_at: 0,
                    });
                (f, u)
            })
            .collect();
        Ok(out)
    }

    /// 获取好友总数
    pub async fn get_friends_count(&self) -> Result<u32> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.get_friends_count(uid).await
    }

    // ========== 频道成员管理方法（Local-first）==========

    /// 保存频道成员到数据库
    pub async fn save_channel_member(&self, member: &entities::ChannelMember) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.save_channel_member(uid, member.clone()).await
    }

    /// 批量保存频道成员（提升性能）
    pub async fn save_channel_members(&self, members: Vec<entities::ChannelMember>) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.save_channel_members(uid, members).await
    }

    /// 删除频道成员（用于 entity_sync group_member tombstone 等）
    pub async fn delete_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor
            .delete_channel_member(uid, channel_id, channel_type, member_uid)
            .await
    }

    /// 从本地数据库获取群成员列表（按 group_id 关联）
    pub async fn get_group_members(
        &self,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<entities::ChannelMember>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor
            .get_group_members(uid, group_id, limit, offset)
            .await
    }

    /// 从本地数据库获取群列表（分页）
    pub async fn get_groups(&self, limit: u32, offset: u32) -> Result<Vec<entities::Group>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.get_groups(uid, limit, offset).await
    }

    /// 按 group_id 获取单个群（ENTITY_SYNC_V1 group tombstone 等）
    pub async fn get_group(&self, group_id: u64) -> Result<Option<entities::Group>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.get_group(uid, group_id).await
    }

    /// 批量保存群（ENTITY_SYNC_V1 群同步）
    pub async fn save_groups(&self, groups: Vec<entities::Group>) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;
        self.db_actor.save_groups(uid, groups).await
    }

    /// 删除好友（user_id = 好友的 user_id）
    pub async fn delete_friend(&self, user_id: u64) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor.delete_friend(uid, user_id).await
    }

    /// 根据对方 user_id 查找私聊的 channel_id
    pub async fn find_channel_id_by_user(&self, target_user_id: u64) -> Result<Option<u64>> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor
            .find_channel_id_by_user(uid, target_user_id)
            .await
    }

    /// 更新频道的 save 字段（收藏状态）
    pub async fn update_channel_save(
        &self,
        channel_id: u64,
        channel_type: i32,
        save: i32,
    ) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor
            .update_channel_save(uid, channel_id, channel_type as u8, save)
            .await
    }

    /// 更新频道的 mute 字段（通知模式）
    pub async fn update_channel_mute(
        &self,
        channel_id: u64,
        channel_type: i32,
        mute: i32,
    ) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor
            .update_channel_mute(uid, channel_id, channel_type as u8, mute)
            .await
    }

    /// 更新会话的 extra 字段
    pub async fn update_channel_extra(
        &self,
        channel_id: u64,
        channel_type: u8,
        extra: String,
    ) -> Result<()> {
        let uid = self
            .current_user
            .read()
            .await
            .clone()
            .ok_or_else(|| PrivchatSDKError::Other("用户未初始化".to_string()))?;

        self.db_actor
            .update_channel_extra(uid, channel_id, channel_type, extra)
            .await
    }

    /// 获取数据库统计信息（请通过 db_actor 使用）
    pub async fn get_stats(&self) -> Result<dao::migration::DatabaseStats> {
        Err(PrivchatSDKError::Other(
            "get_stats 已移除直接连接，请使用 db_actor".to_string(),
        ))
    }

    /// 清理过期数据（请通过 db_actor 使用）
    pub async fn cleanup_expired_data(&self) -> Result<u32> {
        Err(PrivchatSDKError::Other(
            "cleanup_expired_data 已移除直接连接，请使用 db_actor".to_string(),
        ))
    }

    /// 恢复发送队列中的任务（请通过 db_actor 使用）
    pub async fn recover_send_queue(&self) -> Result<()> {
        Err(PrivchatSDKError::Other(
            "recover_send_queue 已移除直接连接，请使用 db_actor 或队列模块".to_string(),
        ))
    }
}

/// 存储统计信息
#[derive(Debug, Clone)]
pub struct StorageStats {
    pub total_users: usize,
    pub current_user: Option<String>,
    pub database_stats: Option<dao::migration::DatabaseStats>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tokio;

    #[tokio::test]
    async fn test_assets_cache_functionality() {
        // 创建临时目录
        let temp_dir = TempDir::new().unwrap();
        let user_data_dir = temp_dir.path().join("data");
        let assets_dir = temp_dir.path().join("assets");

        // 创建assets目录和测试文件
        tokio::fs::create_dir_all(&assets_dir).await.unwrap();

        let test_sql = "CREATE TABLE test (id INTEGER PRIMARY KEY);";
        let test_file = assets_dir.join("20240101000001.sql");
        tokio::fs::write(&test_file, test_sql).await.unwrap();

        // 创建存储管理器
        let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir)
            .await
            .unwrap();

        // 初始化用户
        let uid = "test_user";
        storage_manager.init_user_with_assets(uid).await.unwrap();
        storage_manager.switch_user(uid).await.unwrap();

        // 检查缓存是否创建
        let cache_info = storage_manager.get_assets_cache_info().await.unwrap();
        assert!(cache_info.is_some());

        let cache = cache_info.unwrap();
        assert_eq!(cache.sdk_version, SDK_VERSION);
        assert_eq!(cache.assets_path, assets_dir.to_string_lossy());
        assert_eq!(cache.file_timestamps.len(), 1);
        assert!(cache.file_timestamps.contains_key("20240101000001.sql"));

        // 清理缓存
        storage_manager.clear_assets_cache().await.unwrap();

        // 确认缓存已清理
        let cache_info_after_clear = storage_manager.get_assets_cache_info().await.unwrap();
        assert!(cache_info_after_clear.is_none());

        println!("✅ Assets缓存功能测试通过");
    }

    #[tokio::test]
    async fn test_smart_migration_check() {
        let temp_dir = TempDir::new().unwrap();
        let user_data_dir = temp_dir.path().join("data");
        let assets_dir = temp_dir.path().join("assets");

        // 创建assets目录和测试文件
        tokio::fs::create_dir_all(&assets_dir).await.unwrap();

        let test_sql = "CREATE TABLE test (id INTEGER PRIMARY KEY);";
        let test_file = assets_dir.join("20240101000001.sql");
        tokio::fs::write(&test_file, test_sql).await.unwrap();

        let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir)
            .await
            .unwrap();

        let uid = "test_user";

        // 首次初始化应该需要迁移
        storage_manager.init_user_with_assets(uid).await.unwrap();

        // 检查缓存是否创建
        let cache_info = storage_manager.get_assets_cache_info().await.unwrap();
        assert!(cache_info.is_some());

        // 添加新文件
        let new_sql = "ALTER TABLE test ADD COLUMN name TEXT;";
        let new_file = assets_dir.join("20240201000001.sql");
        tokio::fs::write(&new_file, new_sql).await.unwrap();

        // 再次初始化应该检测到变化
        storage_manager.init_user_with_assets(uid).await.unwrap();

        // 检查缓存是否更新
        let updated_cache = storage_manager
            .get_assets_cache_info()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated_cache.file_timestamps.len(), 2);

        println!("✅ 智能迁移检查测试通过");
    }
}
