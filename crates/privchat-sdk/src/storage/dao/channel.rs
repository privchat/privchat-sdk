//! 频道数据访问层 - 管理频道/群组信息

use rusqlite::{Connection, params, Row};
use crate::error::Result;
use crate::storage::entities::Channel;

pub struct ChannelDao<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    pub fn get_by_id(&self, channel_id: &str, channel_type: i32) -> Result<Option<Channel>> {
        let sql = "SELECT * FROM channel WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![channel_id, channel_type], |row| {
            Ok(self.row_to_channel(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(channel)) => Ok(Some(channel)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!("查询频道失败: {}", e))),
            None => Ok(None),
        }
    }
    
    fn row_to_channel(&self, row: &Row) -> rusqlite::Result<Channel> {
        Ok(Channel {
            id: row.get("id")?,
            channel_id: row.get("channel_id")?,
            channel_type: row.get("channel_type")?,
            show_nick: row.get("show_nick")?,
            username: row.get("username")?,
            channel_name: row.get("channel_name")?,
            channel_remark: row.get("channel_remark")?,
            top: row.get("top")?,
            mute: row.get("mute")?,
            save: row.get("save")?,
            forbidden: row.get("forbidden")?,
            follow: row.get("follow")?,
            is_deleted: row.get("is_deleted")?,
            receipt: row.get("receipt")?,
            status: row.get("status")?,
            invite: row.get("invite")?,
            robot: row.get("robot")?,
            version: row.get("version")?,
            online: row.get("online")?,
            last_offline: row.get("last_offline")?,
            avatar: row.get("avatar")?,
            category: row.get("category")?,
            extra: row.get("extra")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            avatar_cache_key: row.get("avatar_cache_key")?,
            remote_extra: row.get("remote_extra")?,
            flame: row.get("flame")?,
            flame_second: row.get("flame_second")?,
            device_flag: row.get("device_flag")?,
            parent_channel_id: row.get("parent_channel_id")?,
            parent_channel_type: row.get("parent_channel_type")?,
        })
    }
}
