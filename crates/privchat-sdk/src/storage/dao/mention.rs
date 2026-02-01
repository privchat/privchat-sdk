//! @提及数据访问层 - 管理@提及记录和未读计数

use rusqlite::{Connection, params};
use crate::error::Result;

/// @提及记录
#[derive(Debug, Clone)]
pub struct Mention {
    pub id: i64,
    pub message_id: String,
    pub channel_id: String,
    pub channel_type: i32,
    pub mentioned_user_id: String,  // 被@的用户ID（当前用户）
    pub sender_id: String,          // 发送者ID
    pub is_mention_all: bool,       // 是否@全体成员
    pub created_at: i64,            // Unix 时间戳
    pub is_read: bool,              // 是否已读
}

/// @提及数据访问对象
pub struct MentionDao<'a> {
    conn: &'a Connection,
}

impl<'a> MentionDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 初始化表（如果不存在）
    pub fn initialize_table(&self) -> Result<()> {
        let sql = "CREATE TABLE IF NOT EXISTS mention (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            message_id INTEGER NOT NULL,
            channel_id INTEGER NOT NULL,
            channel_type INTEGER NOT NULL,
            mentioned_user_id INTEGER NOT NULL,
            sender_id INTEGER NOT NULL,
            is_mention_all INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            is_read INTEGER NOT NULL DEFAULT 0,
            UNIQUE(message_id, mentioned_user_id)
        )";
        
        self.conn.execute(sql, [])?;
        
        // 创建索引
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mention_channel_user 
             ON mention(channel_id, channel_type, mentioned_user_id, is_read)",
            [],
        )?;
        
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mention_message 
             ON mention(message_id)",
            [],
        )?;
        
        Ok(())
    }
    
    /// 记录@提及
    pub fn record_mention(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        mentioned_user_id: u64,
        sender_id: u64,
        is_mention_all: bool,
    ) -> Result<i64> {
        let sql = "INSERT OR IGNORE INTO mention (
            message_id, channel_id, channel_type, mentioned_user_id,
            sender_id, is_mention_all, created_at, is_read
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)";
        
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        
        self.conn.execute(sql, params![
            message_id,
            channel_id,
            channel_type,
            mentioned_user_id,
            sender_id,
            if is_mention_all { 1 } else { 0 },
            created_at,
        ])?;
        
        Ok(self.conn.last_insert_rowid())
    }
    
    /// 获取会话的未读提及计数
    pub fn get_unread_mention_count(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32> {
        let sql = "SELECT COUNT(*) FROM mention 
                   WHERE channel_id = ?1 AND channel_type = ?2 
                   AND mentioned_user_id = ?3 AND is_read = 0";
        
        let count: i32 = self.conn.query_row(sql, params![channel_id, channel_type, user_id], |row| {
            Ok(row.get(0)?)
        })?;
        
        Ok(count)
    }
    
    /// 获取会话的未读提及消息ID列表
    pub fn get_unread_mention_message_ids(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: Option<i32>,
    ) -> Result<Vec<String>> {
        let limit_clause = if let Some(limit) = limit {
            format!("LIMIT {}", limit)
        } else {
            String::new()
        };
        
        let sql = format!(
            "SELECT message_id FROM mention 
             WHERE channel_id = ?1 AND channel_type = ?2 
             AND mentioned_user_id = ?3 AND is_read = 0 
             ORDER BY created_at DESC {}",
            limit_clause
        );
        
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![channel_id, channel_type, user_id], |row| {
            Ok(row.get::<_, String>(0)?)
        })?;
        
        let mut message_ids = Vec::new();
        for row in rows {
            message_ids.push(row?);
        }
        
        Ok(message_ids)
    }
    
    /// 标记提及为已读
    /// 
    /// 注意：消息的已读状态由消息表的 `status` 字段表示（MessageStatus::Read = 4）
    /// 不需要额外的 `has_mention_read` 字段
    pub fn mark_mention_as_read(
        &self,
        message_id: u64,
        user_id: u64,
    ) -> Result<()> {
        let sql = "UPDATE mention SET is_read = 1 
                   WHERE message_id = ?1 AND mentioned_user_id = ?2";
        
        self.conn.execute(sql, params![message_id, user_id])?;
        Ok(())
    }
    
    /// 标记会话的所有提及为已读
    pub fn mark_all_mentions_as_read(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<()> {
        let sql = "UPDATE mention SET is_read = 1 
                   WHERE channel_id = ?1 AND channel_type = ?2 
                   AND mentioned_user_id = ?3 AND is_read = 0";
        
        self.conn.execute(sql, params![channel_id, channel_type, user_id])?;
        Ok(())
    }
    
    /// 检查消息是否@了指定用户
    pub fn is_user_mentioned(
        &self,
        message_id: u64,
        user_id: u64,
    ) -> Result<bool> {
        let sql = "SELECT COUNT(*) FROM mention 
                   WHERE message_id = ?1 AND mentioned_user_id = ?2";
        
        let count: i32 = self.conn.query_row(sql, params![message_id, user_id], |row| {
            Ok(row.get(0)?)
        })?;
        
        Ok(count > 0)
    }
    
    /// 获取所有会话的未读提及计数（用于会话列表显示）
    pub fn get_all_unread_mention_counts(
        &self,
        user_id: u64,
    ) -> Result<Vec<(String, i32, i32)>> {
        // 返回 (channel_id, channel_type, unread_count)
        let sql = "SELECT channel_id, channel_type, COUNT(*) as unread_count 
                   FROM mention 
                   WHERE mentioned_user_id = ?1 AND is_read = 0 
                   GROUP BY channel_id, channel_type";
        
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, i32>(2)?,
            ))
        })?;
        
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row?);
        }
        
        Ok(counts)
    }
}
