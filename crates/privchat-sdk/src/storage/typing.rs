use crate::PrivchatSDKError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

/// 用户输入状态事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypingEvent {
    /// 用户ID
    pub user_id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 是否正在输入
    pub is_typing: bool,
    /// 事件时间戳
    pub timestamp: u64,
    /// 会话ID（可选，用于群组中的特定会话）
    pub session_id: Option<u64>,
}

/// 用户输入状态记录
#[derive(Debug, Clone)]
pub struct TypingState {
    /// 用户ID
    pub user_id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 是否正在输入
    pub is_typing: bool,
    /// 最后更新时间
    pub last_updated: u64,
    /// 会话ID
    pub session_id: Option<u64>,
}

/// Typing Indicator 管理器配置
#[derive(Debug, Clone)]
pub struct TypingManagerConfig {
    /// 输入状态超时时间（秒），默认30秒
    pub typing_timeout: u64,
    /// 清理过期状态的间隔（秒），默认60秒
    pub cleanup_interval: u64,
    /// 是否启用数据库持久化
    pub enable_persistence: bool,
}

impl Default for TypingManagerConfig {
    fn default() -> Self {
        Self {
            typing_timeout: 30,
            cleanup_interval: 60,
            enable_persistence: false, // 默认不持久化，只在内存中管理
        }
    }
}

/// Typing Indicator 管理器
pub struct TypingManager {
    /// 数据库连接（可选）
    conn: Option<Arc<Mutex<Connection>>>,
    /// 内存中的输入状态缓存
    typing_states: Arc<Mutex<HashMap<String, TypingState>>>,
    /// 配置
    config: TypingManagerConfig,
}

impl TypingManager {
    /// 创建新的 Typing Manager
    pub fn new(config: TypingManagerConfig) -> Self {
        Self {
            conn: None,
            typing_states: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// 使用数据库连接创建 Typing Manager
    pub fn with_database(conn: Connection, config: TypingManagerConfig) -> crate::Result<Self> {
        let conn = Arc::new(Mutex::new(conn));
        let manager = Self {
            conn: Some(conn.clone()),
            typing_states: Arc::new(Mutex::new(HashMap::new())),
            config,
        };

        // 如果启用持久化，初始化数据库表
        if manager.config.enable_persistence {
            manager.initialize_tables()?;
        }

        Ok(manager)
    }

    /// 初始化数据库表
    fn initialize_tables(&self) -> crate::Result<()> {
        if let Some(ref conn) = self.conn {
            let conn = conn.lock().unwrap();
            
            // 创建输入状态表
            conn.execute(
                "CREATE TABLE IF NOT EXISTS typing_states (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    user_id TEXT NOT NULL,
                    channel_id TEXT NOT NULL,
                    channel_type INTEGER NOT NULL,
                    is_typing INTEGER NOT NULL,
                    last_updated INTEGER NOT NULL,
                    session_id TEXT,
                    UNIQUE(user_id, channel_id, channel_type)
                )",
                [],
            )?;

            // 创建索引
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_typing_channel 
                 ON typing_states(channel_id, channel_type)",
                [],
            )?;

            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_typing_user 
                 ON typing_states(user_id)",
                [],
            )?;

            info!("Typing manager database tables initialized");
        }

        Ok(())
    }

    /// 更新用户输入状态
    pub fn update_typing_state(
        &self,
        user_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_typing: bool,
        session_id: Option<u64>,
    ) -> crate::Result<TypingEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let state_key = format!("{}:{}:{}", user_id, channel_id, channel_type);
        
        let typing_state = TypingState {
            user_id: user_id,
            channel_id: channel_id,
            channel_type,
            is_typing,
            last_updated: now,
            session_id: session_id,
        };

        // 更新内存缓存
        {
            let mut states = self.typing_states.lock().unwrap();
            if is_typing {
                states.insert(state_key.clone(), typing_state.clone());
            } else {
                states.remove(&state_key);
            }
        }

        // 如果启用持久化，更新数据库
        if self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                if is_typing {
                    conn.execute(
                        "INSERT OR REPLACE INTO typing_states 
                         (user_id, channel_id, channel_type, is_typing, last_updated, session_id)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            user_id,
                            channel_id,
                            channel_type,
                            if is_typing { 1 } else { 0 },
                            now,
                            session_id
                        ],
                    )?;
                } else {
                    conn.execute(
                        "DELETE FROM typing_states 
                         WHERE user_id = ?1 AND channel_id = ?2 AND channel_type = ?3",
                        params![user_id, channel_id, channel_type],
                    )?;
                }
            }
        }

        debug!(
            "Updated typing state for user {} in channel {}: {}",
            user_id, channel_id, is_typing
        );

        Ok(TypingEvent {
            user_id: user_id,
            channel_id: channel_id,
            channel_type,
            is_typing,
            timestamp: now,
            session_id: session_id,
        })
    }

    /// 获取频道中正在输入的用户列表
    pub fn get_typing_users(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> crate::Result<Vec<TypingState>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut typing_users = Vec::new();

        // 从内存缓存中获取
        {
            let states = self.typing_states.lock().unwrap();
            for state in states.values() {
                if state.channel_id == channel_id 
                    && state.channel_type == channel_type 
                    && state.is_typing
                    && (now - state.last_updated) < self.config.typing_timeout {
                    typing_users.push(state.clone());
                }
            }
        }

        // 如果启用持久化且内存中没有数据，从数据库中获取
        if typing_users.is_empty() && self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                let mut stmt = conn.prepare(
                    "SELECT user_id, channel_id, channel_type, is_typing, last_updated, session_id
                     FROM typing_states 
                     WHERE channel_id = ?1 AND channel_type = ?2 AND is_typing = 1
                     AND last_updated > ?3"
                )?;

                let cutoff_time = now - self.config.typing_timeout;
                let rows = stmt.query_map(
                    params![channel_id, channel_type, cutoff_time],
                    |row| {
                        Ok(TypingState {
                            user_id: row.get(0)?,
                            channel_id: row.get(1)?,
                            channel_type: row.get(2)?,
                            is_typing: row.get::<_, i32>(3)? == 1,
                            last_updated: row.get(4)?,
                            session_id: row.get(5)?,
                        })
                    },
                )?;

                for row in rows {
                    typing_users.push(row?);
                }
            }
        }

        Ok(typing_users)
    }

    /// 获取用户的输入状态
    pub fn get_user_typing_state(
        &self,
        user_id: u64,
        channel_id: u64,
        channel_type: i32,
    ) -> crate::Result<Option<TypingState>> {
        let state_key = format!("{}:{}:{}", user_id, channel_id, channel_type);
        
        // 首先从内存中查找
        {
            let states = self.typing_states.lock().unwrap();
            if let Some(state) = states.get(&state_key) {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                
                // 检查是否过期
                if (now - state.last_updated) < self.config.typing_timeout {
                    return Ok(Some(state.clone()));
                }
            }
        }

        // 如果内存中没有或已过期，从数据库中查找
        if self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let cutoff_time = now - self.config.typing_timeout;

                let result = conn.query_row(
                    "SELECT user_id, channel_id, channel_type, is_typing, last_updated, session_id
                     FROM typing_states 
                     WHERE user_id = ?1 AND channel_id = ?2 AND channel_type = ?3 
                     AND last_updated > ?4",
                    params![user_id, channel_id, channel_type, cutoff_time],
                    |row| {
                        Ok(TypingState {
                            user_id: row.get(0)?,
                            channel_id: row.get(1)?,
                            channel_type: row.get(2)?,
                            is_typing: row.get::<_, i32>(3)? == 1,
                            last_updated: row.get(4)?,
                            session_id: row.get(5)?,
                        })
                    },
                );

                match result {
                    Ok(state) => return Ok(Some(state)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                    Err(e) => return Err(PrivchatSDKError::Database(e.to_string())),
                }
            }
        }

        Ok(None)
    }

    /// 清理过期的输入状态
    pub fn cleanup_expired_states(&self) -> crate::Result<usize> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cutoff_time = now - self.config.typing_timeout;

        let mut removed_count = 0;

        // 清理内存缓存
        {
            let mut states = self.typing_states.lock().unwrap();
            let mut keys_to_remove = Vec::new();
            
            for (key, state) in states.iter() {
                if state.last_updated < cutoff_time {
                    keys_to_remove.push(key.clone());
                }
            }
            
            for key in keys_to_remove {
                states.remove(&key);
                removed_count += 1;
            }
        }

        // 清理数据库
        if self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                let db_removed = conn.execute(
                    "DELETE FROM typing_states WHERE last_updated < ?1",
                    params![cutoff_time],
                )?;
                removed_count += db_removed;
            }
        }

        if removed_count > 0 {
            debug!("Cleaned up {} expired typing states", removed_count);
        }

        Ok(removed_count)
    }

    /// 清理指定用户的所有输入状态
    pub fn clear_user_typing_states(&self, user_id: u64) -> crate::Result<usize> {
        let mut removed_count = 0;

        // 清理内存缓存
        {
            let mut states = self.typing_states.lock().unwrap();
            let mut keys_to_remove = Vec::new();
            
            for (key, state) in states.iter() {
                if state.user_id == user_id {
                    keys_to_remove.push(key.clone());
                }
            }
            
            for key in keys_to_remove {
                states.remove(&key);
                removed_count += 1;
            }
        }

        // 清理数据库
        if self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                let db_removed = conn.execute(
                    "DELETE FROM typing_states WHERE user_id = ?1",
                    params![user_id],
                )?;
                removed_count += db_removed;
            }
        }

        if removed_count > 0 {
            debug!("Cleared {} typing states for user {}", removed_count, user_id);
        }

        Ok(removed_count)
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> TypingManagerStats {
        let memory_count = {
            let states = self.typing_states.lock().unwrap();
            states.len()
        };

        let db_count = if self.config.enable_persistence {
            if let Some(ref conn) = self.conn {
                let conn = conn.lock().unwrap();
                conn.query_row(
                    "SELECT COUNT(*) FROM typing_states WHERE is_typing = 1",
                    [],
                    |row| row.get::<_, i32>(0),
                ).unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };

        TypingManagerStats {
            memory_states_count: memory_count,
            db_states_count: db_count as usize,
            timeout_seconds: self.config.typing_timeout,
            cleanup_interval_seconds: self.config.cleanup_interval,
            persistence_enabled: self.config.enable_persistence,
        }
    }
}

/// Typing Manager 统计信息
#[derive(Debug, Clone)]
pub struct TypingManagerStats {
    /// 内存中的状态数量
    pub memory_states_count: usize,
    /// 数据库中的状态数量
    pub db_states_count: usize,
    /// 超时时间（秒）
    pub timeout_seconds: u64,
    /// 清理间隔（秒）
    pub cleanup_interval_seconds: u64,
    /// 是否启用持久化
    pub persistence_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_manager() -> (TypingManager, Option<NamedTempFile>) {
        let config = TypingManagerConfig {
            typing_timeout: 5, // 5秒超时，便于测试
            cleanup_interval: 10,
            enable_persistence: false,
        };
        (TypingManager::new(config), None)
    }

    fn create_test_manager_with_db() -> (TypingManager, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let conn = Connection::open(&temp_file.path()).unwrap();
        let config = TypingManagerConfig {
            typing_timeout: 5,
            cleanup_interval: 10,
            enable_persistence: true,
        };
        let manager = TypingManager::with_database(conn, config).unwrap();
        (manager, temp_file)
    }

    #[tokio::test]
    async fn test_typing_state_management() {
        let (manager, _temp_file) = create_test_manager();

        // 设置用户开始输入
        let event = manager.update_typing_state(
            1001, 101, 1, true, Some(1)
        ).unwrap();

        assert_eq!(event.user_id, 1001);
        assert_eq!(event.channel_id, 101);
        assert!(event.is_typing);

        // 获取正在输入的用户
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 1);
        assert_eq!(typing_users[0].user_id, 1001);

        // 设置用户停止输入
        let event = manager.update_typing_state(
            1001, 101, 1, false, Some(1)
        ).unwrap();
        assert!(!event.is_typing);

        // 应该没有正在输入的用户
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 0);
    }

    #[tokio::test]
    async fn test_typing_state_timeout() {
        let (manager, _temp_file) = create_test_manager();

        // 设置用户开始输入
        manager.update_typing_state(
            1001, 101, 1, true, None
        ).unwrap();

        // 立即查询应该有用户
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 1);

        // 等待超时
        tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;

        // 查询应该没有用户（已超时）
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_expired_states() {
        let (manager, _temp_file) = create_test_manager();

        // 添加多个用户的输入状态
        manager.update_typing_state(1001, 101, 1, true, None).unwrap();
        manager.update_typing_state(1002, 101, 1, true, None).unwrap();
        manager.update_typing_state(1003, 102, 1, true, None).unwrap();

        // 等待超时
        tokio::time::sleep(tokio::time::Duration::from_secs(6)).await;

        // 清理过期状态
        let removed = manager.cleanup_expired_states().unwrap();
        assert_eq!(removed, 3);

        // 所有频道都应该没有正在输入的用户
        let typing_users1 = manager.get_typing_users(101, 1).unwrap();
        let typing_users2 = manager.get_typing_users(102, 1).unwrap();
        assert_eq!(typing_users1.len(), 0);
        assert_eq!(typing_users2.len(), 0);
    }

    #[tokio::test]
    async fn test_multiple_users_typing() {
        let (manager, _temp_file) = create_test_manager();

        // 多个用户同时输入
        manager.update_typing_state(1001, 101, 1, true, None).unwrap();
        manager.update_typing_state(1002, 101, 1, true, None).unwrap();
        manager.update_typing_state(1003, 101, 1, true, None).unwrap();

        // 应该有3个用户正在输入
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 3);

        // 一个用户停止输入
        manager.update_typing_state(1002, 101, 1, false, None).unwrap();

        // 应该剩下2个用户
        let typing_users = manager.get_typing_users(101, 1).unwrap();
        assert_eq!(typing_users.len(), 2);
    }

    #[tokio::test]
    async fn test_database_persistence() {
        let (manager, _temp_file) = create_test_manager_with_db();

        // 设置用户开始输入
        manager.update_typing_state(1001, 101, 1, true, None).unwrap();

        // 获取统计信息
        let stats = manager.get_stats();
        assert_eq!(stats.memory_states_count, 1);
        assert_eq!(stats.db_states_count, 1);
        assert!(stats.persistence_enabled);

        // 清理用户状态
        let removed = manager.clear_user_typing_states(1001).unwrap();
        assert_eq!(removed, 2); // 内存 + 数据库

        let stats = manager.get_stats();
        assert_eq!(stats.memory_states_count, 0);
        assert_eq!(stats.db_states_count, 0);
    }
} 