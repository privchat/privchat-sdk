//! 会话数据访问层 - 管理会话列表和未读数等

use rusqlite::{Connection, params, Row};
use crate::error::Result;
use crate::storage::entities::Conversation;

/// 会话数据访问对象
pub struct ConversationDao<'a> {
    conn: &'a Connection,
}

impl<'a> ConversationDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 插入或更新会话
    pub fn upsert(&self, conversation: &Conversation) -> Result<i64> {
        let sql = "INSERT OR REPLACE INTO conversation (
            channel_id, channel_type, last_client_msg_no, last_msg_timestamp,
            unread_count, is_deleted, version, extra, last_msg_seq,
            parent_channel_id, parent_channel_type
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)";
        
        self.conn.execute(sql, params![
            conversation.channel_id,
            conversation.channel_type,
            conversation.last_client_msg_no,
            conversation.last_msg_timestamp,
            conversation.unread_count,
            conversation.is_deleted,
            conversation.version,
            conversation.extra,
            conversation.last_msg_seq,
            conversation.parent_channel_id,
            conversation.parent_channel_type,
        ])?;
        
        Ok(self.conn.last_insert_rowid())
    }
    
    /// 根据频道ID获取会话
    pub fn get_by_channel(&self, channel_id: &str, channel_type: i32) -> Result<Option<Conversation>> {
        let sql = "SELECT * FROM conversation WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![channel_id, channel_type], |row| {
            Ok(self.row_to_conversation(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(conversation)) => Ok(Some(conversation)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!("查询会话失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 更新未读数
    pub fn update_unread_count(&self, channel_id: &str, channel_type: i32, count: i32) -> Result<()> {
        let sql = "UPDATE conversation SET unread_count = ?1 WHERE channel_id = ?2 AND channel_type = ?3";
        self.conn.execute(sql, params![count, channel_id, channel_type])?;
        Ok(())
    }
    
    /// 将行转换为会话实体
    fn row_to_conversation(&self, row: &Row) -> rusqlite::Result<Conversation> {
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
    }
}
