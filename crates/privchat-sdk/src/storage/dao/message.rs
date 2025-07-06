//! 消息数据访问层 - 封装所有消息相关的数据库操作
//! 
//! 功能包括：
//! - 消息的增删改查
//! - 消息搜索和分页
//! - 消息状态管理
//! - 批量操作
//! - 消息过期处理

use rusqlite::{Connection, params, Row};
use chrono::Utc;
use crate::error::{Result, PrivchatSDKError};
use crate::storage::entities::{Message, MessageQuery, PageResult};

/// 消息数据访问对象
pub struct MessageDao<'a> {
    conn: &'a Connection,
}

impl<'a> MessageDao<'a> {
    /// 创建新的 MessageDao 实例
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 插入新消息
    pub fn insert(&self, message: &Message) -> Result<i64> {
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let sql = "INSERT INTO message (
            message_id, message_seq, channel_id, channel_type, timestamp, from_uid, 
            type, content, status, voice_status, created_at, updated_at, 
            searchable_word, client_msg_no, is_deleted, setting, order_seq, extra,
            flame, flame_second, viewed, viewed_at, topic_id, expire_time, expire_timestamp
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22, ?23, ?24, ?25
        )";
        
        let _result = self.conn.execute(sql, params![
            message.message_id,
            message.message_seq,
            message.channel_id,
            message.channel_type,
            message.timestamp,
            message.from_uid,
            message.message_type,
            message.content,
            message.status,
            message.voice_status,
            if message.created_at.is_empty() { &now } else { &message.created_at },
            if message.updated_at.is_empty() { &now } else { &message.updated_at },
            message.searchable_word,
            message.client_msg_no,
            message.is_deleted,
            message.setting,
            message.order_seq,
            message.extra,
            message.flame,
            message.flame_second,
            message.viewed,
            message.viewed_at,
            message.topic_id,
            message.expire_time,
            message.expire_timestamp,
        ])?;
        
        Ok(self.conn.last_insert_rowid())
    }
    
    /// 批量插入消息
    pub fn batch_insert(&self, messages: &[Message]) -> Result<Vec<i64>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut row_ids = Vec::new();
        
        for message in messages {
            let row_id = self.insert(message)?;
            row_ids.push(row_id);
        }
        
        tx.commit()?;
        Ok(row_ids)
    }
    
    /// 根据消息ID获取消息
    pub fn get_by_message_id(&self, message_id: &str) -> Result<Option<Message>> {
        let sql = "SELECT * FROM message WHERE message_id = ?1 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![message_id], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(e)) => Err(PrivchatSDKError::Database(format!("查询消息失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 根据客户端消息号获取消息
    pub fn get_by_client_msg_no(&self, client_msg_no: &str) -> Result<Option<Message>> {
        let sql = "SELECT * FROM message WHERE client_msg_no = ?1 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![client_msg_no], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(e)) => Err(PrivchatSDKError::Database(format!("查询消息失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 查询消息列表
    pub fn query(&self, query: &MessageQuery) -> Result<PageResult<Message>> {
        let mut sql = "SELECT * FROM message WHERE 1=1".to_string();
        let mut params: Vec<String> = Vec::new();
        
        // 构建查询条件
        if let Some(channel_id) = &query.channel_id {
            sql.push_str(" AND channel_id = ?");
            params.push(channel_id.clone());
        }
        
        if let Some(channel_type) = query.channel_type {
            sql.push_str(" AND channel_type = ?");
            params.push(channel_type.to_string());
        }
        
        if let Some(from_uid) = &query.from_uid {
            sql.push_str(" AND from_uid = ?");
            params.push(from_uid.clone());
        }
        
        if let Some(message_type) = query.message_type {
            sql.push_str(" AND type = ?");
            params.push(message_type.to_string());
        }
        
        if let Some(start_time) = query.start_time {
            sql.push_str(" AND timestamp >= ?");
            params.push(start_time.to_string());
        }
        
        if let Some(end_time) = query.end_time {
            sql.push_str(" AND timestamp <= ?");
            params.push(end_time.to_string());
        }
        
        if let Some(is_deleted) = query.is_deleted {
            sql.push_str(" AND is_deleted = ?");
            params.push(is_deleted.to_string());
        } else {
            sql.push_str(" AND is_deleted = 0");
        }
        
        // 排序
        if let Some(order_by) = &query.order_by {
            sql.push_str(&format!(" ORDER BY {}", order_by));
        } else {
            sql.push_str(" ORDER BY timestamp DESC");
        }
        
        // 分页
        let limit = query.limit.unwrap_or(20);
        let offset = query.offset.unwrap_or(0);
        sql.push_str(&format!(" LIMIT {} OFFSET {}", limit, offset));
        
        // 查询数据
        let mut stmt = self.conn.prepare(&sql)?;
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(&param_refs[..], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        
        // 查询总数
        let count_sql = sql.replace("SELECT *", "SELECT COUNT(*)").split(" ORDER BY").next().unwrap().to_string();
        let total: i64 = self.conn.query_row(&count_sql, &param_refs[..], |row| row.get(0))?;
        
        Ok(PageResult::new(messages, total, offset / limit + 1, limit))
    }
    
    /// 搜索消息
    pub fn search(&self, channel_id: &str, channel_type: i32, keyword: &str, limit: Option<u32>) -> Result<Vec<Message>> {
        let limit = limit.unwrap_or(20);
        
        let sql = "SELECT m.* FROM message m 
                  WHERE m.channel_id = ?1 AND m.channel_type = ?2 
                  AND m.is_deleted = 0 
                  AND (m.content LIKE ?3 OR m.searchable_word LIKE ?3)
                  ORDER BY m.timestamp DESC 
                  LIMIT ?4";
        
        let keyword_pattern = format!("%{}%", keyword);
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![channel_id, channel_type, keyword_pattern, limit], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        
        Ok(messages)
    }
    
    /// 更新消息状态
    pub fn update_status(&self, message_id: &str, status: crate::storage::entities::MessageStatus) -> Result<()> {
        let sql = "UPDATE message SET status = ?1, updated_at = ?2 WHERE message_id = ?3";
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let affected = self.conn.execute(sql, params![status as i32, now, message_id])?;
        
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        
        Ok(())
    }
    
    /// 撤回消息
    pub fn revoke(&self, message_id: &str, revoker: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        
        // 更新消息状态
        let sql = "UPDATE message SET is_deleted = 1, updated_at = ?1 WHERE message_id = ?2";
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        self.conn.execute(sql, params![now, message_id])?;
        
        // 更新消息扩展信息
        let extra_sql = "INSERT OR REPLACE INTO message_extra (
            message_id, revoke, revoker, extra_version
        ) VALUES (?1, 1, ?2, ?3)";
        
        self.conn.execute(extra_sql, params![message_id, revoker, Utc::now().timestamp()])?;
        
        tx.commit()?;
        Ok(())
    }
    
    /// 编辑消息
    pub fn edit(&self, message_id: &str, new_content: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        
        // 更新消息内容
        let sql = "UPDATE message SET content = ?1, updated_at = ?2 WHERE message_id = ?3";
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        self.conn.execute(sql, params![new_content, now, message_id])?;
        
        // 更新消息扩展信息
        let extra_sql = "INSERT OR REPLACE INTO message_extra (
            message_id, content_edit, edited_at, extra_version
        ) VALUES (?1, ?2, ?3, ?4)";
        
        self.conn.execute(extra_sql, params![
            message_id, 
            new_content, 
            Utc::now().timestamp(),
            Utc::now().timestamp()
        ])?;
        
        tx.commit()?;
        Ok(())
    }
    
    /// 标记消息为已读
    pub fn mark_as_read(&self, message_id: &str) -> Result<()> {
        let sql = "UPDATE message SET viewed = 1, viewed_at = ?1, updated_at = ?2 WHERE message_id = ?3";
        let now = Utc::now().timestamp();  // Returns i64
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        self.conn.execute(sql, params![now, now_str, message_id])?;
        Ok(())
    }
    
    /// 删除过期消息
    pub fn delete_expired(&self) -> Result<u32> {
        let sql = "UPDATE message SET is_deleted = 1, updated_at = ?1 
                  WHERE expire_timestamp > 0 AND expire_timestamp < ?2 AND is_deleted = 0";
        let now = Utc::now().timestamp();  // Returns i64
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let affected = self.conn.execute(sql, params![now_str, now])?;
        Ok(affected as u32)
    }
    
    /// 获取频道中的消息数量
    pub fn count_by_channel(&self, channel_id: &str, channel_type: i32) -> Result<i64> {
        let sql = "SELECT COUNT(*) FROM message WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0";
        let count: i64 = self.conn.query_row(sql, params![channel_id, channel_type], |row| row.get(0))?;
        Ok(count)
    }
    
    /// 获取用户发送的消息数量
    pub fn count_by_user(&self, from_uid: &str) -> Result<i64> {
        let sql = "SELECT COUNT(*) FROM message WHERE from_uid = ?1 AND is_deleted = 0";
        let count: i64 = self.conn.query_row(sql, params![from_uid], |row| row.get(0))?;
        Ok(count)
    }
    
    /// 软删除消息
    pub fn soft_delete(&self, message_id: &str) -> Result<()> {
        let sql = "UPDATE message SET is_deleted = 1, updated_at = ?1 WHERE message_id = ?2";
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let affected = self.conn.execute(sql, params![now, message_id])?;
        
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        
        Ok(())
    }
    
    /// 恢复删除的消息
    pub fn restore(&self, message_id: &str) -> Result<()> {
        let sql = "UPDATE message SET is_deleted = 0, updated_at = ?1 WHERE message_id = ?2";
        let now = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let affected = self.conn.execute(sql, params![now, message_id])?;
        
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        
        Ok(())
    }
    
    /// 将数据库行转换为 Message 实体
    fn row_to_message(&self, row: &Row) -> rusqlite::Result<Message> {
        Ok(Message {
            client_seq: row.get("client_seq")?,
            message_id: row.get("message_id")?,
            message_seq: row.get("message_seq")?,
            channel_id: row.get("channel_id")?,
            channel_type: row.get("channel_type")?,
            timestamp: row.get("timestamp")?,
            from_uid: row.get("from_uid")?,
            message_type: row.get("type")?,
            content: row.get("content")?,
            status: row.get("status")?,
            voice_status: row.get("voice_status")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            searchable_word: row.get("searchable_word")?,
            client_msg_no: row.get("client_msg_no")?,
            is_deleted: row.get("is_deleted")?,
            setting: row.get("setting")?,
            order_seq: row.get("order_seq")?,
            extra: row.get("extra")?,
            flame: row.get("flame")?,
            flame_second: row.get("flame_second")?,
            viewed: row.get("viewed")?,
            viewed_at: row.get("viewed_at")?,
            topic_id: row.get("topic_id")?,
            expire_time: row.get("expire_time")?,
            expire_timestamp: row.get("expire_timestamp")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;
    use chrono::Utc;
    
    fn create_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        
        // 创建消息表结构
        conn.execute(
            "CREATE TABLE message (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                client_seq INTEGER,
                message_id TEXT UNIQUE,
                message_seq INTEGER NOT NULL,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                timestamp INTEGER,
                from_uid TEXT NOT NULL,
                type INTEGER NOT NULL,
                content TEXT NOT NULL,
                status INTEGER NOT NULL DEFAULT 0,
                voice_status INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                searchable_word TEXT NOT NULL DEFAULT '',
                client_msg_no TEXT NOT NULL DEFAULT '',
                is_deleted INTEGER NOT NULL DEFAULT 0,
                setting INTEGER NOT NULL DEFAULT 0,
                order_seq INTEGER NOT NULL DEFAULT 0,
                extra TEXT NOT NULL DEFAULT '{}',
                flame INTEGER NOT NULL DEFAULT 0,
                flame_second INTEGER NOT NULL DEFAULT 0,
                viewed INTEGER NOT NULL DEFAULT 0,
                viewed_at INTEGER NOT NULL DEFAULT 0,
                topic_id TEXT NOT NULL DEFAULT '',
                expire_time INTEGER,
                expire_timestamp INTEGER
            )",
            [],
        ).unwrap();
        
        conn
    }
    
    #[test]
    fn test_insert_and_get_message() {
        let conn = create_test_db();
        let dao = MessageDao::new(&conn);
        
        let message = Message {
            client_seq: None,
            message_id: Some("test_msg_001".to_string()),
            message_seq: 1,
            channel_id: "test_channel".to_string(),
            channel_type: 1,
            timestamp: Some(Utc::now().timestamp_millis()),
            from_uid: "user_001".to_string(),
            message_type: 1,
            content: "Hello, world!".to_string(),
            status: 1,
            voice_status: 0,
            created_at: "".to_string(),
            updated_at: "".to_string(),
            searchable_word: "Hello world".to_string(),
            client_msg_no: "client_001".to_string(),
            is_deleted: 0,
            setting: 0,
            order_seq: 1,
            extra: "{}".to_string(),
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: "".to_string(),
            expire_time: None,
            expire_timestamp: None,
        };
        
        // 插入消息
        let row_id = dao.insert(&message).unwrap();
        assert!(row_id > 0);
        
        // 获取消息
        let retrieved = dao.get_by_message_id("test_msg_001").unwrap();
        assert!(retrieved.is_some());
        
        let retrieved_msg = retrieved.unwrap();
        assert_eq!(retrieved_msg.message_id, Some("test_msg_001".to_string()));
        assert_eq!(retrieved_msg.content, "Hello, world!");
    }
    
    #[test]
    fn test_search_messages() {
        let conn = create_test_db();
        let dao = MessageDao::new(&conn);
        
        // 插入测试消息
        let messages = vec![
            Message {
                client_seq: None,
                message_id: Some("msg_001".to_string()),
                message_seq: 1,
                channel_id: "test_channel".to_string(),
                channel_type: 1,
                timestamp: Some(Utc::now().timestamp_millis()),
                from_uid: "user_001".to_string(),
                message_type: 1,
                content: "Hello Rust".to_string(),
                status: 1,
                voice_status: 0,
                created_at: "".to_string(),
                updated_at: "".to_string(),
                searchable_word: "Hello Rust".to_string(),
                client_msg_no: "client_001".to_string(),
                is_deleted: 0,
                setting: 0,
                order_seq: 1,
                extra: "{}".to_string(),
                flame: 0,
                flame_second: 0,
                viewed: 0,
                viewed_at: 0,
                topic_id: "".to_string(),
                expire_time: None,
                expire_timestamp: None,
            },
            Message {
                client_seq: None,
                message_id: Some("msg_002".to_string()),
                message_seq: 2,
                channel_id: "test_channel".to_string(),
                channel_type: 1,
                timestamp: Some(Utc::now().timestamp_millis()),
                from_uid: "user_002".to_string(),
                message_type: 1,
                content: "Hello World".to_string(),
                status: 1,
                voice_status: 0,
                created_at: "".to_string(),
                updated_at: "".to_string(),
                searchable_word: "Hello World".to_string(),
                client_msg_no: "client_002".to_string(),
                is_deleted: 0,
                setting: 0,
                order_seq: 2,
                extra: "{}".to_string(),
                flame: 0,
                flame_second: 0,
                viewed: 0,
                viewed_at: 0,
                topic_id: "".to_string(),
                expire_time: None,
                expire_timestamp: None,
            },
        ];
        
        dao.batch_insert(&messages).unwrap();
        
        // 搜索消息
        let results = dao.search("test_channel", 1, "Hello", Some(10)).unwrap();
        assert_eq!(results.len(), 2);
        
        let rust_results = dao.search("test_channel", 1, "Rust", Some(10)).unwrap();
        assert_eq!(rust_results.len(), 1);
        assert_eq!(rust_results[0].content, "Hello Rust");
    }
} 