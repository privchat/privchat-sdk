//! SQLite 存储模块 - 结构化数据和全文搜索
//! 
//! 本模块提供：
//! - SQLCipher 加密数据库
//! - FTS5 全文搜索功能
//! - 消息、用户、频道等数据存储
//! - 事务管理和连接池

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::{RwLock, Mutex};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use crate::error::{PrivchatSDKError, Result};
use crate::storage::{SqliteStats};

/// SQLite 存储组件
#[derive(Debug)]
pub struct SqliteStore {
    base_path: PathBuf,
    /// 用户数据库连接池
    user_connections: Arc<RwLock<HashMap<String, Arc<Mutex<Connection>>>>>,
    /// 当前用户ID
    current_user: Arc<RwLock<Option<String>>>,
}

impl SqliteStore {
    /// 创建新的 SQLite 存储实例
    pub async fn new(base_path: &Path) -> Result<Self> {
        let base_path = base_path.to_path_buf();
        
        Ok(Self {
            base_path,
            user_connections: Arc::new(RwLock::new(HashMap::new())),
            current_user: Arc::new(RwLock::new(None)),
        })
    }
    
    /// 初始化用户数据库
    pub async fn init_user_database(&self, uid: &str) -> Result<()> {
        let user_dir = self.base_path.join("users").join(uid);
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户数据库目录失败: {}", e)))?;
        
        let db_path = user_dir.join("messages.db");
        let encryption_key = self.derive_encryption_key(uid);
        
        // 创建加密数据库连接
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;
        
        // 设置加密密钥
        conn.pragma_update(None, "key", &encryption_key)
            .map_err(|e| PrivchatSDKError::Database(format!("设置加密密钥失败: {}", e)))?;
        
        // 启用 WAL 模式和其他优化
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| PrivchatSDKError::Database(format!("设置 WAL 模式失败: {}", e)))?;
        
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| PrivchatSDKError::Database(format!("设置同步模式失败: {}", e)))?;
        
        conn.pragma_update(None, "cache_size", "-64000") // 64MB 缓存
            .map_err(|e| PrivchatSDKError::Database(format!("设置缓存大小失败: {}", e)))?;
        
        // 创建数据库表
        self.create_database_tables(&conn).await?;
        
        // 存储连接到连接池
        let mut connections = self.user_connections.write().await;
        connections.insert(uid.to_string(), Arc::new(Mutex::new(conn)));
        
        tracing::info!("用户数据库初始化完成: {}", uid);
        
        Ok(())
    }
    
    /// 切换用户
    pub async fn switch_user(&self, uid: &str) -> Result<()> {
        // 如果用户数据库不存在，先初始化
        let connections = self.user_connections.read().await;
        if !connections.contains_key(uid) {
            drop(connections);
            self.init_user_database(uid).await?;
        }
        
        // 更新当前用户
        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());
        
        Ok(())
    }
    
    /// 清理用户数据
    pub async fn cleanup_user_data(&self, uid: &str) -> Result<()> {
        let mut connections = self.user_connections.write().await;
        connections.remove(uid);
        
        Ok(())
    }
    
    /// 获取当前用户的数据库连接
    pub async fn get_connection(&self) -> Result<Arc<Mutex<Connection>>> {
        let current_user = self.current_user.read().await;
        let uid = current_user.as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        
        let connections = self.user_connections.read().await;
        let conn = connections.get(uid)
            .ok_or_else(|| PrivchatSDKError::Database("用户数据库连接不存在".to_string()))?;
        
        Ok(conn.clone())
    }
    
    /// 保存消息到数据库
    pub async fn save_message(&self, message: &MessageRecord) -> Result<i64> {
        let conn_mutex = self.get_connection().await?;
        let conn = conn_mutex.lock().await;
        
        let message_id = conn.execute(
            "INSERT INTO messages (message_id, message_type, from_uid, to_uid, content, timestamp, status) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                message.message_id,
                message.message_type,
                message.from_uid,
                message.to_uid,
                message.content,
                message.timestamp,
                message.status
            ],
        ).map_err(|e| PrivchatSDKError::Database(format!("保存消息失败: {}", e)))?;
        
        // 同时保存到 FTS 表用于全文搜索
        if !message.content.is_empty() {
            conn.execute(
                "INSERT INTO messages_fts (message_id, content) VALUES (?1, ?2)",
                params![message.message_id, message.content],
            ).map_err(|e| PrivchatSDKError::Database(format!("保存 FTS 索引失败: {}", e)))?;
        }
        
        Ok(message_id as i64)
    }
    
    /// 批量保存消息
    pub async fn save_messages(&self, messages: &[MessageRecord]) -> Result<Vec<i64>> {
        let conn_mutex = self.get_connection().await?;
        let conn = conn_mutex.lock().await;
        let mut message_ids = Vec::new();
        
        // 使用事务提高性能
        let tx = conn.unchecked_transaction()
            .map_err(|e| PrivchatSDKError::Database(format!("开启事务失败: {}", e)))?;
        
        for message in messages {
            let message_id = tx.execute(
                "INSERT INTO messages (message_id, message_type, from_uid, to_uid, content, timestamp, status) 
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message.message_id,
                    message.message_type,
                    message.from_uid,
                    message.to_uid,
                    message.content,
                    message.timestamp,
                    message.status
                ],
            ).map_err(|e| PrivchatSDKError::Database(format!("批量保存消息失败: {}", e)))?;
            
            // 同时保存到 FTS 表
            if !message.content.is_empty() {
                tx.execute(
                    "INSERT INTO messages_fts (message_id, content) VALUES (?1, ?2)",
                    params![message.message_id, message.content],
                ).map_err(|e| PrivchatSDKError::Database(format!("批量保存 FTS 索引失败: {}", e)))?;
            }
            
            message_ids.push(message_id as i64);
        }
        
        tx.commit().map_err(|e| PrivchatSDKError::Database(format!("提交事务失败: {}", e)))?;
        
        Ok(message_ids)
    }
    
    /// 查询消息
    pub async fn get_messages(&self, query: &MessageQuery) -> Result<Vec<MessageRecord>> {
        let conn_mutex = self.get_connection().await?;
        let conn = conn_mutex.lock().await;
        let mut messages = Vec::new();
        
        // 构建查询语句
        let mut sql = "SELECT message_id, message_type, from_uid, to_uid, content, timestamp, status FROM messages WHERE 1=1".to_string();
        let mut params = Vec::new();
        
        if let Some(from_uid) = &query.from_uid {
            sql.push_str(" AND from_uid = ?");
            params.push(from_uid.as_str());
        }
        
        if let Some(to_uid) = &query.to_uid {
            sql.push_str(" AND to_uid = ?");
            params.push(to_uid.as_str());
        }
        
        if let Some(message_type) = &query.message_type {
            sql.push_str(" AND message_type = ?");
            params.push(message_type.as_str());
        }
        
        let start_time_str = query.start_time.map(|t| t.to_string());
        if let Some(ref start_time_str) = start_time_str {
            sql.push_str(" AND timestamp >= ?");
            params.push(start_time_str.as_str());
        }
        
        let end_time_str = query.end_time.map(|t| t.to_string());
        if let Some(ref end_time_str) = end_time_str {
            sql.push_str(" AND timestamp <= ?");
            params.push(end_time_str.as_str());
        }
        
        sql.push_str(" ORDER BY timestamp DESC");
        
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT {}", limit));
        }
        
        let mut stmt = conn.prepare(&sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备查询语句失败: {}", e)))?;
        
        let message_iter = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(MessageRecord {
                message_id: row.get(0)?,
                message_type: row.get(1)?,
                from_uid: row.get(2)?,
                to_uid: row.get(3)?,
                content: row.get(4)?,
                timestamp: row.get(5)?,
                status: row.get(6)?,
            })
        }).map_err(|e| PrivchatSDKError::Database(format!("执行查询失败: {}", e)))?;
        
        for message in message_iter {
            messages.push(message.map_err(|e| PrivchatSDKError::Database(format!("读取消息记录失败: {}", e)))?);
        }
        
        Ok(messages)
    }
    
    /// 全文搜索消息
    pub async fn search_messages(&self, search_query: &str, limit: Option<u32>) -> Result<Vec<MessageRecord>> {
        let conn_mutex = self.get_connection().await?;
        let conn = conn_mutex.lock().await;
        let mut messages = Vec::new();
        
        let sql = "SELECT m.message_id, m.message_type, m.from_uid, m.to_uid, m.content, m.timestamp, m.status 
                   FROM messages m 
                   JOIN messages_fts fts ON m.message_id = fts.message_id 
                   WHERE messages_fts MATCH ?1 
                   ORDER BY m.timestamp DESC";
        
        let final_sql = if let Some(limit) = limit {
            format!("{} LIMIT {}", sql, limit)
        } else {
            sql.to_string()
        };
        
        let mut stmt = conn.prepare(&final_sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备搜索语句失败: {}", e)))?;
        
        let message_iter = stmt.query_map(params![search_query], |row| {
            Ok(MessageRecord {
                message_id: row.get(0)?,
                message_type: row.get(1)?,
                from_uid: row.get(2)?,
                to_uid: row.get(3)?,
                content: row.get(4)?,
                timestamp: row.get(5)?,
                status: row.get(6)?,
            })
        }).map_err(|e| PrivchatSDKError::Database(format!("执行搜索失败: {}", e)))?;
        
        for message in message_iter {
            messages.push(message.map_err(|e| PrivchatSDKError::Database(format!("读取搜索结果失败: {}", e)))?);
        }
        
        Ok(messages)
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Result<SqliteStats> {
        let conn_mutex = self.get_connection().await?;
        let conn = conn_mutex.lock().await;
        
        // 获取消息数量
        let message_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM messages",
            [],
            |row| row.get(0)
        ).map_err(|e| PrivchatSDKError::Database(format!("获取消息数量失败: {}", e)))?;
        
        // 获取用户数量
        let user_count: u64 = conn.query_row(
            "SELECT COUNT(DISTINCT from_uid) FROM messages",
            [],
            |row| row.get(0)
        ).map_err(|e| PrivchatSDKError::Database(format!("获取用户数量失败: {}", e)))?;
        
        // 获取频道数量
        let channel_count: u64 = conn.query_row(
            "SELECT COUNT(*) FROM channels",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        // 获取数据库大小
        let database_size: u64 = conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        Ok(SqliteStats {
            database_size,
            message_count,
            user_count,
            channel_count,
            table_count: 4, // messages, channels, settings, message_fts
            total_records: message_count + user_count + channel_count,
        })
    }
    
    /// 创建数据库表
    async fn create_database_tables(&self, conn: &Connection) -> Result<()> {
        // 创建消息表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL UNIQUE,
                message_type TEXT NOT NULL,
                from_uid TEXT NOT NULL,
                to_uid TEXT NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建消息表失败: {}", e)))?;
        
        // 创建 FTS5 全文搜索表
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                message_id UNINDEXED,
                content,
                content='messages',
                content_rowid='id'
            )",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建 FTS 表失败: {}", e)))?;
        
        // 创建频道表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS channels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL UNIQUE,
                channel_name TEXT NOT NULL,
                channel_type TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建频道表失败: {}", e)))?;
        
        // 创建用户设置表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL UNIQUE,
                value TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建设置表失败: {}", e)))?;
        
        // 创建索引
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp)",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建时间戳索引失败: {}", e)))?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_from_uid ON messages(from_uid)",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建发送者索引失败: {}", e)))?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_to_uid ON messages(to_uid)",
            [],
        ).map_err(|e| PrivchatSDKError::Database(format!("创建接收者索引失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 派生加密密钥
    fn derive_encryption_key(&self, uid: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"privchat_sdk_encryption_key_v1");
        hasher.update(uid.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }
}

/// 消息记录
#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub message_id: String,
    pub message_type: String,
    pub from_uid: String,
    pub to_uid: String,
    pub content: String,
    pub timestamp: i64,
    pub status: String,
}

/// 消息查询参数
#[derive(Debug, Clone, Default)]
pub struct MessageQuery {
    pub from_uid: Option<String>,
    pub to_uid: Option<String>,
    pub message_type: Option<String>,
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use chrono::Utc;
    
    #[tokio::test]
    async fn test_sqlite_store_init() {
        let temp_dir = TempDir::new().unwrap();
        let store = SqliteStore::new(temp_dir.path()).await.unwrap();
        
        // 初始化用户数据库
        store.init_user_database("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 验证能够获取连接
        let conn = store.get_connection().await.unwrap();
        
        // 验证表已创建
        let conn_guard = conn.lock().await;
        let table_exists: bool = conn_guard.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')",
            [],
            |row| row.get(0)
        ).unwrap();
        
        assert!(table_exists);
    }
    
    #[tokio::test]
    async fn test_message_operations() {
        let temp_dir = TempDir::new().unwrap();
        let store = SqliteStore::new(temp_dir.path()).await.unwrap();
        
        store.init_user_database("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 保存消息
        let message = MessageRecord {
            message_id: "msg_123".to_string(),
            message_type: "text".to_string(),
            from_uid: "user_1".to_string(),
            to_uid: "user_2".to_string(),
            content: "Hello, world!".to_string(),
            timestamp: Utc::now().timestamp_millis(),
            status: "sent".to_string(),
        };
        
        let message_id = store.save_message(&message).await.unwrap();
        assert!(message_id > 0);
        
        // 查询消息
        let query = MessageQuery {
            from_uid: Some("user_1".to_string()),
            ..Default::default()
        };
        
        let messages = store.get_messages(&query).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, "msg_123");
        
        // 全文搜索
        let search_results = store.search_messages("Hello", Some(10)).await.unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].content, "Hello, world!");
    }
    
    #[tokio::test]
    async fn test_batch_save_messages() {
        let temp_dir = TempDir::new().unwrap();
        let store = SqliteStore::new(temp_dir.path()).await.unwrap();
        
        store.init_user_database("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 批量保存消息
        let messages = vec![
            MessageRecord {
                message_id: "msg_1".to_string(),
                message_type: "text".to_string(),
                from_uid: "user_1".to_string(),
                to_uid: "user_2".to_string(),
                content: "Message 1".to_string(),
                timestamp: Utc::now().timestamp_millis(),
                status: "sent".to_string(),
            },
            MessageRecord {
                message_id: "msg_2".to_string(),
                message_type: "text".to_string(),
                from_uid: "user_2".to_string(),
                to_uid: "user_1".to_string(),
                content: "Message 2".to_string(),
                timestamp: Utc::now().timestamp_millis(),
                status: "sent".to_string(),
            },
        ];
        
        let message_ids = store.save_messages(&messages).await.unwrap();
        assert_eq!(message_ids.len(), 2);
        
        // 验证消息已保存
        let query = MessageQuery::default();
        let saved_messages = store.get_messages(&query).await.unwrap();
        assert_eq!(saved_messages.len(), 2);
    }
} 