use crate::PrivchatSDKError;
use rusqlite::{params, Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

/// 表情反馈事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionEvent {
    /// 消息ID
    pub message_id: String,
    /// 频道ID
    pub channel_id: String,
    /// 频道类型
    pub channel_type: i32,
    /// 用户ID
    pub user_id: String,
    /// 表情符号
    pub emoji: String,
    /// 操作类型（添加/移除）
    pub action: ReactionAction,
    /// 事件时间戳
    pub timestamp: u64,
}

/// 表情反馈操作类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ReactionAction {
    /// 添加表情
    Add,
    /// 移除表情
    Remove,
}

/// 消息表情统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReactionSummary {
    /// 消息ID
    pub message_id: String,
    /// 表情统计：emoji -> 用户ID列表
    pub reactions: HashMap<String, Vec<String>>,
    /// 总反馈数
    pub total_count: usize,
}

/// 单个表情的详细信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionDetail {
    /// 表情符号
    pub emoji: String,
    /// 反馈用户列表
    pub user_ids: Vec<String>,
    /// 反馈数量
    pub count: usize,
    /// 最后更新时间
    pub last_updated: u64,
}

/// 用户表情反馈记录
#[derive(Debug, Clone)]
pub struct UserReaction {
    /// 用户ID
    pub user_id: String,
    /// 消息ID
    pub message_id: String,
    /// 频道ID
    pub channel_id: String,
    /// 频道类型
    pub channel_type: i32,
    /// 表情符号
    pub emoji: String,
    /// 创建时间
    pub created_at: u64,
}

/// Reaction Manager 配置
#[derive(Debug, Clone)]
pub struct ReactionManagerConfig {
    /// 是否允许用户对同一消息添加多个不同表情
    pub allow_multiple_emojis: bool,
    /// 单个消息最大表情种类数
    pub max_emoji_types_per_message: usize,
    /// 单个表情最大用户数
    pub max_users_per_emoji: usize,
    /// 是否启用表情去重（同一用户同一表情只能添加一次）
    pub enable_deduplication: bool,
}

impl Default for ReactionManagerConfig {
    fn default() -> Self {
        Self {
            allow_multiple_emojis: true,
            max_emoji_types_per_message: 50,
            max_users_per_emoji: 1000,
            enable_deduplication: true,
        }
    }
}

/// Reaction 管理器
pub struct ReactionManager {
    /// 数据库连接
    conn: Arc<Mutex<Connection>>,
    /// 配置
    config: ReactionManagerConfig,
}

impl ReactionManager {
    /// 创建新的 Reaction Manager
    pub fn new(conn: Connection, config: ReactionManagerConfig) -> crate::Result<Self> {
        let manager = Self {
            conn: Arc::new(Mutex::new(conn)),
            config,
        };

        manager.initialize_tables()?;
        Ok(manager)
    }

    /// 初始化数据库表
    fn initialize_tables(&self) -> crate::Result<()> {
        let conn = self.conn.lock().unwrap();

        // 创建表情反馈表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS message_reactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                user_id TEXT NOT NULL,
                emoji TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                UNIQUE(message_id, user_id, emoji)
            )",
            [],
        )?;

        // 创建索引
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reactions_message 
             ON message_reactions(message_id)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reactions_user 
             ON message_reactions(user_id)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reactions_emoji 
             ON message_reactions(emoji)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reactions_channel 
             ON message_reactions(channel_id, channel_type)",
            [],
        )?;

        info!("Reaction manager database tables initialized");
        Ok(())
    }

    /// 添加表情反馈
    pub fn add_reaction(
        &self,
        message_id: &str,
        channel_id: &str,
        channel_type: i32,
        user_id: &str,
        emoji: &str,
    ) -> crate::Result<ReactionEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();

        // 检查是否启用去重
        if self.config.enable_deduplication {
            // 检查用户是否已经对这个消息添加了这个表情
            let exists: bool = conn.query_row(
                "SELECT 1 FROM message_reactions 
                 WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
                params![message_id, user_id, emoji],
                |_| Ok(true),
            ).unwrap_or(false);

            if exists {
                return Err(PrivchatSDKError::InvalidOperation(
                    format!("User {} already reacted with {} to message {}", 
                           user_id, emoji, message_id)
                ));
            }
        }

        // 检查是否允许多个表情
        if !self.config.allow_multiple_emojis {
            // 检查用户是否已经对这个消息添加了其他表情
            let existing_count: i32 = conn.query_row(
                "SELECT COUNT(*) FROM message_reactions 
                 WHERE message_id = ?1 AND user_id = ?2",
                params![message_id, user_id],
                |row| row.get(0),
            ).unwrap_or(0);

            if existing_count > 0 {
                return Err(PrivchatSDKError::InvalidOperation(
                    format!("User {} already reacted to message {}", user_id, message_id)
                ));
            }
        }

        // 检查消息的表情种类数限制
        let emoji_types_count: i32 = conn.query_row(
            "SELECT COUNT(DISTINCT emoji) FROM message_reactions WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        ).unwrap_or(0);

        if emoji_types_count >= self.config.max_emoji_types_per_message as i32 {
            // 检查是否是已存在的表情类型
            let emoji_exists: bool = conn.query_row(
                "SELECT 1 FROM message_reactions WHERE message_id = ?1 AND emoji = ?2",
                params![message_id, emoji],
                |_| Ok(true),
            ).unwrap_or(false);

            if !emoji_exists {
                return Err(PrivchatSDKError::InvalidOperation(
                    format!("Message {} has reached maximum emoji types limit", message_id)
                ));
            }
        }

        // 检查单个表情的用户数限制
        let emoji_users_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM message_reactions 
             WHERE message_id = ?1 AND emoji = ?2",
            params![message_id, emoji],
            |row| row.get(0),
        ).unwrap_or(0);

        if emoji_users_count >= self.config.max_users_per_emoji as i32 {
            return Err(PrivchatSDKError::InvalidOperation(
                format!("Emoji {} on message {} has reached maximum users limit", 
                       emoji, message_id)
            ));
        }

        // 插入表情反馈记录
        conn.execute(
            "INSERT INTO message_reactions 
             (message_id, channel_id, channel_type, user_id, emoji, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![message_id, channel_id, channel_type, user_id, emoji, now],
        )?;

        debug!(
            "Added reaction {} by user {} to message {}",
            emoji, user_id, message_id
        );

        Ok(ReactionEvent {
            message_id: message_id.to_string(),
            channel_id: channel_id.to_string(),
            channel_type,
            user_id: user_id.to_string(),
            emoji: emoji.to_string(),
            action: ReactionAction::Add,
            timestamp: now,
        })
    }

    /// 移除表情反馈
    pub fn remove_reaction(
        &self,
        message_id: &str,
        user_id: &str,
        emoji: &str,
    ) -> crate::Result<ReactionEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();

        // 获取要删除的记录信息
        let (channel_id, channel_type): (String, i32) = conn.query_row(
            "SELECT channel_id, channel_type FROM message_reactions 
             WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
            params![message_id, user_id, emoji],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).map_err(|_| PrivchatSDKError::InvalidOperation(
            format!("Reaction {} by user {} on message {} not found", 
                   emoji, user_id, message_id)
        ))?;

        // 删除表情反馈记录
        let affected_rows = conn.execute(
            "DELETE FROM message_reactions 
             WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
            params![message_id, user_id, emoji],
        )?;

        if affected_rows == 0 {
            return Err(PrivchatSDKError::InvalidOperation(
                format!("Reaction {} by user {} on message {} not found", 
                       emoji, user_id, message_id)
            ));
        }

        debug!(
            "Removed reaction {} by user {} from message {}",
            emoji, user_id, message_id
        );

        Ok(ReactionEvent {
            message_id: message_id.to_string(),
            channel_id,
            channel_type,
            user_id: user_id.to_string(),
            emoji: emoji.to_string(),
            action: ReactionAction::Remove,
            timestamp: now,
        })
    }

    /// 获取消息的所有表情反馈统计
    pub fn get_message_reactions(&self, message_id: &str) -> crate::Result<MessageReactionSummary> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT emoji, user_id FROM message_reactions 
             WHERE message_id = ?1 ORDER BY emoji, created_at"
        )?;

        let rows = stmt.query_map(params![message_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut reactions: HashMap<String, Vec<String>> = HashMap::new();
        let mut total_count = 0;

        for row in rows {
            let (emoji, user_id) = row?;
            reactions.entry(emoji).or_insert_with(Vec::new).push(user_id);
            total_count += 1;
        }

        Ok(MessageReactionSummary {
            message_id: message_id.to_string(),
            reactions,
            total_count,
        })
    }

    /// 获取特定表情的详细信息
    pub fn get_emoji_details(
        &self,
        message_id: &str,
        emoji: &str,
    ) -> crate::Result<Option<ReactionDetail>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT user_id, created_at FROM message_reactions 
             WHERE message_id = ?1 AND emoji = ?2 ORDER BY created_at"
        )?;

        let rows = stmt.query_map(params![message_id, emoji], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
        })?;

        let mut user_ids = Vec::new();
        let mut last_updated = 0u64;

        for row in rows {
            let (user_id, created_at) = row?;
            user_ids.push(user_id);
            last_updated = last_updated.max(created_at);
        }

        if user_ids.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ReactionDetail {
                emoji: emoji.to_string(),
                count: user_ids.len(),
                user_ids,
                last_updated,
            }))
        }
    }

    /// 获取用户的所有表情反馈
    pub fn get_user_reactions(&self, user_id: &str) -> crate::Result<Vec<UserReaction>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT message_id, channel_id, channel_type, emoji, created_at 
             FROM message_reactions WHERE user_id = ?1 ORDER BY created_at DESC"
        )?;

        let rows = stmt.query_map(params![user_id], |row| {
            Ok(UserReaction {
                user_id: user_id.to_string(),
                message_id: row.get(0)?,
                channel_id: row.get(1)?,
                channel_type: row.get(2)?,
                emoji: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;

        let mut reactions = Vec::new();
        for row in rows {
            reactions.push(row?);
        }

        Ok(reactions)
    }

    /// 检查用户是否对消息添加了特定表情
    pub fn has_user_reacted(
        &self,
        message_id: &str,
        user_id: &str,
        emoji: &str,
    ) -> crate::Result<bool> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn.query_row(
            "SELECT 1 FROM message_reactions 
             WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
            params![message_id, user_id, emoji],
            |_| Ok(true),
        ).unwrap_or(false);

        Ok(exists)
    }

    /// 获取频道中的热门表情
    pub fn get_popular_emojis(
        &self,
        channel_id: &str,
        channel_type: i32,
        limit: usize,
    ) -> crate::Result<Vec<(String, usize)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT emoji, COUNT(*) as count FROM message_reactions 
             WHERE channel_id = ?1 AND channel_type = ?2 
             GROUP BY emoji ORDER BY count DESC LIMIT ?3"
        )?;

        let rows = stmt.query_map(params![channel_id, channel_type, limit], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, usize>(1)?))
        })?;

        let mut emojis = Vec::new();
        for row in rows {
            emojis.push(row?);
        }

        Ok(emojis)
    }

    /// 清理指定消息的所有表情反馈
    pub fn clear_message_reactions(&self, message_id: &str) -> crate::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let affected_rows = conn.execute(
            "DELETE FROM message_reactions WHERE message_id = ?1",
            params![message_id],
        )?;

        debug!("Cleared {} reactions for message {}", affected_rows, message_id);
        Ok(affected_rows)
    }

    /// 清理指定用户的所有表情反馈
    pub fn clear_user_reactions(&self, user_id: &str) -> crate::Result<usize> {
        let conn = self.conn.lock().unwrap();
        let affected_rows = conn.execute(
            "DELETE FROM message_reactions WHERE user_id = ?1",
            params![user_id],
        )?;

        debug!("Cleared {} reactions for user {}", affected_rows, user_id);
        Ok(affected_rows)
    }

    /// 获取统计信息
    pub fn get_stats(&self) -> crate::Result<ReactionManagerStats> {
        let conn = self.conn.lock().unwrap();

        let total_reactions: i32 = conn.query_row(
            "SELECT COUNT(*) FROM message_reactions",
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        let unique_messages: i32 = conn.query_row(
            "SELECT COUNT(DISTINCT message_id) FROM message_reactions",
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        let unique_users: i32 = conn.query_row(
            "SELECT COUNT(DISTINCT user_id) FROM message_reactions",
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        let unique_emojis: i32 = conn.query_row(
            "SELECT COUNT(DISTINCT emoji) FROM message_reactions",
            [],
            |row| row.get(0),
        ).unwrap_or(0);

        Ok(ReactionManagerStats {
            total_reactions: total_reactions as usize,
            unique_messages: unique_messages as usize,
            unique_users: unique_users as usize,
            unique_emojis: unique_emojis as usize,
            max_emoji_types_per_message: self.config.max_emoji_types_per_message,
            max_users_per_emoji: self.config.max_users_per_emoji,
            allow_multiple_emojis: self.config.allow_multiple_emojis,
            enable_deduplication: self.config.enable_deduplication,
        })
    }
}

/// Reaction Manager 统计信息
#[derive(Debug, Clone)]
pub struct ReactionManagerStats {
    /// 总表情反馈数
    pub total_reactions: usize,
    /// 有表情反馈的消息数
    pub unique_messages: usize,
    /// 参与表情反馈的用户数
    pub unique_users: usize,
    /// 使用的表情种类数
    pub unique_emojis: usize,
    /// 单个消息最大表情种类数
    pub max_emoji_types_per_message: usize,
    /// 单个表情最大用户数
    pub max_users_per_emoji: usize,
    /// 是否允许多个表情
    pub allow_multiple_emojis: bool,
    /// 是否启用去重
    pub enable_deduplication: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn create_test_manager() -> (ReactionManager, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let conn = Connection::open(&temp_file.path()).unwrap();
        let config = ReactionManagerConfig::default();
        let manager = ReactionManager::new(conn, config).unwrap();
        (manager, temp_file)
    }

    #[tokio::test]
    async fn test_add_and_remove_reaction() {
        let (manager, _temp_file) = create_test_manager();

        // 添加表情反馈
        let event = manager.add_reaction(
            "msg1", "channel1", 1, "user1", "👍"
        ).unwrap();

        assert_eq!(event.action, ReactionAction::Add);
        assert_eq!(event.emoji, "👍");
        assert_eq!(event.user_id, "user1");

        // 检查表情反馈是否存在
        let has_reacted = manager.has_user_reacted("msg1", "user1", "👍").unwrap();
        assert!(has_reacted);

        // 获取消息的表情统计
        let summary = manager.get_message_reactions("msg1").unwrap();
        assert_eq!(summary.total_count, 1);
        assert_eq!(summary.reactions.get("👍").unwrap().len(), 1);

        // 移除表情反馈
        let event = manager.remove_reaction("msg1", "user1", "👍").unwrap();
        assert_eq!(event.action, ReactionAction::Remove);

        // 检查表情反馈是否已移除
        let has_reacted = manager.has_user_reacted("msg1", "user1", "👍").unwrap();
        assert!(!has_reacted);

        let summary = manager.get_message_reactions("msg1").unwrap();
        assert_eq!(summary.total_count, 0);
    }

    #[tokio::test]
    async fn test_multiple_users_same_emoji() {
        let (manager, _temp_file) = create_test_manager();

        // 多个用户添加相同表情
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user2", "👍").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user3", "👍").unwrap();

        // 获取表情详情
        let detail = manager.get_emoji_details("msg1", "👍").unwrap().unwrap();
        assert_eq!(detail.count, 3);
        assert_eq!(detail.user_ids.len(), 3);
        assert!(detail.user_ids.contains(&"user1".to_string()));
        assert!(detail.user_ids.contains(&"user2".to_string()));
        assert!(detail.user_ids.contains(&"user3".to_string()));
    }

    #[tokio::test]
    async fn test_multiple_emojis_same_message() {
        let (manager, _temp_file) = create_test_manager();

        // 同一消息添加不同表情
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user1", "❤️").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user2", "😂").unwrap();

        let summary = manager.get_message_reactions("msg1").unwrap();
        assert_eq!(summary.total_count, 3);
        assert_eq!(summary.reactions.len(), 3);
        assert!(summary.reactions.contains_key("👍"));
        assert!(summary.reactions.contains_key("❤️"));
        assert!(summary.reactions.contains_key("😂"));
    }

    #[tokio::test]
    async fn test_duplicate_reaction_prevention() {
        let (manager, _temp_file) = create_test_manager();

        // 添加表情反馈
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();

        // 尝试添加重复的表情反馈
        let result = manager.add_reaction("msg1", "channel1", 1, "user1", "👍");
        assert!(result.is_err());

        // 确认只有一个反馈
        let summary = manager.get_message_reactions("msg1").unwrap();
        assert_eq!(summary.total_count, 1);
    }

    #[tokio::test]
    async fn test_popular_emojis() {
        let (manager, _temp_file) = create_test_manager();

        // 添加多个表情反馈
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg2", "channel1", 1, "user2", "👍").unwrap();
        manager.add_reaction("msg3", "channel1", 1, "user3", "👍").unwrap();
        manager.add_reaction("msg4", "channel1", 1, "user1", "❤️").unwrap();
        manager.add_reaction("msg5", "channel1", 1, "user2", "❤️").unwrap();
        manager.add_reaction("msg6", "channel1", 1, "user3", "😂").unwrap();

        // 获取热门表情
        let popular = manager.get_popular_emojis("channel1", 1, 3).unwrap();
        assert_eq!(popular.len(), 3);
        assert_eq!(popular[0], ("👍".to_string(), 3)); // 最受欢迎
        assert_eq!(popular[1], ("❤️".to_string(), 2));
        assert_eq!(popular[2], ("😂".to_string(), 1));
    }

    #[tokio::test]
    async fn test_user_reactions() {
        let (manager, _temp_file) = create_test_manager();

        // 用户添加多个表情反馈
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg2", "channel1", 1, "user1", "❤️").unwrap();
        manager.add_reaction("msg3", "channel2", 1, "user1", "😂").unwrap();

        // 获取用户的所有表情反馈
        let user_reactions = manager.get_user_reactions("user1").unwrap();
        assert_eq!(user_reactions.len(), 3);

        // 验证反馈内容
        let emojis: Vec<&str> = user_reactions.iter().map(|r| r.emoji.as_str()).collect();
        assert!(emojis.contains(&"👍"));
        assert!(emojis.contains(&"❤️"));
        assert!(emojis.contains(&"😂"));
    }

    #[tokio::test]
    async fn test_clear_operations() {
        let (manager, _temp_file) = create_test_manager();

        // 添加一些表情反馈
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user2", "❤️").unwrap();
        manager.add_reaction("msg2", "channel1", 1, "user1", "😂").unwrap();

        // 清理消息的所有表情反馈
        let cleared = manager.clear_message_reactions("msg1").unwrap();
        assert_eq!(cleared, 2);

        let summary = manager.get_message_reactions("msg1").unwrap();
        assert_eq!(summary.total_count, 0);

        // 清理用户的所有表情反馈
        let cleared = manager.clear_user_reactions("user1").unwrap();
        assert_eq!(cleared, 1); // msg2 上的 😂

        let user_reactions = manager.get_user_reactions("user1").unwrap();
        assert_eq!(user_reactions.len(), 0);
    }

    #[tokio::test]
    async fn test_stats() {
        let (manager, _temp_file) = create_test_manager();

        // 添加一些表情反馈
        manager.add_reaction("msg1", "channel1", 1, "user1", "👍").unwrap();
        manager.add_reaction("msg1", "channel1", 1, "user2", "👍").unwrap();
        manager.add_reaction("msg2", "channel1", 1, "user1", "❤️").unwrap();

        let stats = manager.get_stats().unwrap();
        assert_eq!(stats.total_reactions, 3);
        assert_eq!(stats.unique_messages, 2);
        assert_eq!(stats.unique_users, 2);
        assert_eq!(stats.unique_emojis, 2);
    }
} 