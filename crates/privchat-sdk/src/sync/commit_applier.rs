/// Commit 应用器
/// 
/// 职责：
/// - 将服务器的 ServerCommit 应用到本地数据库
/// - 处理不同类型的 Commit（消息、删除、编辑、撤回等）
/// - 触发 UI 更新事件

use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::error::{PrivchatSDKError, Result};
use crate::storage::StorageManager;
use crate::storage::entities::Message;
use crate::events::{EventManager, SDKEvent};
use privchat_protocol::rpc::sync::ServerCommit;

/// Commit 应用器
pub struct CommitApplier {
    /// 存储管理器
    storage: Arc<StorageManager>,
    
    /// 事件管理器（可选，用于触发 UI 更新）
    event_manager: Option<Arc<EventManager>>,
}

impl CommitApplier {
    /// 创建 Commit 应用器
    pub fn new(storage: Arc<StorageManager>, event_manager: Option<Arc<EventManager>>) -> Self {
        Self {
            storage,
            event_manager,
        }
    }
    
    /// 批量应用 Commits
    /// 
    /// Commits 必须按 pts 递增顺序
    pub async fn apply_commits(&self, commits: &[ServerCommit]) -> Result<()> {
        debug!("开始应用 {} 条 commits", commits.len());
        
        for commit in commits {
            if let Err(e) = self.apply_single_commit(commit).await {
                error!("应用 commit 失败: pts={}, error={:?}", commit.pts, e);
                // 继续应用其他 commits（容错）
            }
        }
        
        Ok(())
    }
    
    /// 应用单条 Commit
    async fn apply_single_commit(&self, commit: &ServerCommit) -> Result<()> {
        debug!("应用 commit: pts={}, message_type={}", commit.pts, commit.message_type);
        
        match commit.message_type.as_str() {
            "text" | "image" | "video" | "audio" | "file" => {
                self.apply_message_commit(commit).await?;
            }
            "revoke" => {
                self.apply_revoke_commit(commit).await?;
            }
            "delete" => {
                self.apply_delete_commit(commit).await?;
            }
            "edit" => {
                self.apply_edit_commit(commit).await?;
            }
            "reaction" => {
                self.apply_reaction_commit(commit).await?;
            }
            _ => {
                warn!("未知的 message_type: {}", commit.message_type);
            }
        }
        
        Ok(())
    }
    
    // ============================================================
    // 不同类型 Commit 的处理
    // ============================================================
    
    /// 应用消息 Commit
    async fn apply_message_commit(&self, commit: &ServerCommit) -> Result<()> {
        // 解析消息内容
        let message = self.parse_message_from_commit(commit)?;
        
        // 保存到数据库
        self.storage.save_message(&message).await?;
        
        // 触发事件
        if let Some(event_manager) = &self.event_manager {
            event_manager.emit(SDKEvent::MessageReceived {
                server_message_id: commit.server_msg_id,
                channel_id: commit.channel_id,
                channel_type: commit.channel_type as i32,
                from_uid: message.from_uid,
                timestamp: message.created_at as u64,
                content: message.content.clone(), // ✅ 添加消息内容
            }).await;
        }
        
        debug!("消息已保存: server_msg_id={}, pts={}", commit.server_msg_id, commit.pts);
        Ok(())
    }
    
    /// 应用撤回 Commit
    async fn apply_revoke_commit(&self, commit: &ServerCommit) -> Result<()> {
        // 从 payload 中提取被撤回的消息 ID
        let revoked_msg_id = commit.content.get("revoked_message_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| PrivchatSDKError::InvalidData("撤回 commit 缺少 revoked_message_id".to_string()))?;
        
        // 协议中 revoked_message_id 视为 message.id
        self.storage.revoke_message(revoked_msg_id as i64).await?;
        
        // 触发事件
        if let Some(event_manager) = &self.event_manager {
            use crate::storage::advanced_features::MessageRevokeEvent;
            event_manager.emit(SDKEvent::MessageRevoked(MessageRevokeEvent {
                message_id: revoked_msg_id,
                channel_id: commit.channel_id,
                channel_type: commit.channel_type as i32,
                revoker_uid: commit.sender_id,
                revoked_at: commit.server_timestamp as u64,
                reason: None, // 撤回原因（可选）
            })).await;
        }
        
        debug!("消息已撤回: revoked_msg_id={}, pts={}", revoked_msg_id, commit.pts);
        Ok(())
    }
    
    /// 应用删除 Commit
    async fn apply_delete_commit(&self, commit: &ServerCommit) -> Result<()> {
        // 从 payload 中提取被删除的消息 ID
        let deleted_msg_id = commit.content.get("deleted_message_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| PrivchatSDKError::InvalidData("删除 commit 缺少 deleted_message_id".to_string()))?;
        
        self.storage.delete_message(deleted_msg_id as i64).await?;
        
        // 触发事件（删除操作暂不发送事件，或者可以用MessageRevoked代替）
        // if let Some(event_manager) = &self.event_manager {
        //     // TODO: 添加 MessageDeleted 事件到 SDKEvent
        // }
        
        debug!("消息已删除: deleted_msg_id={}, pts={}", deleted_msg_id, commit.pts);
        Ok(())
    }
    
    /// 应用编辑 Commit
    async fn apply_edit_commit(&self, commit: &ServerCommit) -> Result<()> {
        // 从 payload 中提取编辑信息
        let edited_msg_id = commit.content.get("edited_message_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| PrivchatSDKError::InvalidData("编辑 commit 缺少 edited_message_id".to_string()))?;
        
        let new_content = commit.content.get("new_content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PrivchatSDKError::InvalidData("编辑 commit 缺少 new_content".to_string()))?
            .to_string();
        
        self.storage.update_message_content(edited_msg_id as i64, &new_content).await?;
        
        // 触发事件
        if let Some(event_manager) = &self.event_manager {
            use crate::storage::advanced_features::MessageEditEvent;
            event_manager.emit(SDKEvent::MessageEdited(MessageEditEvent {
                message_id: edited_msg_id,
                channel_id: commit.channel_id,
                channel_type: commit.channel_type as i32,
                editor_uid: commit.sender_id,
                new_content: new_content.clone(),
                edited_at: commit.server_timestamp as u64,
                edit_version: 1, // TODO: 从 commit 中获取版本号
            })).await;
        }
        
        debug!("消息已编辑: edited_msg_id={}, pts={}", edited_msg_id, commit.pts);
        Ok(())
    }
    
    /// 应用反应 Commit
    async fn apply_reaction_commit(&self, commit: &ServerCommit) -> Result<()> {
        // 从 payload 中提取反应信息
        let message_id = commit.content.get("message_id")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| PrivchatSDKError::InvalidData("反应 commit 缺少 message_id".to_string()))?;
        
        let reaction = commit.content.get("reaction")
            .and_then(|v| v.as_str())
            .ok_or_else(|| PrivchatSDKError::InvalidData("反应 commit 缺少 reaction".to_string()))?
            .to_string();
        
        self.storage.add_message_reaction(message_id as i64, commit.sender_id, &reaction).await?;
        
        // 触发事件
        if let Some(event_manager) = &self.event_manager {
            use crate::storage::reaction::{ReactionEvent, ReactionAction};
            event_manager.emit(SDKEvent::ReactionAdded(ReactionEvent {
                message_id,
                channel_id: commit.channel_id,
                channel_type: commit.channel_type as i32,
                user_id: commit.sender_id,
                emoji: reaction.clone(),
                action: ReactionAction::Add,
                timestamp: commit.server_timestamp as u64,
            })).await;
        }
        
        debug!("反应已添加: message_id={}, pts={}", message_id, commit.pts);
        Ok(())
    }
    
    // ============================================================
    // 辅助方法
    // ============================================================
    
    /// 从 ServerCommit 解析 Message
    fn parse_message_from_commit(&self, commit: &ServerCommit) -> Result<Message> {
        // 提取消息内容
        let content = commit.content.get("text")
            .or_else(|| commit.content.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        
        // 提取 extra（可选）
        let extra = commit.content.get("extra")
            .map(|v| v.to_string());
        
        // 构造 Message
        let message = Message {
            id: None, // 自增，数据库会分配
            server_message_id: Some(commit.server_msg_id),
            pts: commit.pts as i64,
            channel_id: commit.channel_id,
            channel_type: commit.channel_type as i32,
            timestamp: Some(commit.server_timestamp),
            from_uid: commit.sender_id,
            message_type: commit.message_type.parse().unwrap_or(1),
            content,
            status: 2, // 2 = 已送达
            voice_status: 0,
            created_at: commit.server_timestamp,
            updated_at: commit.server_timestamp,
            searchable_word: String::new(),
            local_message_id: 0,
            is_deleted: 0,
            setting: 0,
            order_seq: commit.pts as i64,
            extra: extra.unwrap_or_default(),
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: String::new(),
            expire_time: None,
            expire_timestamp: None,
            revoked: 0,
            revoked_at: 0,
            revoked_by: None,
        };
        
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    #[ignore]
    async fn test_commit_applier() {
        // TODO: 添加单元测试
    }
}
