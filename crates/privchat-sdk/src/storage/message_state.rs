use crate::storage::entities::{MessageStatus};
use crate::error::{Result, PrivchatSDKError};
use rusqlite::{Connection, params, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 消息状态变更事件
#[derive(Debug, Clone)]
pub struct MessageStatusEvent {
    pub local_message_id: u64,
    pub channel_id: String,
    pub old_status: MessageStatus,
    pub new_status: MessageStatus,
    pub timestamp: u64,
    pub extra_data: HashMap<String, String>,
}

/// 负责管理消息状态的原子更新和事件通知
pub struct MessageStateManager {
    conn: Arc<Mutex<Connection>>,
}

impl MessageStatus {
    /// 检查是否可以从当前状态转换到目标状态
    pub fn can_transition_to(&self, target: MessageStatus) -> bool {
        match (self, target) {
            (MessageStatus::Draft, MessageStatus::Sending) => true,
            (MessageStatus::Sending, MessageStatus::Sent) => true,
            (MessageStatus::Sent, MessageStatus::Delivered) => true,
            (MessageStatus::Delivered, MessageStatus::Read) => true,
            (MessageStatus::Sending, MessageStatus::Failed) => true,
            (MessageStatus::Failed, MessageStatus::Retrying) => true,
            (MessageStatus::Retrying, MessageStatus::Sending) => true,
            (MessageStatus::Sent, MessageStatus::Revoked) => true,
            (MessageStatus::Delivered, MessageStatus::Revoked) => true,
            (MessageStatus::Read, MessageStatus::Revoked) => true,
            (MessageStatus::Draft, MessageStatus::Expired) => true,
            (MessageStatus::Sending, MessageStatus::Expired) => true,
            (MessageStatus::Sent, MessageStatus::Expired) => true,
            (MessageStatus::Delivered, MessageStatus::Expired) => true,
            (MessageStatus::Read, MessageStatus::Expired) => true,
            _ => false,
        }
    }
}

impl MessageStateManager {
    pub fn new(conn: Connection) -> Self {
        Self { 
            conn: Arc::new(Mutex::new(conn))
        }
    }
    
    /// 从共享的数据库连接创建 MessageStateManager
    pub fn new_from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    pub fn update_message_state(&self, local_message_id: u64, new_status: i32) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE messages SET status = ?1 WHERE local_message_id = ?2",
            params![new_status, local_message_id]
        )?;

        Ok(updated > 0)
    }

    pub fn batch_update_message_states(&self, updates: &[(String, i32)]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "UPDATE messages SET status = ?1 WHERE local_message_id = ?2"
        )?;
        
        let mut updated = 0;
        for (local_message_id, new_status) in updates {
            updated += stmt.execute(params![*new_status, local_message_id])?;
        }
        
        Ok(updated)
    }
    
    pub fn query_message_status(&self, local_message_id: u64) -> Result<Option<i32>> {
        let conn = self.conn.lock().unwrap();
        let sql = "SELECT status FROM messages WHERE local_message_id = ?1";
        let mut stmt = conn.prepare(sql)?;
        Ok(stmt.query_row(params![local_message_id], |row| row.get(0)).optional()?)
    }
    
    /// 更新消息的服务端 message_id（发送成功后调用）
    pub fn update_server_message_id(&self, local_message_id: u64, server_message_id: u64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE message SET message_id = ?1 WHERE local_message_id = ?2",
            params![server_message_id, local_message_id]
        )?;
        
        tracing::info!("✅ 已更新消息的服务端ID: local_message_id={}, message_id={}, affected_rows={}", 
                      local_message_id, server_message_id, updated);
        Ok(updated > 0)
    }

    pub fn query_message_status_batch(&self, local_message_ids: &[String]) -> Result<HashMap<String, i32>> {
        let conn = self.conn.lock().unwrap();
        let json_array = serde_json::to_string(local_message_ids).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
        let sql = "SELECT local_message_id, status FROM messages WHERE local_message_id IN (SELECT value FROM json_each(?1))";
        let mut stmt = conn.prepare(sql)?;
        let mut results = HashMap::new();
        let rows = stmt.query_map(params![json_array], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i32>(1)?))
        })?;
        for row in rows {
            let (local_message_id, status) = row?;
            results.insert(local_message_id, status);
        }
        Ok(results)
    }
    
    pub fn update_message_extra(&self, local_message_id: u64, extra: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE messages SET extra = ?1 WHERE local_message_id = ?2",
            params![extra, local_message_id]
        )?;
        
        Ok(updated > 0)
    }
    
    pub fn batch_update_message_extra(&self, updates: &[(String, String)]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "UPDATE messages SET extra = ?1 WHERE local_message_id = ?2"
        )?;
        
        let mut updated = 0;
        for (local_message_id, extra) in updates {
            updated += stmt.execute(params![extra, local_message_id])?;
        }
        
        Ok(updated)
    }
    
    pub fn query_message_extra(&self, local_message_id: u64) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT extra FROM messages WHERE local_message_id = ?1"
        )?;
        
        Ok(stmt.query_row(params![local_message_id], |row| row.get(0)).optional()?)
    }
    
    pub fn batch_query_message_extra(&self, local_message_ids: &[String]) -> Result<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let json_array = serde_json::to_string(local_message_ids).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT local_message_id, extra FROM messages WHERE local_message_id IN (SELECT value FROM json_each(?1))"
        )?;
        
        let rows = stmt.query_map(params![json_array], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        
        let mut results = HashMap::new();
        for row in rows {
            let (local_message_id, extra) = row?;
            results.insert(local_message_id, extra);
        }
        
        Ok(results)
    }
    
    pub fn update_message_events(&self, events: &[(String, i32, String)]) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "UPDATE messages SET status = ?1, events = json_array_append(COALESCE(events, '[]'), ?2) WHERE local_message_id = ?3"
        )?;
        
        let mut updated = 0;
        for (local_message_id, new_status, event) in events {
            updated += stmt.execute(params![*new_status, event, local_message_id])?;
        }
        
        Ok(updated)
    }

    pub fn query_message_events(&self, local_message_id: u64) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let sql = "SELECT events FROM messages WHERE local_message_id = ?1";
        let mut stmt = conn.prepare(sql)?;
        let events_json: Option<String> = stmt.query_row(params![local_message_id], |row| row.get(0)).optional()?;
        if let Some(events_json) = events_json {
            let events: Vec<String> = serde_json::from_str(&events_json).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
            Ok(events)
        } else {
            Ok(Vec::new())
        }
    }

    pub fn query_message_events_batch(&self, local_message_ids: &[String]) -> Result<HashMap<String, Vec<String>>> {
        let conn = self.conn.lock().unwrap();
        let json_array = serde_json::to_string(local_message_ids).map_err(|e| PrivchatSDKError::JsonError(e.to_string()))?;
        let sql = "SELECT local_message_id, events FROM messages WHERE local_message_id IN (SELECT value FROM json_each(?1))";
        let mut stmt = conn.prepare(sql)?;
        let mut results = HashMap::new();
        let rows = stmt.query_map(params![json_array], |row| {
            let local_message_id = row.get::<_, String>(0)?;
            let events_json: Option<String> = row.get(1)?;
            let events = if let Some(events_json) = events_json {
                serde_json::from_str(&events_json).map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))?
            } else {
                Vec::new()
            };
            Ok((local_message_id, events))
        })?;
        for row in rows {
            let (local_message_id, events) = row?;
            results.insert(local_message_id, events);
        }
        Ok(results)
    }
    
    pub fn query_undelivered_messages(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT local_message_id FROM messages WHERE status < 3 ORDER BY created_at ASC"
        )?;
        
        let rows = stmt.query_map([], |row| row.get(0))?;
        
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        
        Ok(results)
    }
}

impl Clone for MessageStateManager {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::entities::MessageStatus;
    
    #[tokio::test]
    async fn test_status_transition_validation() {
        // 测试有效转换
        assert!(MessageStatus::Draft.can_transition_to(MessageStatus::Sending));
        assert!(MessageStatus::Sending.can_transition_to(MessageStatus::Sent));
        assert!(MessageStatus::Sent.can_transition_to(MessageStatus::Delivered));
        assert!(MessageStatus::Delivered.can_transition_to(MessageStatus::Read));
        
        // 测试无效转换
        assert!(!MessageStatus::Read.can_transition_to(MessageStatus::Sending));
        assert!(!MessageStatus::Revoked.can_transition_to(MessageStatus::Sent));
        assert!(!MessageStatus::Burned.can_transition_to(MessageStatus::Read));
    }
    
    #[tokio::test]
    async fn test_status_helpers() {
        assert!(MessageStatus::Read.is_final_state());
        assert!(MessageStatus::Sent.is_sent_successfully());
        assert!(MessageStatus::Failed.is_send_failed());
        assert!(MessageStatus::Sending.needs_network_processing());
        assert!(MessageStatus::Failed.can_retry());
        assert!(MessageStatus::Sent.can_revoke());
    }
} 