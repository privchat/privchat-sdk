//! 高级特性集成模块 - 将所有高级特性与事件系统整合
//! 
//! 这个模块提供了一个统一的接口来使用所有高级特性，
//! 并自动处理事件的发布和状态同步。

use crate::storage::advanced_features::{
    AdvancedFeaturesManager, ReadReceiptEvent, MessageEditEvent
};
use crate::storage::message_state::MessageStateManager;
use crate::events::{EventManager, SDKEvent, event_builders, ConnectionState};
use crate::error::{Result, PrivchatSDKError};
use std::sync::Arc;
use tracing::info;

/// 高级特性集成管理器
/// 
/// 这个管理器将所有高级特性与事件系统整合在一起，
/// 提供统一的 API 接口，并自动处理事件发布。
pub struct AdvancedFeaturesIntegration {
    /// 高级特性管理器
    features_manager: Arc<AdvancedFeaturesManager>,
    /// 事件管理器
    event_manager: Arc<EventManager>,
    /// 消息状态管理器
    state_manager: Arc<MessageStateManager>,
}

impl AdvancedFeaturesIntegration {
    /// 创建新的高级特性集成管理器
    pub fn new(
        features_manager: Arc<AdvancedFeaturesManager>,
        event_manager: Arc<EventManager>,
        state_manager: Arc<MessageStateManager>,
    ) -> Self {
        Self {
            features_manager,
            event_manager,
            state_manager,
        }
    }

    // ========== 已读回执功能 ==========

    /// 标记消息为已读（带事件发布）
    pub async fn mark_message_as_read(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        reader_uid: u64,
    ) -> Result<()> {
        // 调用底层功能
        let receipt_event = self.features_manager.mark_message_as_read(
            message_id, channel_id, channel_type, reader_uid
        )?;

        // 发布已读回执事件
        self.event_manager.emit(SDKEvent::ReadReceiptReceived(receipt_event)).await;

        // 获取消息序列号，用于更新会话已读状态
        let message_seq = {
            let conn = (*self.features_manager).get_db_connection();
            conn.query_row(
                "SELECT message_seq FROM messages WHERE message_id = ?1",
                rusqlite::params![message_id as i64],
                |row| row.get::<_, i64>(0)
            ).unwrap_or(0)
        };

        // 更新会话已读状态
        if message_seq > 0 {
            self.features_manager.update_channel_read_state(
                channel_id, 
                channel_type, 
                reader_uid, 
                Some(message_id), 
                message_seq
            )?;
        }

        // 更新未读数并发布事件
        let unread_count = self.features_manager.get_unread_count(
            channel_id, channel_type, reader_uid
        )?;

        let unread_event = event_builders::unread_count_changed(
            channel_id,
            channel_type,
            unread_count,
        );
        self.event_manager.emit(unread_event).await;

        info!("Message {} marked as read by {} with events", message_id, reader_uid);
        Ok(())
    }

    /// 批量标记消息为已读（带事件发布）
    pub async fn batch_mark_messages_as_read(
        &self,
        message_ids: &[u64],
        channel_id: u64,
        channel_type: i32,
        reader_uid: u64,
    ) -> Result<()> {
        // 调用底层功能
        let receipt_events = self.features_manager.batch_mark_messages_as_read(
            message_ids, channel_id, channel_type, reader_uid
        )?;

        // 发布所有已读回执事件
        for receipt_event in receipt_events {
            self.event_manager.emit(SDKEvent::ReadReceiptReceived(receipt_event)).await;
        }

        // 更新未读数并发布事件
        let unread_count = self.features_manager.get_unread_count(
            channel_id, channel_type, reader_uid
        )?;

        let unread_event = event_builders::unread_count_changed(
            channel_id,
            channel_type,
            unread_count,
        );
        self.event_manager.emit(unread_event).await;

        info!("Batch marked {} messages as read by {} with events", message_ids.len(), reader_uid);
        Ok(())
    }

    /// 更新会话已读状态（带事件发布）
    pub async fn update_channel_read_state(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        last_read_message_id: Option<u64>,
        last_read_seq: i64,
    ) -> Result<()> {
        // 调用底层功能
        let read_state = self.features_manager.update_channel_read_state(
            channel_id, channel_type, user_id, last_read_message_id, last_read_seq
        )?;

        // 发布未读数变更事件
        let unread_event = event_builders::unread_count_changed(
            channel_id,
            channel_type,
            read_state.unread_count,
        );
        self.event_manager.emit(unread_event).await;

        info!("Updated channel read state for user {} in channel {}", user_id, channel_id);
        Ok(())
    }

    // ========== 消息撤回功能 ==========

    /// 撤回消息（带事件发布）
    pub async fn revoke_message(
        &self,
        message_id: u64,
        revoker_uid: u64,
        reason: Option<&str>,
    ) -> Result<()> {
        // 检查撤回权限
        if !self.features_manager.can_revoke_message(message_id, revoker_uid)? {
            return Err(PrivchatSDKError::InvalidOperation(
                "Cannot revoke this message".to_string()
            ));
        }

        // 调用底层功能
        let revoke_event = self.features_manager.revoke_message(
            message_id, revoker_uid, reason
        )?;

        // 发布消息撤回事件
        self.event_manager.emit(SDKEvent::MessageRevoked(revoke_event)).await;

        info!("Message {} revoked by {} with events", message_id, revoker_uid);
        Ok(())
    }

    /// 检查消息是否可以撤回
    pub async fn can_revoke_message(&self, message_id: u64, user_id: u64) -> Result<bool> {
        self.features_manager.can_revoke_message(message_id, user_id)
    }

    // ========== 消息编辑功能 ==========

    /// 编辑消息（带事件发布）
    pub async fn edit_message(
        &self,
        message_id: u64,
        editor_uid: u64,
        new_content: &str,
    ) -> Result<()> {
        // 检查编辑权限
        if !self.features_manager.can_edit_message(message_id, editor_uid)? {
            return Err(PrivchatSDKError::InvalidOperation(
                "Cannot edit this message".to_string()
            ));
        }

        // 调用底层功能
        let edit_event = self.features_manager.edit_message(
            message_id, editor_uid, new_content
        )?;

        // 发布消息编辑事件
        self.event_manager.emit(SDKEvent::MessageEdited(edit_event)).await;

        info!("Message {} edited by {} with events", message_id, editor_uid);
        Ok(())
    }

    /// 检查消息是否可以编辑
    pub async fn can_edit_message(&self, message_id: u64, user_id: u64) -> Result<bool> {
        self.features_manager.can_edit_message(message_id, user_id)
    }

    /// 获取消息编辑历史
    pub async fn get_message_edit_history(&self, message_id: u64) -> Result<Vec<MessageEditEvent>> {
        self.features_manager.get_message_edit_history(message_id)
    }

    // ========== Typing Indicator 功能 ==========

    /// 发送正在输入指示器
    pub async fn send_typing_indicator(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        is_typing: bool,
    ) -> Result<()> {
        // 创建正在输入事件
        let typing_event = event_builders::typing_indicator(
            channel_id,
            channel_type,
            user_id,
            is_typing,
        );

        // 发布事件
        self.event_manager.emit(typing_event).await;

        info!("Typing indicator sent for user {} in channel {}: {}", user_id, channel_id, is_typing);
        Ok(())
    }

    // ========== 用户在线状态功能 ==========

    /// 更新用户在线状态
    pub async fn update_user_presence(
        &self,
        user_id: u64,
        is_online: bool,
        last_seen: Option<u64>,
    ) -> Result<()> {
        // 创建在线状态变更事件
        let presence_event = event_builders::user_presence_changed(
            user_id,
            is_online,
            last_seen,
        );

        // 发布事件
        self.event_manager.emit(presence_event).await;

        info!("User presence updated for {}: online={}", user_id, is_online);
        Ok(())
    }

    // ========== 连接状态管理 ==========

    /// 更新连接状态
    pub async fn update_connection_state(&self, is_connected: bool, reason: &str) -> Result<()> {
        // 创建连接状态变更事件
        let old_state = if is_connected { ConnectionState::Disconnected } else { ConnectionState::Connected };
        let new_state = if is_connected { ConnectionState::Connected } else { ConnectionState::Disconnected };
        let connection_event = event_builders::connection_state_changed(
            old_state,
            new_state,
        );

        // 发布事件
        self.event_manager.emit(connection_event).await;

        info!("Connection state updated: connected={}, reason={}", is_connected, reason);
        Ok(())
    }

    // ========== 消息生命周期事件 ==========

    /// 发布消息接收事件
    pub async fn emit_message_received(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
    ) -> Result<()> {
        let event = event_builders::message_received(
            message_id,
            channel_id,
            channel_type,
            from_uid,
        );

        self.event_manager.emit(event).await;
        Ok(())
    }

    /// 发布消息发送成功事件
    pub async fn emit_message_sent(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<()> {
        let event = event_builders::message_sent(
            message_id,
            channel_id,
            channel_type,
        );

        self.event_manager.emit(event).await;
        Ok(())
    }

    /// 发布消息发送失败事件
    pub async fn emit_message_send_failed(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        error: &str,
    ) -> Result<()> {
        let event = event_builders::message_send_failed(
            message_id,
            channel_id,
            channel_type,
            error.to_string(),
        );

        self.event_manager.emit(event).await;
        Ok(())
    }

    // ========== 查询和统计功能 ==========

    /// 获取消息的已读回执列表
    pub async fn get_message_read_receipts(&self, message_id: u64) -> Result<Vec<ReadReceiptEvent>> {
        self.features_manager.get_message_read_receipts(message_id)
    }
    
    /// 获取会话的未读消息数量
    pub async fn get_unread_count(&self, channel_id: u64, channel_type: i32, user_id: u64) -> Result<i32> {
        self.features_manager.get_unread_count(channel_id, channel_type, user_id)
    }
    
    /// 获取总未读消息数（所有会话未读数的和）
    /// 
    /// 这是 Telegram、微信等主流 IM SDK 的标准设计：
    /// - 总未读数 = 所有会话未读数的和
    /// - 默认排除免打扰的会话（与主流 SDK 一致）
    /// 
    /// # 参数
    /// - `exclude_muted`: 是否排除免打扰的会话（默认 true）
    /// 
    /// # 返回
    /// 总未读消息数
    /// 
    /// # 示例
    /// ```rust
    /// // 获取总未读数（排除免打扰）
    /// let total = integration.get_total_unread_count(true).await?;
    /// 
    /// // 获取总未读数（包括免打扰）
    /// let total_all = integration.get_total_unread_count(false).await?;
    /// ```
    pub async fn get_total_unread_count(&self, exclude_muted: bool) -> Result<i32> {
        // 通过 features_manager 获取连接
        let conn = self.features_manager.get_connection();
        let conn_guard = conn.lock().unwrap();
        let channel_dao = crate::storage::dao::ChannelDao::new(&*conn_guard);
        
        if exclude_muted {
            channel_dao.get_total_unread_count_exclude_muted()
        } else {
            channel_dao.get_total_unread_count()
        }
    }
    
    /// 验证总未读数是否等于会话列表未读数的和
    /// 
    /// 这是一个验证方法，用于确保数据一致性
    /// 
    /// # 返回
    /// (总未读数, 会话列表未读数和, 是否一致)
    pub async fn verify_total_unread_count(&self, exclude_muted: bool) -> Result<(i32, i32, bool)> {
        let conn = self.features_manager.get_connection();
        let conn_guard = conn.lock().unwrap();
        let channel_dao = crate::storage::dao::ChannelDao::new(&*conn_guard);
        
        // 获取总未读数
        let total = if exclude_muted {
            channel_dao.get_total_unread_count_exclude_muted()?
        } else {
            channel_dao.get_total_unread_count()?
        };
        
        // 获取所有会话并计算和
        let channels = channel_dao.list_all()?;
        let sum: i32 = if exclude_muted {
            channels.iter()
                .filter(|conv| {
                    // 检查是否免打扰
                    if let Ok(extra) = serde_json::from_str::<serde_json::Value>(&conv.extra) {
                        !extra.get("muted").and_then(|v| v.as_bool()).unwrap_or(false)
                    } else {
                        true
                    }
                })
                .map(|conv| conv.unread_count)
                .sum()
        } else {
            channels.iter()
                .map(|conv| conv.unread_count)
                .sum()
        };
        
        let is_consistent = total == sum;
        
        Ok((total, sum, is_consistent))
    }

    /// 获取会话的所有已读状态
    pub async fn get_channel_read_states(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Vec<crate::storage::advanced_features::ChannelReadState>> {
        self.features_manager.get_channel_read_states(channel_id, channel_type)
    }

    /// 获取事件统计
    pub async fn get_event_stats(&self) -> crate::events::EventStats {
        self.event_manager.get_stats().await
    }

    /// 获取活跃事件订阅者数量
    pub fn get_subscriber_count(&self) -> usize {
        self.event_manager.subscriber_count()
    }

    // ========== 清理和维护功能 ==========

    /// 清理过期的已读回执记录
    pub async fn cleanup_old_read_receipts(&self, days_to_keep: u32) -> Result<usize> {
        self.features_manager.cleanup_old_read_receipts(days_to_keep)
    }

    /// 清理过期的编辑历史记录
    pub async fn cleanup_old_edit_history(&self, days_to_keep: u32) -> Result<usize> {
        self.features_manager.cleanup_old_edit_history(days_to_keep)
    }

    /// 执行定期清理任务
    pub async fn perform_maintenance(&self) -> Result<()> {
        info!("Starting advanced features maintenance");

        // 清理30天前的已读回执
        let receipts_cleaned = self.cleanup_old_read_receipts(30).await?;
        
        // 清理7天前的编辑历史
        let history_cleaned = self.cleanup_old_edit_history(7).await?;

        info!(
            "Maintenance completed: {} read receipts cleaned, {} edit history records cleaned",
            receipts_cleaned, history_cleaned
        );

        Ok(())
    }
}

impl Clone for AdvancedFeaturesIntegration {
    fn clone(&self) -> Self {
        Self {
            features_manager: self.features_manager.clone(),
            event_manager: self.event_manager.clone(),
            state_manager: self.state_manager.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::advanced_features::AdvancedFeaturesManager;
    use crate::storage::message_state::MessageStateManager;
    use crate::events::EventManager;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn create_test_integration() -> (AdvancedFeaturesIntegration, NamedTempFile) {
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
                searchable_word TEXT,
                content_edit TEXT,
                edited_at INTEGER,
                extra_version INTEGER
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

        // 为MessageStateManager创建同一个数据库的连接
        let state_manager_conn = Connection::open(&temp_file.path()).unwrap();
        let _ = state_manager_conn.execute("PRAGMA journal_mode=WAL", []);
        let _ = state_manager_conn.execute("PRAGMA synchronous=NORMAL", []);
        
        let state_manager = Arc::new(MessageStateManager::new(state_manager_conn));
        
        let features_manager = Arc::new(AdvancedFeaturesManager::new(
            conn,
            state_manager.clone(),
            120, // 2分钟撤回限制
            300, // 5分钟编辑限制
        ));
        
        features_manager.initialize_tables().unwrap();
        
        let event_manager = Arc::new(EventManager::new(100));
        
        let integration = AdvancedFeaturesIntegration::new(
            features_manager,
            event_manager,
            state_manager,
        );
        
        (integration, temp_file)
    }

    #[tokio::test]
    async fn test_integrated_read_receipt_with_events() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 插入测试消息
        {
            let conn = integration.features_manager.get_connection();
            let conn_guard = conn.lock().unwrap();
            conn_guard.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 标记消息为已读
        integration.mark_message_as_read("msg1", "channel1", 1, "user2").await.unwrap();
        
        // 检查是否收到已读回执事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::ReadReceiptReceived(receipt) => {
                assert_eq!(receipt.message_id, "msg1");
                assert_eq!(receipt.reader_uid, "user2");
            }
            _ => panic!("Expected ReadReceiptReceived event"),
        }

        // 检查是否收到未读数变更事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::UnreadCountChanged { channel_id, unread_count, .. } => {
                assert_eq!(channel_id, "channel1");
                assert_eq!(unread_count, 0);
            }
            _ => panic!("Expected UnreadCountChanged event"),
        }
    }

    #[tokio::test]
    async fn test_integrated_message_revoke_with_events() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 插入测试消息
        {
            let conn = integration.features_manager.get_connection();
            let conn_guard = conn.lock().unwrap();
            conn_guard.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, status, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 2, 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 撤回消息
        integration.revoke_message("msg1", "user1", Some("Test revoke")).await.unwrap();
        
        // 检查是否收到撤回事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::MessageRevoked(revoke) => {
                assert_eq!(revoke.message_id, "msg1");
                assert_eq!(revoke.revoker_uid, "user1");
                assert_eq!(revoke.reason, Some("Test revoke".to_string()));
            }
            _ => panic!("Expected MessageRevoked event"),
        }
    }

    #[tokio::test]
    async fn test_integrated_message_edit_with_events() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 插入测试消息
        {
            let conn = integration.features_manager.get_connection();
            let conn_guard = conn.lock().unwrap();
            conn_guard.execute(
                "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, status, message_seq) 
                 VALUES ('msg1', 'channel1', 1, 'user1', 'Hello', 2, 1)",
                [],
            ).unwrap();
        } // 这里连接的借用结束

        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 编辑消息
        integration.edit_message("msg1", "user1", "Hello World!").await.unwrap();
        
        // 检查是否收到编辑事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::MessageEdited(edit) => {
                assert_eq!(edit.message_id, "msg1");
                assert_eq!(edit.editor_uid, "user1");
                assert_eq!(edit.new_content, "Hello World!");
            }
            _ => panic!("Expected MessageEdited event"),
        }
    }

    #[tokio::test]
    async fn test_typing_indicator() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 发送正在输入指示器
        integration.send_typing_indicator("channel1", 1, "user1", true).await.unwrap();
        
        // 检查是否收到正在输入事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::TypingIndicator(typing_event) => {
                assert_eq!(typing_event.channel_id, "channel1");
                assert_eq!(typing_event.user_id, "user1");
                assert!(typing_event.is_typing);
            }
            _ => panic!("Expected TypingIndicator event"),
        }
    }

    #[tokio::test]
    async fn test_user_presence_update() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 更新用户在线状态
        integration.update_user_presence("user1", true, None).await.unwrap();
        
        // 检查是否收到在线状态变更事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::UserPresenceChanged { user_id, is_online, .. } => {
                assert_eq!(user_id, "user1");
                assert!(is_online);
            }
            _ => panic!("Expected UserPresenceChanged event"),
        }
    }

    #[tokio::test]
    async fn test_connection_state_update() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 更新连接状态
        integration.update_connection_state(false, "Network error").await.unwrap();
        
        // 检查是否收到连接状态变更事件
        let event = receiver.recv().await.unwrap();
        match event {
            SDKEvent::ConnectionStateChanged { new_state, .. } => {
                assert_eq!(new_state, crate::events::ConnectionState::Disconnected);
            }
            _ => panic!("Expected ConnectionStateChanged event"),
        }
    }

    #[tokio::test]
    async fn test_batch_operations_with_events() {
        let (integration, _temp_file) = create_test_integration().await;
        
        // 插入测试消息
        {
            let conn = integration.features_manager.get_connection();
            let conn_guard = conn.lock().unwrap();
            for i in 1..=3 {
                conn_guard.execute(
                    "INSERT INTO messages (message_id, channel_id, channel_type, from_uid, content, message_seq) 
                     VALUES (?, 'channel1', 1, 'user1', 'Hello', ?)",
                    rusqlite::params![format!("msg{}", i), i],
                ).unwrap();
            }
        } // 这里连接的借用结束

        // 订阅事件
        let mut receiver = integration.event_manager.subscribe().await;
        
        // 批量标记为已读
        let message_ids = vec!["msg1".to_string(), "msg2".to_string(), "msg3".to_string()];
        integration.batch_mark_messages_as_read(&message_ids, "channel1", 1, "user2").await.unwrap();
        
        // 应该收到3个已读回执事件 + 1个未读数变更事件
        let mut read_receipt_count = 0;
        let mut unread_count_changed = false;
        
        for _ in 0..4 {
            let event = receiver.recv().await.unwrap();
            match event {
                SDKEvent::ReadReceiptReceived(_) => read_receipt_count += 1,
                SDKEvent::UnreadCountChanged { .. } => unread_count_changed = true,
                _ => {}
            }
        }
        
        assert_eq!(read_receipt_count, 3);
        assert!(unread_count_changed);
    }
} 