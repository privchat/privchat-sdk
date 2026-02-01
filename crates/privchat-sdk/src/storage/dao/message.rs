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
    /// 
    /// ✨ 如果消息有@提及，会在 extra 字段中标记 `has_mention: true`
    pub fn insert(&self, message: &Message) -> Result<i64> {
        let now = Utc::now().timestamp_millis();
        
        // ✨ 如果消息的 extra 中已经有 has_mention 标记，保留它
        // 否则使用原始的 extra（has_mention 标记由 process_mentions 在收到消息时设置）
        let extra_str = message.extra.clone();
        
        let sql = "INSERT INTO message (
            message_id, pts, channel_id, channel_type, timestamp, from_uid, 
            type, content, status, voice_status, created_at, updated_at, 
            searchable_word, local_message_id, is_deleted, setting, order_seq, extra,
            flame, flame_second, viewed, viewed_at, topic_id, expire_time, expire_timestamp,
            revoked, revoked_at, revoked_by
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
        )";
        
        let _result = self.conn.execute(sql, params![
            message.server_message_id.map(|id| id as i64),  // u64 -> i64
            message.pts,
            message.channel_id as i64,  // u64 -> i64
            message.channel_type,
            message.timestamp,
            message.from_uid as i64,  // u64 -> i64
            message.message_type,
            message.content,
            message.status,
            message.voice_status,
            if message.created_at == 0 { now } else { message.created_at },
            if message.updated_at == 0 { now } else { message.updated_at },
            message.searchable_word,
            message.local_message_id,
            message.is_deleted,
            message.setting,
            message.order_seq,
            extra_str,  // 使用更新后的 extra
            message.flame,
            message.flame_second,
            message.viewed,
            message.viewed_at,
            message.topic_id,
            message.expire_time,
            message.expire_timestamp,
            message.revoked,
            message.revoked_at,
            message.revoked_by.map(|id| id as i64),  // u64 -> i64
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
    
    /// 按 (channel_id, server_message_id) 检查消息是否已存在，用于落库前去重
    /// 返回已存在时的 message.id（行 id），不存在返回 None
    pub fn get_id_by_channel_and_server_message_id(
        &self,
        channel_id: u64,
        server_message_id: u64,
    ) -> Result<Option<i64>> {
        let sql = "SELECT id FROM message WHERE channel_id = ?1 AND message_id = ?2 AND is_deleted = 0";
        match self.conn.query_row(
            sql,
            params![channel_id as i64, server_message_id as i64],
            |row| row.get(0),
        ) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(PrivchatSDKError::Database(format!("查重失败: {}", e))),
        }
    }

    /// 根据 server message_id 获取消息
    /// 
    /// ⚠️ **注意**：本方法按 **server message_id** 查询（WHERE message_id = ?），非 message.id。
    /// 
    /// 用途：当已知 server message_id（如推送、同步场景）时，查找对应的本地消息记录。
    /// 
    /// **业务侧消息操作**（revoke、edit、retry 等）应使用 `get_by_id(message.id)`。
    pub fn get_by_message_id(&self, message_id: u64) -> Result<Option<Message>> {
        let sql = "SELECT * FROM message WHERE message_id = ?1 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![message_id as i64], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(e)) => Err(PrivchatSDKError::Database(format!("查询消息失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 根据 message.id（主键）获取消息
    pub fn get_by_id(&self, id: i64) -> Result<Option<Message>> {
        let sql = "SELECT * FROM message WHERE id = ?1 AND is_deleted = 0";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![id], |row| Ok(self.row_to_message(row)?))?;
        match rows.next() {
            Some(Ok(message)) => Ok(Some(message)),
            Some(Err(e)) => Err(PrivchatSDKError::Database(format!("查询消息失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 查询消息列表
    pub fn query(&self, query: &MessageQuery) -> Result<PageResult<Message>> {
        let mut sql = "SELECT * FROM message WHERE 1=1".to_string();
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        
        // 构建查询条件
        if let Some(channel_id) = query.channel_id {
            sql.push_str(" AND channel_id = ?");
            params.push(Box::new(channel_id as i64));
        }
        
        if let Some(channel_type) = query.channel_type {
            sql.push_str(" AND channel_type = ?");
            params.push(Box::new(channel_type));
        }
        
        if let Some(from_uid) = query.from_uid {
            sql.push_str(" AND from_uid = ?");
            params.push(Box::new(from_uid as i64));
        }
        
        if let Some(message_type) = query.message_type {
            sql.push_str(" AND type = ?");
            params.push(Box::new(message_type));
        }
        
        if let Some(start_time) = query.start_time {
            sql.push_str(" AND timestamp >= ?");
            params.push(Box::new(start_time));
        }
        
        if let Some(end_time) = query.end_time {
            sql.push_str(" AND timestamp <= ?");
            params.push(Box::new(end_time));
        }
        
        if let Some(is_deleted) = query.is_deleted {
            sql.push_str(" AND is_deleted = ?");
            params.push(Box::new(is_deleted));
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
    pub fn search(&self, channel_id: u64, channel_type: i32, keyword: &str, limit: Option<u32>) -> Result<Vec<Message>> {
        let limit = limit.unwrap_or(20);
        
        let sql = "SELECT m.* FROM message m 
                  WHERE m.channel_id = ?1 AND m.channel_type = ?2 
                  AND m.is_deleted = 0 
                  AND (m.content LIKE ?3 OR m.searchable_word LIKE ?3)
                  ORDER BY m.timestamp DESC 
                  LIMIT ?4";
        
        let keyword_pattern = format!("%{}%", keyword);
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![channel_id as i64, channel_type, keyword_pattern, limit], |row| {
            Ok(self.row_to_message(row)?)
        })?;
        
        let mut messages = Vec::new();
        for row in rows {
            messages.push(row?);
        }
        
        Ok(messages)
    }
    
    /// 更新消息状态（按 message.id）
    pub fn update_status(&self, id: i64, status: crate::storage::entities::MessageStatus) -> Result<()> {
        let sql = "UPDATE message SET status = ?1, updated_at = ?2 WHERE id = ?3";
        let now = Utc::now().timestamp_millis();
        let affected = self.conn.execute(sql, params![status as i32, now, id])?;
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        Ok(())
    }
    
    /// 更新消息的服务端 ID（按 message.id，仅协议层写入）
    pub fn update_server_message_id(&self, id: i64, server_message_id: u64) -> Result<()> {
        let sql = "UPDATE message SET message_id = ?1, updated_at = ?2 WHERE id = ?3";
        let now = Utc::now().timestamp_millis();
        self.conn.execute(sql, params![server_message_id as i64, now, id])?;
        Ok(())
    }
    
    /// 撤回消息（按 message.id）
    pub fn revoke(&self, id: i64, revoker_id: u64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().timestamp_millis();
        let revoked_at = now;
        let sql = "UPDATE message SET revoked = 1, revoked_at = ?1, revoked_by = ?2, updated_at = ?3 WHERE id = ?4";
        self.conn.execute(sql, params![revoked_at, revoker_id as i64, now, id])?;
        let extra_sql = "INSERT OR REPLACE INTO message_extra (message_id, revoke, revoker, extra_version) VALUES (?1, 1, ?2, ?3)";
        self.conn.execute(extra_sql, params![id, revoker_id as i64, Utc::now().timestamp()])?;
        tx.commit()?;
        Ok(())
    }
    
    /// 编辑消息（按 message.id）
    pub fn edit(&self, id: i64, new_content: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let now = Utc::now().timestamp_millis();
        let sql = "UPDATE message SET content = ?1, updated_at = ?2 WHERE id = ?3";
        self.conn.execute(sql, params![new_content, now, id])?;
        let extra_sql = "INSERT OR REPLACE INTO message_extra (message_id, content_edit, edited_at, extra_version) VALUES (?1, ?2, ?3, ?4)";
        self.conn.execute(extra_sql, params![id, new_content, Utc::now().timestamp(), Utc::now().timestamp()])?;
        tx.commit()?;
        Ok(())
    }
    
    /// 标记消息为已读（按 message.id）
    pub fn mark_as_read(&self, id: i64) -> Result<()> {
        let sql = "UPDATE message SET viewed = 1, viewed_at = ?1, updated_at = ?2 WHERE id = ?3";
        let now = Utc::now().timestamp_millis();
        self.conn.execute(sql, params![now, now, id])?;
        Ok(())
    }
    
    /// 删除过期消息
    pub fn delete_expired(&self) -> Result<u32> {
        let sql = "UPDATE message SET is_deleted = 1, updated_at = ?1 
                  WHERE expire_timestamp > 0 AND expire_timestamp < ?2 AND is_deleted = 0";
        let now = Utc::now().timestamp_millis();
        let now_str = Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string();
        
        let affected = self.conn.execute(sql, params![now_str, now])?;
        Ok(affected as u32)
    }
    
    /// 获取频道中的消息数量
    pub fn count_by_channel(&self, channel_id: u64, channel_type: i32) -> Result<i64> {
        let sql = "SELECT COUNT(*) FROM message WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0";
        let count: i64 = self.conn.query_row(sql, params![channel_id as i64, channel_type], |row| row.get(0))?;
        Ok(count)
    }
    
    /// 获取用户发送的消息数量
    pub fn count_by_user(&self, from_uid: u64) -> Result<i64> {
        let sql = "SELECT COUNT(*) FROM message WHERE from_uid = ?1 AND is_deleted = 0";
        let count: i64 = self.conn.query_row(sql, params![from_uid as i64], |row| row.get(0))?;
        Ok(count)
    }
    
    /// 软删除消息（按 message.id）
    pub fn soft_delete(&self, id: i64) -> Result<()> {
        let sql = "UPDATE message SET is_deleted = 1, updated_at = ?1 WHERE id = ?2";
        let now = Utc::now().timestamp_millis();
        let affected = self.conn.execute(sql, params![now, id])?;
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        Ok(())
    }
    
    /// 恢复删除的消息（按 message.id）
    pub fn restore(&self, id: i64) -> Result<()> {
        let sql = "UPDATE message SET is_deleted = 0, updated_at = ?1 WHERE id = ?2";
        let now = Utc::now().timestamp_millis();
        let affected = self.conn.execute(sql, params![now, id])?;
        if affected == 0 {
            return Err(PrivchatSDKError::Database("消息不存在".to_string()));
        }
        Ok(())
    }
    
    /// 将数据库行转换为 Message 实体
    fn row_to_message(&self, row: &Row) -> rusqlite::Result<Message> {
        Ok(Message {
            id: row.get("id")?,  // ⭐ client_seq -> id
            server_message_id: row.get::<_, Option<i64>>("message_id")?.map(|id| id as u64),  // 列名仍为 message_id
            pts: row.get("pts")?,  // ⭐ message_seq -> pts
            channel_id: row.get::<_, i64>("channel_id")? as u64,  // i64 -> u64
            channel_type: row.get("channel_type")?,
            timestamp: row.get("timestamp")?,
            from_uid: row.get::<_, i64>("from_uid")? as u64,  // i64 -> u64
            message_type: row.get("type")?,
            content: row.get("content")?,
            status: row.get("status")?,
            voice_status: row.get("voice_status")?,
            created_at: row.get("created_at").unwrap_or(0),
            updated_at: row.get("updated_at").unwrap_or(0),
            searchable_word: row.get("searchable_word")?,
            local_message_id: row.get("local_message_id")?,
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
            revoked: row.get("revoked").unwrap_or(0),
            revoked_at: row.get("revoked_at").unwrap_or(0),
            revoked_by: row.get::<_, Option<i64>>("revoked_by")?.map(|id| id as u64),  // i64 -> u64
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
                pts BIGINT DEFAULT 0,
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
                local_message_id INTEGER NOT NULL DEFAULT 0,
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
                expire_timestamp INTEGER,
                revoked SMALLINT NOT NULL DEFAULT 0,
                revoked_at BIGINT DEFAULT 0,
                revoked_by TEXT NOT NULL DEFAULT ''
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
            id: None,
            server_message_id: Some(1001),
            pts: 1,  // ⭐ message_seq -> pts
            channel_id: 2001,
            channel_type: 1,
            timestamp: Some(Utc::now().timestamp_millis()),
            from_uid: 3001,
            message_type: 1,
            content: "Hello, world!".to_string(),
            status: 1,
            voice_status: 0,
            created_at: 0,
            updated_at: 0,
            searchable_word: "Hello world".to_string(),
            local_message_id: 1,
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
            revoked: 0,
            revoked_at: 0,
            revoked_by: None,
        };
        
        // 插入消息
        let row_id = dao.insert(&message).unwrap();
        assert!(row_id > 0);
        
        // 获取消息
        let retrieved = dao.get_by_message_id(1001).unwrap();
        assert!(retrieved.is_some());
        
        let retrieved_msg = retrieved.unwrap();
        assert_eq!(retrieved_msg.server_message_id, Some(1001));
        assert_eq!(retrieved_msg.content, "Hello, world!");
    }
    
    #[test]
    fn test_search_messages() {
        let conn = create_test_db();
        let dao = MessageDao::new(&conn);
        
        // 插入测试消息
        let messages = vec![
            Message {
                id: None,
                server_message_id: Some(1001),
                pts: 1,  // ⭐ message_seq -> pts
                channel_id: 2001,
                channel_type: 1,
                timestamp: Some(Utc::now().timestamp_millis()),
                from_uid: 3001,
                message_type: 1,
                content: "Hello Rust".to_string(),
                status: 1,
                voice_status: 0,
                created_at: 0,
                updated_at: 0,
                searchable_word: "Hello Rust".to_string(),
                local_message_id: 1,
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
                revoked: 0,
                revoked_at: 0,
                revoked_by: None,
            },
            Message {
                id: None,
                server_message_id: Some(1002),
                pts: 2,  // ⭐ message_seq -> pts
                channel_id: 2001,
                channel_type: 1,
                timestamp: Some(Utc::now().timestamp_millis()),
                from_uid: 3002,
                message_type: 1,
                content: "Hello World".to_string(),
                status: 1,
                voice_status: 0,
                created_at: 0,
                updated_at: 0,
                searchable_word: "Hello World".to_string(),
                local_message_id: 2,
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
                revoked: 0,
                revoked_at: 0,
                revoked_by: None,
            },
        ];
        
        dao.batch_insert(&messages).unwrap();
        
        // 搜索消息
        let results = dao.search(2001, 1, "Hello", Some(10)).unwrap();
        assert_eq!(results.len(), 2);
        
        let rust_results = dao.search(2001, 1, "Rust", Some(10)).unwrap();
        assert_eq!(rust_results.len(), 1);
        assert_eq!(rust_results[0].content, "Hello Rust");
    }
} 