//! 存储模块 - 现代化 IM SDK 的数据持久化层
//! 
//! 采用分层架构设计：
//! - StorageManager: 统一的存储管理器，提供高级 API
//! - DAO Layer: 数据访问层，每张表一个专门的操作模块
//! - Entities: 数据实体定义，类型安全的数据传输
//! - 支持多用户、事务管理、数据迁移

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::{HashMap, BTreeMap};
use tokio::sync::{RwLock, Mutex};
use rusqlite::Connection;
use serde::{Serialize, Deserialize};
use tracing;

use crate::error::{PrivchatSDKError, Result};
use crate::storage::entities::{Message, Conversation, Channel, MessageExtra};
use crate::storage::queue::{TaskQueue, TaskQueueTrait, SendTask, MessageData, QueuePriority, TaskStatus};

pub mod dao;
pub mod entities;
pub mod sqlite;
pub mod kv;
pub mod message_state;
pub mod queue;
pub mod media;
pub mod migration;
pub mod advanced_features;
pub mod advanced_features_integration;

// 重新导出核心类型
pub use entities::*;
pub use dao::{DaoFactory, TransactionManager};
pub use dao::migration::MigrationDao;
pub use advanced_features::{
    AdvancedFeaturesManager, ReadReceiptEvent, MessageRevokeEvent, 
    MessageEditEvent, ConversationReadState
};
pub use advanced_features_integration::AdvancedFeaturesIntegration;

/// SDK 版本号 - 用于缓存失效检查
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    /// 用户数据库连接池
    user_connections: Arc<RwLock<HashMap<String, Arc<Mutex<Connection>>>>>,
    /// 当前活跃用户
    current_user: Arc<RwLock<Option<String>>>,
    /// KV 存储
    kv_store: Option<Arc<crate::storage::kv::KvStore>>,
    /// 队列管理器
    queue_manager: Option<Arc<dyn crate::storage::queue::TaskQueueTrait + Send + Sync>>,
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
        tokio::fs::create_dir_all(base_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建存储目录失败: {}", e)))?;
        
        // 如果提供了 assets 目录，确保它存在
        if let Some(assets_path) = assets_path {
            tokio::fs::create_dir_all(assets_path).await
                .map_err(|e| PrivchatSDKError::IO(format!("创建 assets 目录失败: {}", e)))?;
        }
        
        // 初始化 KV 存储
        let kv_store = Arc::new(crate::storage::kv::KvStore::new(base_path).await?);
        
        Ok(Self {
            base_path: base_path.to_path_buf(),
            assets_path: assets_path.map(|p| p.to_path_buf()),
            user_connections: Arc::new(RwLock::new(HashMap::new())),
            current_user: Arc::new(RwLock::new(None)),
            kv_store: Some(kv_store.clone()),
            queue_manager: Some(Arc::new(crate::storage::queue::TaskQueue::Persistent(
                crate::storage::queue::PersistentTaskQueue::new(kv_store, String::new())
            ))),
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
    
    /// 初始化用户数据库（使用嵌入式 SQL）
    pub async fn init_user(&self, uid: &str) -> Result<()> {
        // 初始化 KV 存储
        if let Some(kv_store) = &self.kv_store {
            kv_store.init_user_tree(uid).await?;
        }
        
        let user_dir = self.user_dir(uid);
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;
        
        let db_path = user_dir.join("messages.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;
        
        // 执行数据库迁移
        let migration_dao = dao::MigrationDao::new(&conn);
        migration_dao.migrate()?;
        
        // 验证数据库结构
        if !migration_dao.validate_schema()? {
            return Err(PrivchatSDKError::Database("数据库结构验证失败".to_string()));
        }
        
        // 存储连接到连接池
        let mut connections = self.user_connections.write().await;
        connections.insert(uid.to_string(), Arc::new(Mutex::new(conn)));
        
        tracing::info!("用户数据库初始化完成: {} (使用嵌入式 SQL)", uid);
        
        Ok(())
    }
    
    /// 初始化用户数据库（使用外部 assets 目录）
    pub async fn init_user_with_assets(&self, uid: &str) -> Result<()> {
        let assets_path = self.assets_path.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("未设置 assets 目录路径".to_string()))?;
        
        self.init_user_with_smart_migration(uid, assets_path).await
    }
    
    /// 初始化用户数据库（使用自定义 assets 目录）
    pub async fn init_user_with_custom_assets(&self, uid: &str, assets_path: &Path) -> Result<()> {
        self.init_user_with_smart_migration(uid, assets_path).await
    }
    
    /// 智能迁移初始化 - 使用缓存优化
    async fn init_user_with_smart_migration(&self, uid: &str, assets_path: &Path) -> Result<()> {
        // 初始化 KV 存储
        if let Some(kv_store) = &self.kv_store {
            kv_store.init_user_tree(uid).await?;
            kv_store.switch_user(uid).await?;
        }
        
        let user_dir = self.user_dir(uid);
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;
        
        let db_path = user_dir.join("messages.db");
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;
        
        // 检查是否需要重新扫描 assets 目录
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
        
        // 存储连接到连接池
        let mut connections = self.user_connections.write().await;
        connections.insert(uid.to_string(), Arc::new(Mutex::new(conn)));
        
        tracing::info!("用户数据库初始化完成: {} (使用智能迁移)", uid);
        
        Ok(())
    }
    
    /// 检查是否需要重新扫描 assets 目录执行迁移
    async fn check_need_migration(&self, assets_path: &Path) -> Result<bool> {
        let kv_store = self.kv_store.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化".to_string()))?;
        
        // 获取缓存的 assets 信息
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
            tracing::info!("assets 目录路径变化: {} -> {}", cache.assets_path, assets_path.display());
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
    async fn scan_assets_timestamps(&self, assets_path: &Path) -> Result<std::collections::BTreeMap<String, u64>> {
        let mut file_timestamps = std::collections::BTreeMap::new();
        
        if !assets_path.exists() {
            return Ok(file_timestamps);
        }
        
        let mut entries = tokio::fs::read_dir(assets_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("读取 assets 目录失败: {}", e)))?;
        
        while let Some(entry) = entries.next_entry().await
            .map_err(|e| PrivchatSDKError::IO(format!("遍历 assets 目录失败: {}", e)))? {
            
            let path = entry.path();
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if file_name.ends_with(".sql") {
                        let metadata = entry.metadata().await
                            .map_err(|e| PrivchatSDKError::IO(format!("获取文件元数据失败: {}", e)))?;
                        
                        let timestamp = metadata.modified()
                            .map_err(|e| PrivchatSDKError::IO(format!("获取文件修改时间失败: {}", e)))?
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
        let kv_store = self.kv_store.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化".to_string()))?;
        
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
    
    /// 获取用户数据目录
    pub fn user_dir(&self, uid: &str) -> PathBuf {
        self.base_path.join("users").join(uid)
    }
    
    /// 获取基础数据目录
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }
    
    /// 清理用户的 assets 缓存
    pub async fn clear_assets_cache(&self) -> Result<()> {
        let kv_store = self.kv_store.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化".to_string()))?;
        
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
        let kv_store = self.kv_store.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("KV 存储未初始化".to_string()))?;
        
        let cache: Option<AssetsCache> = kv_store.get(cache_keys::ASSETS_CACHE).await?;
        Ok(cache)
    }
    
    /// 获取KV存储实例的引用
    pub fn kv_store(&self) -> Option<&Arc<crate::storage::kv::KvStore>> {
        self.kv_store.as_ref()
    }
    
    /// 切换当前用户
    pub async fn switch_user(&self, uid: &str) -> Result<()> {
        // 检查用户数据库是否存在
        let connections = self.user_connections.read().await;
        if !connections.contains_key(uid) {
            return Err(PrivchatSDKError::Database("用户数据库不存在，请先初始化".to_string()));
        }
        drop(connections);
        
        // 切换 KV 存储用户
        if let Some(kv_store) = &self.kv_store {
            kv_store.switch_user(uid).await?;
        }
        
        // 切换当前用户
        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());
        
        tracing::info!("用户切换完成: {}", uid);
        
        Ok(())
    }
    
    /// 获取当前用户的数据库连接
    async fn get_connection(&self) -> Result<Arc<Mutex<Connection>>> {
        let current_user = self.current_user.read().await;
        let uid = current_user.as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        
        let connections = self.user_connections.read().await;
        let conn = connections.get(uid)
            .ok_or_else(|| PrivchatSDKError::Database("用户数据库连接不存在".to_string()))?;
        
        Ok(conn.clone())
    }
    
    // ===== 消息相关的高级 API =====
    
    /// 发送消息 - 统一的消息发送接口
    pub async fn send_message(
        &self,
        channel_id: &str,
        channel_type: i32,
        from_uid: &str,
        content: &str,
        message_type: i32,
    ) -> Result<String> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_id = uuid::Uuid::new_v4().to_string();
        let client_msg_no = format!("{}_{}", from_uid, chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
        
        let message = Message {
            client_seq: None,
            message_id: Some(message_id.clone()),
            message_seq: chrono::Utc::now().timestamp_millis(),
            channel_id: channel_id.to_string(),
            channel_type,
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            from_uid: from_uid.to_string(),
            message_type,
            content: content.to_string(),
            status: 0, // 发送中
            voice_status: 0,
            created_at: "".to_string(),
            updated_at: "".to_string(),
            searchable_word: content.to_string(),
            client_msg_no,
            is_deleted: 0,
            setting: 0,
            order_seq: chrono::Utc::now().timestamp_millis(),
            extra: "{}".to_string(),
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: "".to_string(),
            expire_time: None,
            expire_timestamp: None,
        };
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.insert(&message)?;
        
        // 更新会话信息
        self.update_conversation_after_message(&*conn_guard, &message)?;
        
        Ok(message_id)
    }
    
    /// 撤回消息
    pub async fn revoke_message(&self, message_id: &str, revoker: &str) -> Result<()> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.revoke(message_id, revoker)?;
        
        Ok(())
    }
    
    /// 编辑消息
    pub async fn edit_message(&self, message_id: &str, new_content: &str) -> Result<()> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.edit(message_id, new_content)?;
        
        Ok(())
    }
    
    /// 查询消息
    pub async fn query_messages(&self, query: &MessageQuery) -> Result<PageResult<Message>> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.query(query)
    }
    
    /// 搜索消息
    pub async fn search_messages(
        &self,
        channel_id: &str,
        channel_type: i32,
        keyword: &str,
        limit: Option<u32>,
    ) -> Result<Vec<Message>> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.search(channel_id, channel_type, keyword, limit)
    }
    
    // ===== 会话相关的高级 API =====
    
    /// 获取会话列表
    pub async fn get_conversations(&self, query: &ConversationQuery) -> Result<Vec<Conversation>> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        // 这里可以实现分页查询逻辑
        let sql = "SELECT * FROM conversation WHERE is_deleted = 0 ORDER BY last_msg_timestamp DESC";
        let mut stmt = conn_guard.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(Conversation {
                id: row.get("id")?,
                channel_id: row.get("channel_id")?,
                channel_type: row.get("channel_type")?,
                last_client_msg_no: row.get("last_client_msg_no")?,
                last_msg_timestamp: row.get("last_msg_timestamp")?,
                unread_count: row.get("unread_count")?,
                is_deleted: row.get("is_deleted")?,
                version: row.get("version")?,
                extra: row.get("extra")?,
                last_msg_seq: row.get("last_msg_seq")?,
                parent_channel_id: row.get("parent_channel_id")?,
                parent_channel_type: row.get("parent_channel_type")?,
            })
        })?;
        
        let mut conversations = Vec::new();
        for row in rows {
            conversations.push(row?);
        }
        
        Ok(conversations)
    }
    
    /// 标记会话为已读
    pub async fn mark_conversation_read(&self, channel_id: &str, channel_type: i32) -> Result<()> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let conversation_dao = dao::ConversationDao::new(&*conn_guard);
        conversation_dao.update_unread_count(channel_id, channel_type, 0)?;
        
        Ok(())
    }
    
    // ===== 频道相关的高级 API =====
    
    /// 获取频道信息
    pub async fn get_channel(&self, channel_id: &str, channel_type: i32) -> Result<Option<Channel>> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let channel_dao = dao::ChannelDao::new(&*conn_guard);
        channel_dao.get_by_id(channel_id, channel_type)
    }
    
    // ===== 事务管理 =====
    
    /// 执行事务操作
    pub async fn execute_transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R>,
    {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let tx_manager = dao::TransactionManager::new(&*conn_guard);
        tx_manager.execute(f)
    }
    
    // ===== 内部辅助方法 =====
    
    /// 更新会话信息（在发送消息后）
    fn update_conversation_after_message(&self, conn: &Connection, message: &Message) -> Result<()> {
        let conversation_dao = dao::ConversationDao::new(conn);
        
        // 查找现有会话
        if let Some(mut conversation) = conversation_dao.get_by_channel(&message.channel_id, message.channel_type)? {
            // 更新会话信息
            conversation.last_client_msg_no = message.client_msg_no.clone();
            conversation.last_msg_timestamp = message.timestamp;
            conversation.last_msg_seq = message.message_seq;
            conversation.version += 1;
            
            conversation_dao.upsert(&conversation)?;
        } else {
            // 创建新会话
            let new_conversation = Conversation {
                id: None,
                channel_id: message.channel_id.clone(),
                channel_type: message.channel_type,
                last_client_msg_no: message.client_msg_no.clone(),
                last_msg_timestamp: message.timestamp,
                unread_count: 0,
                is_deleted: 0,
                version: 1,
                extra: "{}".to_string(),
                last_msg_seq: message.message_seq,
                parent_channel_id: "".to_string(),
                parent_channel_type: 0,
            };
            
            conversation_dao.upsert(&new_conversation)?;
        }
        
        Ok(())
    }
    
    /// 获取数据库统计信息
    pub async fn get_stats(&self) -> Result<dao::migration::DatabaseStats> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let migration_dao = dao::MigrationDao::new(&*conn_guard);
        migration_dao.get_database_stats()
    }
    
    /// 清理过期数据
    pub async fn cleanup_expired_data(&self) -> Result<u32> {
        let conn = self.get_connection().await?;
        let conn_guard = conn.lock().await;
        
        let message_dao = dao::MessageDao::new(&*conn_guard);
        message_dao.delete_expired()
    }

    /// 恢复发送队列中的任务
    pub async fn recover_send_queue(&self) -> Result<()> {
        let conn = self.get_connection().await?;
        let conn = conn.lock().await;
        
        let mut stmt = conn.prepare(
            "SELECT m.*, me.* FROM messages m 
             LEFT JOIN message_extra me ON m.message_id = me.message_id 
             WHERE m.status IN (1, 8)"
        )?;
        
        let mut rows = stmt.query([])?;
        
        while let Some(row) = rows.next()? {
            let message = Message {
                client_seq: row.get(0)?,
                message_id: row.get(1)?,
                message_seq: row.get(2)?,
                channel_id: row.get(3)?,
                channel_type: row.get(4)?,
                timestamp: row.get(5)?,
                from_uid: row.get(6)?,
                message_type: row.get(7)?,
                content: row.get(8)?,
                status: row.get(9)?,
                voice_status: row.get(10)?,
                created_at: row.get(11)?,
                updated_at: row.get(12)?,
                searchable_word: row.get(13)?,
                client_msg_no: row.get(14)?,
                is_deleted: row.get(15)?,
                setting: row.get(16)?,
                order_seq: row.get(17)?,
                extra: row.get(18)?,
                flame: row.get(19)?,
                flame_second: row.get(20)?,
                viewed: row.get(21)?,
                viewed_at: row.get(22)?,
                topic_id: row.get(23)?,
                expire_time: row.get(24)?,
                expire_timestamp: row.get(25)?,
            };
            
            let message_data = MessageData {
                client_msg_no: message.client_msg_no.clone(),
                channel_id: message.channel_id.clone(),
                channel_type: message.channel_type,
                from_uid: message.from_uid.clone(),
                content: message.content.clone(),
                message_type: message.message_type,
                extra: HashMap::new(),
                created_at: message.timestamp.unwrap_or_else(|| chrono::Utc::now().timestamp()) as u64,
                expires_at: message.expire_timestamp.map(|t| t as u64),
            };
            
            let task = SendTask::new(
                message.client_msg_no.clone(),
                message.channel_id.clone(),
                message_data,
                QueuePriority::Normal,
            );
            
            if let Some(queue_manager) = &self.queue_manager {
                queue_manager.push(task).await?;
            }
        }
        
        Ok(())
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
        let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await.unwrap();
        
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
        
        let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await.unwrap();
        
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
        let updated_cache = storage_manager.get_assets_cache_info().await.unwrap().unwrap();
        assert_eq!(updated_cache.file_timestamps.len(), 2);
        
        println!("✅ 智能迁移检查测试通过");
    }
} 