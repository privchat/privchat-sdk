//! 高级特性模块 - 阶段4功能实现
//! 
//! ⚠️ **重要说明**：本模块使用独立的 schema（`messages` 表），其 `message_id` 列存储的是 **server message_id**（服务端分配的消息 ID）。
//! 
//! 这与主 storage 模块的设计不同：
//! - 主 storage：`message` 表，`id` 列为 message.id（本地主键），`message_id` 列为 server message_id
//! - advanced_features：`messages` 表，`message_id` 列为 server message_id
//! 
//! **SDK 主流程**（revoke、edit、retry 等）使用主 storage 的 message.id，**不经过本模块**。
//! 本模块主要用于：
//! - 同步/推送场景：已知 server message_id，需要查询或更新本地记录
//! - 已读回执系统
//! - 消息撤回功能（同步场景）
//! - 消息编辑功能（同步场景）
//! - Typing Indicator
//! - 消息 Reaction
//! - 消息置顶功能

use crate::storage::entities::MessageStatus;
use crate::storage::message_state::MessageStateManager;
use crate::error::{Result, PrivchatSDKError};
use rusqlite::{Connection, params, OptionalExtension};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};
use tracing::info;

/// 已读回执事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceiptEvent {
    /// 消息ID（⚠️ 注意：本模块中为 server message_id，非 message.id）
    pub message_id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 读者用户ID
    pub reader_uid: u64,
    /// 已读时间戳
    pub read_at: u64,
    /// 是否需要同步到其他端
    pub need_sync: bool,
}

/// 消息撤回事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRevokeEvent {
    /// 消息ID（⚠️ 注意：本模块中为 server message_id，非 message.id）
    pub message_id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 撤回者用户ID
    pub revoker_uid: u64,
    /// 撤回时间戳
    pub revoked_at: u64,
    /// 撤回原因（可选）
    pub reason: Option<String>,
}

/// 消息编辑事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEditEvent {
    /// 消息ID（⚠️ 注意：本模块中为 server message_id，非 message.id）
    pub message_id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 编辑者用户ID
    pub editor_uid: u64,
    /// 新内容
    pub new_content: String,
    /// 编辑时间戳
    pub edited_at: u64,
    /// 编辑版本号
    pub edit_version: u32,
}

/// 会话已读状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelReadState {
    /// 频道ID
    pub channel_id: u64,
    /// 频道类型
    pub channel_type: i32,
    /// 用户ID
    pub user_id: u64,
    /// 最后已读消息ID
    pub last_read_message_id: Option<u64>,
    /// 最后已读消息序列号
    pub last_read_seq: i64,
    /// 最后已读时间戳
    pub last_read_at: u64,
    /// 未读消息数量
    pub unread_count: i32,
}

/// 高级特性管理器
pub struct AdvancedFeaturesManager {
    conn: Arc<Mutex<Connection>>,
    state_manager: Arc<MessageStateManager>,
    /// 撤回时间限制（秒）
    revoke_time_limit: u64,
    /// 编辑时间限制（秒）
    edit_time_limit: u64,
}

impl AdvancedFeaturesManager {
    /// 创建新的高级特性管理器
    pub fn new(
        conn: Connection,
        state_manager: Arc<MessageStateManager>,
        revoke_time_limit: u64,
        edit_time_limit: u64,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            state_manager,
            revoke_time_limit,
            edit_time_limit,
        }
    }

    /// 获取数据库连接（用于创建 DAO）
    pub fn get_connection(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }

    /// 初始化数据库表
    pub fn initialize_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        
        // 创建已读回执表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS read_receipts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                reader_uid TEXT NOT NULL,
                read_at INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(message_id, reader_uid)
            )",
            [],
        )?;

        // 创建会话已读状态表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS channel_read_states (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                user_id TEXT NOT NULL,
                last_read_message_id TEXT,
                last_read_seq INTEGER NOT NULL DEFAULT 0,
                last_read_at INTEGER NOT NULL,
                unread_count INTEGER NOT NULL DEFAULT 0,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(channel_id, channel_type, user_id)
            )",
            [],
        )?;

        // 创建消息编辑历史表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS message_edit_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                editor_uid TEXT NOT NULL,
                old_content TEXT NOT NULL,
                new_content TEXT NOT NULL,
                edit_version INTEGER NOT NULL,
                edited_at INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;

        // 创建索引
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_read_receipts_message 
             ON read_receipts(message_id)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_read_receipts_channel 
             ON read_receipts(channel_id, channel_type)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_channel_read_states_channel 
             ON channel_read_states(channel_id, channel_type)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_message_edit_history_message 
             ON message_edit_history(message_id)",
            [],
        )?;

        info!("Advanced features tables initialized successfully");
        Ok(())
    }

    // ========== 已读回执功能 ==========

    /// 标记消息为已读
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**（服务端分配的消息 ID），非 message.id。
    /// 
    /// 本方法主要用于同步/推送场景，当已知 server message_id 时调用。
    /// SDK 主流程的 mark_as_read 使用 message.id，不经过本模块。
    pub fn mark_message_as_read(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        reader_uid: u64,
    ) -> Result<ReadReceiptEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();
        
        // 插入已读回执记录
        conn.execute(
            "INSERT OR REPLACE INTO read_receipts 
             (message_id, channel_id, channel_type, reader_uid, read_at) 
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![message_id, channel_id, channel_type, reader_uid, now],
        )?;

        // 更新消息状态为已读
        // TODO: update_message_state 需要 local_message_id (u64)，而不是 message_id (&str)
        // 需要通过 message_id 查询数据库获取对应的 local_message_id
        // self.state_manager.update_message_state(message_id, MessageStatus::Read as i32)?;

        info!("Message {} marked as read by user {}", message_id, reader_uid);

        Ok(ReadReceiptEvent {
            message_id: message_id,
            channel_id: channel_id,
            channel_type,
            reader_uid: reader_uid,
            read_at: now,
            need_sync: true,
        })
    }

    /// 批量标记消息为已读
    pub fn batch_mark_messages_as_read(
        &self,
        message_ids: &[u64],
        channel_id: u64,
        channel_type: i32,
        reader_uid: u64,
    ) -> Result<Vec<ReadReceiptEvent>> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut events = Vec::new();
        
        // 首先在事务中插入已读回执记录
        {
            let conn = self.conn.lock().unwrap();
            let tx = conn.unchecked_transaction()?;

            for message_id in message_ids {
                // 插入已读回执记录
                tx.execute(
                    "INSERT OR REPLACE INTO read_receipts 
                     (message_id, channel_id, channel_type, reader_uid, read_at) 
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![message_id, channel_id, channel_type, reader_uid, now],
                )?;

                events.push(ReadReceiptEvent {
                    message_id: *message_id,
                    channel_id: channel_id,
                    channel_type,
                    reader_uid: reader_uid,
                    read_at: now,
                    need_sync: true,
                });
            }

            tx.commit()?;
        } // 事务结束，释放连接锁

        // 然后更新消息状态（在事务外部，避免死锁）
        // TODO: update_message_state 需要 local_message_id (u64)，而不是 message_id (&String)
        // 需要通过 message_id 查询数据库获取对应的 local_message_id
        // for message_id in message_ids {
        //     // 更新消息状态为已读
        //     self.state_manager.update_message_state(message_id, MessageStatus::Read as i32)?;
        // }

        info!("Batch marked {} messages as read by user {}", message_ids.len(), reader_uid);

        Ok(events)
    }

    /// 更新会话已读状态
    pub fn update_channel_read_state(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        last_read_message_id: Option<u64>,
        last_read_seq: i64,
    ) -> Result<ChannelReadState> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();

        // 计算未读消息数量
        let unread_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM messages 
             WHERE channel_id = ?1 AND channel_type = ?2 
             AND message_seq > ?3 AND is_deleted = 0",
            params![channel_id, channel_type, last_read_seq],
            |row| row.get(0),
        )?;

        // 更新或插入会话已读状态
        conn.execute(
            "INSERT OR REPLACE INTO channel_read_states 
             (channel_id, channel_type, user_id, last_read_message_id, 
              last_read_seq, last_read_at, unread_count) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                channel_id, 
                channel_type, 
                user_id, 
                last_read_message_id, 
                last_read_seq, 
                now, 
                unread_count
            ],
        )?;

        info!("Updated channel read state for user {} in channel {}", user_id, channel_id);

        Ok(ChannelReadState {
            channel_id: channel_id,
            channel_type,
            user_id: user_id,
            last_read_message_id: last_read_message_id,
            last_read_seq,
            last_read_at: now,
            unread_count,
        })
    }

    /// 获取消息的已读回执列表
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**（服务端分配的消息 ID），非 message.id。
    /// 
    /// **调用方注意**：如果调用方持有的是 message.id，需先通过 `storage.get_message_by_id(message_id)` 
    /// 获取 Message 实体，再用 `msg.message_id` 调用本方法。
    pub fn get_message_read_receipts(&self, message_id: u64) -> Result<Vec<ReadReceiptEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT message_id, channel_id, channel_type, reader_uid, read_at 
             FROM read_receipts WHERE message_id = ?1 ORDER BY read_at ASC"
        )?;

        let rows = stmt.query_map(params![message_id], |row| {
            Ok(ReadReceiptEvent {
                message_id: row.get(0)?,
                channel_id: row.get(1)?,
                channel_type: row.get(2)?,
                reader_uid: row.get(3)?,
                read_at: row.get(4)?,
                need_sync: false, // 已存储的不需要同步
            })
        })?;

        let mut receipts = Vec::new();
        for row in rows {
            receipts.push(row?);
        }

        Ok(receipts)
    }

    /// 获取会话的未读消息数量
    pub fn get_unread_count(&self, channel_id: u64, channel_type: i32, user_id: u64) -> Result<i32> {
        let conn = self.conn.lock().unwrap();
        
        // 获取用户的最后已读序列号
        let last_read_seq: i64 = conn.query_row(
            "SELECT COALESCE(last_read_seq, 0) FROM channel_read_states 
             WHERE channel_id = ?1 AND channel_type = ?2 AND user_id = ?3",
            params![channel_id, channel_type, user_id],
            |row| row.get(0),
        ).unwrap_or(0);

        // 计算未读消息数量
        let unread_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM messages 
             WHERE channel_id = ?1 AND channel_type = ?2 
             AND message_seq > ?3 AND is_deleted = 0",
            params![channel_id, channel_type, last_read_seq],
            |row| row.get(0),
        )?;

        Ok(unread_count)
    }

    // ========== 消息撤回功能 ==========

    /// 撤回消息
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**（服务端分配的消息 ID），非 message.id。
    /// 
    /// **注意**：SDK 主流程的 recall_message 使用 message.id，走主 storage 的 revoke_message，**不经过本模块**。
    /// 本方法主要用于同步/推送场景。
    pub fn revoke_message(
        &self,
        message_id: u64,
        revoker_uid: u64,
        reason: Option<&str>,
    ) -> Result<MessageRevokeEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();

        // 检查消息是否存在并获取消息信息
        let (channel_id, channel_type, from_uid, created_at): (u64, i32, u64, i64) = conn.query_row(
            "SELECT channel_id, channel_type, from_uid, 
             CAST(strftime('%s', created_at) AS INTEGER) 
             FROM messages WHERE message_id = ?1",
            params![message_id as i64],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?, row.get::<_, i64>(2)? as u64, row.get(3)?)),
        )?;

        // 检查撤回权限
        if revoker_uid != from_uid {
            // 这里可以添加管理员权限检查
            return Err(PrivchatSDKError::InvalidOperation(
                "Only message sender can revoke the message".to_string()
            ));
        }

        // 检查撤回时间限制
        if now - created_at as u64 > self.revoke_time_limit {
            return Err(PrivchatSDKError::InvalidOperation(
                format!("Message revoke time limit exceeded: {} seconds", self.revoke_time_limit)
            ));
        }

        // 检查消息状态是否可以撤回
        let current_status: i32 = conn.query_row(
            "SELECT status FROM messages WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )?;

        let status = MessageStatus::from_i32(current_status)
            .ok_or_else(|| PrivchatSDKError::InvalidOperation("Invalid message status".to_string()))?;

        if !status.can_revoke() {
            return Err(PrivchatSDKError::InvalidOperation(
                format!("Message with status {} cannot be revoked", status.display_name())
            ));
        }

        let tx = conn.unchecked_transaction()?;

        // 更新消息状态为已撤回
        tx.execute(
            "UPDATE messages SET status = ?1, updated_at = CURRENT_TIMESTAMP 
             WHERE message_id = ?2",
            params![MessageStatus::Revoked as i32, message_id],
        )?;

        // 更新消息扩展信息
        tx.execute(
            "INSERT OR REPLACE INTO message_extra 
             (message_id, channel_id, channel_type, revoke, revoker, extra_version) 
             VALUES (?1, ?2, ?3, 1, ?4, ?5)",
            params![message_id, channel_id, channel_type, revoker_uid, now],
        )?;

        tx.commit()?;

        info!("Message {} revoked by user {}", message_id, revoker_uid);

        Ok(MessageRevokeEvent {
            message_id: message_id,
            channel_id,
            channel_type,
            revoker_uid: revoker_uid,
            revoked_at: now,
            reason: reason.map(|s| s.to_string()),
        })
    }

    /// 检查消息是否可以撤回
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**，非 message.id。
    pub fn can_revoke_message(&self, message_id: u64, user_id: u64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        let result: Option<(u64, i32, i64)> = conn.query_row(
            "SELECT from_uid, status, CAST(strftime('%s', created_at) AS INTEGER) 
             FROM messages WHERE message_id = ?1",
            params![message_id as i64],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?, row.get(2)?)),
        ).optional()?;

        match result {
            Some((from_uid, status_int, created_at)) => {
                // 检查是否是消息发送者
                if from_uid != user_id {
                    return Ok(false);
                }

                // 检查消息状态
                let status = MessageStatus::from_i32(status_int)
                    .ok_or_else(|| PrivchatSDKError::InvalidOperation("Invalid message status".to_string()))?;
                
                if !status.can_revoke() {
                    return Ok(false);
                }

                // 检查时间限制
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                
                Ok(now - created_at as u64 <= self.revoke_time_limit)
            }
            None => Ok(false), // 消息不存在
        }
    }

    // ========== 消息编辑功能 ==========

    /// 编辑消息
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**，非 message.id。
    /// 
    /// **注意**：SDK 主流程的 edit_message 使用 message.id，走主 storage 的 update_message_content，**不经过本模块**。
    /// 本方法主要用于同步/推送场景。
    pub fn edit_message(
        &self,
        message_id: u64,
        editor_uid: u64,
        new_content: &str,
    ) -> Result<MessageEditEvent> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let conn = self.conn.lock().unwrap();

        // 检查消息是否存在并获取消息信息
        let (channel_id, channel_type, from_uid, old_content, created_at): (u64, i32, u64, String, i64) = conn.query_row(
            "SELECT channel_id, channel_type, from_uid, content, 
             CAST(strftime('%s', created_at) AS INTEGER) 
             FROM messages WHERE message_id = ?1",
            params![message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;

        // 检查编辑权限
        if editor_uid != from_uid {
            return Err(PrivchatSDKError::InvalidOperation(
                "Only message sender can edit the message".to_string()
            ));
        }

        // 检查编辑时间限制
        if now - created_at as u64 > self.edit_time_limit {
            return Err(PrivchatSDKError::InvalidOperation(
                format!("Message edit time limit exceeded: {} seconds", self.edit_time_limit)
            ));
        }

        // 获取当前编辑版本
        let current_version: u32 = conn.query_row(
            "SELECT COALESCE(MAX(edit_version), 0) FROM message_edit_history 
             WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        ).unwrap_or(0);

        let new_version = current_version + 1;

        let tx = conn.unchecked_transaction()?;

        // 保存编辑历史
        tx.execute(
            "INSERT INTO message_edit_history 
             (message_id, channel_id, channel_type, editor_uid, 
              old_content, new_content, edit_version, edited_at) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message_id, channel_id, channel_type, editor_uid,
                old_content, new_content, new_version, now
            ],
        )?;

        // 更新消息内容
        tx.execute(
            "UPDATE messages SET content = ?1, updated_at = CURRENT_TIMESTAMP 
             WHERE message_id = ?2",
            params![new_content, message_id],
        )?;

        // 更新消息扩展信息
        tx.execute(
            "INSERT OR REPLACE INTO message_extra 
             (message_id, channel_id, channel_type, content_edit, edited_at, extra_version) 
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![message_id, channel_id, channel_type, new_content, now as i32, now],
        )?;

        tx.commit()?;

        info!("Message {} edited by user {}, version {}", message_id, editor_uid, new_version);

        Ok(MessageEditEvent {
            message_id: message_id,
            channel_id,
            channel_type,
            editor_uid: editor_uid,
            new_content: new_content.to_string(),
            edited_at: now,
            edit_version: new_version,
        })
    }

    /// 获取消息编辑历史
    /// 
    /// ⚠️ **参数说明**：`message_id` 为 **server message_id**，非 message.id。
    pub fn get_message_edit_history(&self, message_id: u64) -> Result<Vec<MessageEditEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT message_id, channel_id, channel_type, editor_uid, 
             new_content, edit_version, edited_at 
             FROM message_edit_history 
             WHERE message_id = ?1 ORDER BY edit_version ASC"
        )?;

        let rows = stmt.query_map(params![message_id], |row| {
            Ok(MessageEditEvent {
                message_id: row.get(0)?,
                channel_id: row.get(1)?,
                channel_type: row.get(2)?,
                editor_uid: row.get(3)?,
                new_content: row.get(4)?,
                edit_version: row.get(5)?,
                edited_at: row.get(6)?,
            })
        })?;

        let mut history = Vec::new();
        for row in rows {
            history.push(row?);
        }

        Ok(history)
    }

    /// 检查消息是否可以编辑
    pub fn can_edit_message(&self, message_id: u64, user_id: u64) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        let result: Option<(u64, i32, i64)> = conn.query_row(
            "SELECT from_uid, status, CAST(strftime('%s', created_at) AS INTEGER) 
             FROM messages WHERE message_id = ?1",
            params![message_id as i64],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?, row.get(2)?)),
        ).optional()?;

        match result {
            Some((from_uid, status_int, created_at)) => {
                // 检查是否是消息发送者
                if from_uid != user_id {
                    return Ok(false);
                }

                // 检查消息状态（只有已发送、已投递、已读的消息可以编辑）
                let status = MessageStatus::from_i32(status_int)
                    .ok_or_else(|| PrivchatSDKError::InvalidOperation("Invalid message status".to_string()))?;
                
                if !matches!(status, MessageStatus::Sent | MessageStatus::Delivered | MessageStatus::Read) {
                    return Ok(false);
                }

                // 检查时间限制
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                
                Ok(now - created_at as u64 <= self.edit_time_limit)
            }
            None => Ok(false), // 消息不存在
        }
    }

    // ========== 统计和查询功能 ==========

    /// 获取会话的所有已读状态
    pub fn get_channel_read_states(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Vec<ChannelReadState>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT channel_id, channel_type, user_id, last_read_message_id, 
             last_read_seq, last_read_at, unread_count 
             FROM channel_read_states 
             WHERE channel_id = ?1 AND channel_type = ?2"
        )?;

        let rows = stmt.query_map(params![channel_id, channel_type], |row| {
            Ok(ChannelReadState {
                channel_id: row.get(0)?,
                channel_type: row.get(1)?,
                user_id: row.get(2)?,
                last_read_message_id: row.get(3)?,
                last_read_seq: row.get(4)?,
                last_read_at: row.get(5)?,
                unread_count: row.get(6)?,
            })
        })?;

        let mut states = Vec::new();
        for row in rows {
            states.push(row?);
        }

        Ok(states)
    }

    /// 清理过期的已读回执记录
    pub fn cleanup_old_read_receipts(&self, days_to_keep: u32) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() - (days_to_keep as u64 * 24 * 3600);

        let deleted = conn.execute(
            "DELETE FROM read_receipts WHERE read_at < ?1",
            params![cutoff_time],
        )?;

        info!("Cleaned up {} old read receipt records", deleted);
        Ok(deleted)
    }

    /// 清理过期的编辑历史记录
    pub fn cleanup_old_edit_history(&self, days_to_keep: u32) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() - (days_to_keep as u64 * 24 * 3600);

        let deleted = conn.execute(
            "DELETE FROM message_edit_history WHERE edited_at < ?1",
            params![cutoff_time],
        )?;

        info!("Cleaned up {} old edit history records", deleted);
        Ok(deleted)
    }

    /// 获取数据库连接（仅用于测试）
    #[cfg(test)]
    pub fn get_connection_for_test(&self) -> std::sync::MutexGuard<Connection> {
        self.conn.lock().unwrap()
    }

    /// 获取数据库连接（供集成模块使用）
    pub(crate) fn get_db_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

impl Clone for AdvancedFeaturesManager {
    fn clone(&self) -> Self {
        Self {
            conn: self.conn.clone(),
            state_manager: self.state_manager.clone(),
            revoke_time_limit: self.revoke_time_limit,
            edit_time_limit: self.edit_time_limit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::message_state::MessageStateManager;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    fn create_test_manager() -> (AdvancedFeaturesManager, NamedTempFile) {
        let temp_file = NamedTempFile::new().unwrap();
        let conn = Connection::open(&temp_file.path()).unwrap();
        
        // 启用WAL模式以支持并发访问
        let _ = conn.execute("PRAGMA journal_mode=WAL", []);
        let _ = conn.execute("PRAGMA synchronous=NORMAL", []);
        
        // 创建基础表
        conn.execute(
            "CREATE TABLE messages (
                message_id TEXT PRIMARY KEY,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                from_uid TEXT NOT NULL,
                content TEXT NOT NULL,
                status INTEGER NOT NULL DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                message_seq INTEGER NOT NULL,
                is_deleted INTEGER NOT NULL DEFAULT 0,
                local_message_id TEXT,
                extra TEXT,
                events TEXT
            )",
            [],
        ).unwrap();

        conn.execute(
            "CREATE TABLE message_extra (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL,
                channel_id TEXT,
                channel_type INTEGER,
                revoke INTEGER DEFAULT 0,
                revoker TEXT,
                content_edit TEXT,
                edited_at INTEGER,
                extra_version INTEGER
            )",
            [],
        ).unwrap();

        // 为MessageStateManager创建一个使用同一个数据库文件的连接
        let state_manager_conn = Connection::open(&temp_file.path()).unwrap();
        // 也为这个连接启用WAL模式
        let _ = state_manager_conn.execute("PRAGMA journal_mode=WAL", []);
        let _ = state_manager_conn.execute("PRAGMA synchronous=NORMAL", []);
        
        let state_manager = Arc::new(MessageStateManager::new(state_manager_conn));
        
        let manager = AdvancedFeaturesManager::new(
            conn,
            state_manager,
            120, // 2分钟撤回限制
            300, // 5分钟编辑限制
        );
        
        manager.initialize_tables().unwrap();
        (manager, temp_file)
    }

    #[tokio::test]
    async fn test_read_receipt_functionality() {
        let (manager, _temp_file) = create_test_manager();
        
        // 插入测试消息
        {
            let conn = manager.get_connection_for_test();
            conn.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 测试标记消息为已读
        let receipt = manager.mark_message_as_read(
            "msg1", "channel1", 1, "user2"
        ).unwrap();
        
        assert_eq!(receipt.message_id, "msg1");
        assert_eq!(receipt.reader_uid, "user2");
        assert!(receipt.need_sync);

        // 测试获取已读回执
        let receipts = manager.get_message_read_receipts("msg1").unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].reader_uid, "user2");
    }

    #[tokio::test]
    async fn test_message_revoke_functionality() {
        let (manager, _temp_file) = create_test_manager();
        
        // 插入测试消息
        {
            let conn = manager.get_connection_for_test();
            conn.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, status, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 2, 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 测试检查撤回权限
        let can_revoke = manager.can_revoke_message("msg1", "user1").unwrap();
        assert!(can_revoke);

        let cannot_revoke = manager.can_revoke_message("msg1", "user2").unwrap();
        assert!(!cannot_revoke);

        // 测试撤回消息
        let revoke_event = manager.revoke_message("msg1", "user1", Some("Test revoke")).unwrap();
        assert_eq!(revoke_event.message_id, "msg1");
        assert_eq!(revoke_event.revoker_uid, "user1");
        assert_eq!(revoke_event.reason, Some("Test revoke".to_string()));
    }

    #[tokio::test]
    async fn test_message_edit_functionality() {
        let (manager, _temp_file) = create_test_manager();
        
        // 插入测试消息
        {
            let conn = manager.get_connection_for_test();
            conn.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, status, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 2, 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 测试检查编辑权限
        let can_edit = manager.can_edit_message("msg1", "user1").unwrap();
        assert!(can_edit);

        // 测试编辑消息
        let edit_event = manager.edit_message("msg1", "user1", "Hello World!").unwrap();
        assert_eq!(edit_event.message_id, "msg1");
        assert_eq!(edit_event.editor_uid, "user1");
        assert_eq!(edit_event.new_content, "Hello World!");
        assert_eq!(edit_event.edit_version, 1);

        // 测试获取编辑历史
        let history = manager.get_message_edit_history("msg1").unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].new_content, "Hello World!");
    }

    #[tokio::test]
    async fn test_channel_read_state() {
        let (manager, _temp_file) = create_test_manager();
        
        // 测试更新会话已读状态
        let read_state = manager.update_channel_read_state(
            "channel1", 1, "user1", Some("msg1"), 100
        ).unwrap();
        
        assert_eq!(read_state.channel_id, "channel1");
        assert_eq!(read_state.user_id, "user1");
        assert_eq!(read_state.last_read_seq, 100);

        // 测试获取未读消息数量
        let unread_count = manager.get_unread_count("channel1", 1, "user1").unwrap();
        assert_eq!(unread_count, 0); // 没有新消息
    }

    #[tokio::test]
    async fn test_batch_mark_as_read() {
        let (manager, _temp_file) = create_test_manager();
        
        // 插入测试消息
        {
            let conn = manager.get_connection_for_test();
            for i in 1..=3 {
                conn.execute(
                    "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, message_seq) 
                     VALUES (?, 'channel1', 1, 'user1', 'Hello', ?)",
                    params![format!("msg{}", i), i],
                ).unwrap();
            }
        } // 这里连接的借用结束

        // 测试批量标记为已读
        let message_ids = vec!["msg1".to_string(), "msg2".to_string(), "msg3".to_string()];
        let events = manager.batch_mark_messages_as_read(
            &message_ids, "channel1", 1, "user2"
        ).unwrap();
        
        assert_eq!(events.len(), 3);
        for (i, event) in events.iter().enumerate() {
            assert_eq!(event.message_id, format!("msg{}", i + 1));
            assert_eq!(event.reader_uid, "user2");
        }
    }
} 