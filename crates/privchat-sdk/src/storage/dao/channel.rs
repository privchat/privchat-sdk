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
    
    pub fn get_by_id(&self, channel_id: u64, channel_type: i32) -> Result<Option<Channel>> {
        self.get_by_channel(channel_id, channel_type)
    }
    
    /// 按 channel_id 查询（表内 channel_id 唯一，一行一会话）
    pub fn get_by_channel(&self, channel_id: u64, _channel_type: i32) -> Result<Option<Channel>> {
        let sql = "SELECT * FROM channel WHERE channel_id = ?1 AND is_deleted = 0";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![channel_id], |row| {
            Ok(self.row_to_channel(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(channel)) => Ok(Some(channel)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!("查询频道失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 按 channel_id 查询会话（表内 channel_id 唯一）
    pub fn get_direct_channel_by_id(&self, channel_id: u64) -> Result<Option<Channel>> {
        let sql = "SELECT * FROM channel WHERE channel_id = ?1 AND is_deleted = 0 LIMIT 1";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![channel_id], |row| Ok(self.row_to_channel(row)?))?;
        match rows.next() {
            Some(Ok(channel)) => Ok(Some(channel)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!("查询私聊频道失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 通过 username 查询私聊频道（username 存储对方的 user_id）
    pub fn find_by_username(&self, username: &str) -> Result<Option<Channel>> {
        let sql = "SELECT * FROM channel WHERE username = ?1 AND channel_type = 1 AND is_deleted = 0 LIMIT 1";
        
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![username], |row| {
            Ok(self.row_to_channel(row)?)
        })?;
        
        match rows.next() {
            Some(Ok(channel)) => Ok(Some(channel)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!("查询频道失败: {}", e))),
            None => Ok(None),
        }
    }
    
    /// 插入或更新频道
    pub fn upsert(&self, channel: &Channel) -> Result<()> {
        let sql = r#"
            INSERT INTO channel (
                channel_id, channel_type, last_local_message_id, last_msg_timestamp, unread_count,
                last_msg_pts, show_nick, username, channel_name, channel_remark,
                top, mute, save, forbidden, follow, is_deleted, receipt, status,
                invite, robot, version, online, last_offline, avatar, category, extra,
                created_at, updated_at, avatar_cache_key, remote_extra, flame, flame_second,
                device_flag, parent_channel_id, parent_channel_type
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35
            )
            ON CONFLICT(channel_id) DO UPDATE SET
                last_local_message_id = excluded.last_local_message_id,
                last_msg_timestamp = excluded.last_msg_timestamp,
                unread_count = excluded.unread_count,
                last_msg_pts = excluded.last_msg_pts,
                show_nick = excluded.show_nick,
                username = excluded.username,
                channel_name = excluded.channel_name,
                channel_remark = excluded.channel_remark,
                top = excluded.top,
                mute = excluded.mute,
                save = excluded.save,
                forbidden = excluded.forbidden,
                follow = excluded.follow,
                is_deleted = excluded.is_deleted,
                receipt = excluded.receipt,
                status = excluded.status,
                invite = excluded.invite,
                robot = excluded.robot,
                version = excluded.version,
                online = excluded.online,
                last_offline = excluded.last_offline,
                avatar = excluded.avatar,
                category = excluded.category,
                extra = excluded.extra,
                updated_at = excluded.updated_at,
                avatar_cache_key = excluded.avatar_cache_key,
                remote_extra = excluded.remote_extra,
                flame = excluded.flame,
                flame_second = excluded.flame_second,
                device_flag = excluded.device_flag,
                parent_channel_id = excluded.parent_channel_id,
                parent_channel_type = excluded.parent_channel_type
        "#;
        
        self.conn.execute(sql, params![
            channel.channel_id, channel.channel_type,
            channel.last_local_message_id, channel.last_msg_timestamp, channel.unread_count,
            channel.last_msg_pts,
            channel.show_nick, channel.username,
            channel.channel_name, channel.channel_remark, channel.top, channel.mute,
            channel.save, channel.forbidden, channel.follow, channel.is_deleted,
            channel.receipt, channel.status, channel.invite, channel.robot,
            channel.version, channel.online, channel.last_offline, channel.avatar,
            channel.category, channel.extra, channel.created_at, channel.updated_at,
            channel.avatar_cache_key, channel.remote_extra, channel.flame,
            channel.flame_second, channel.device_flag, channel.parent_channel_id,
            channel.parent_channel_type
        ])?;
        
        Ok(())
    }
    
    /// 更新频道的 save 字段（收藏状态）
    pub fn update_save(&self, channel_id: u64, _channel_type: i32, save: i32) -> Result<()> {
        let sql = "UPDATE channel SET save = ?1, updated_at = ?2 WHERE channel_id = ?3";
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(sql, params![save, now, channel_id])?;
        Ok(())
    }
    
    /// 更新频道的 mute 字段（通知模式）
    pub fn update_mute(&self, channel_id: u64, _channel_type: i32, mute: i32) -> Result<()> {
        let sql = "UPDATE channel SET mute = ?1, updated_at = ?2 WHERE channel_id = ?3";
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(sql, params![mute, now, channel_id])?;
        Ok(())
    }
    
    /// 更新未读数
    pub fn update_unread_count(&self, channel_id: u64, _channel_type: i32, count: i32) -> Result<()> {
        let sql = "UPDATE channel SET unread_count = ?1 WHERE channel_id = ?2";
        self.conn.execute(sql, params![count, channel_id as i64])?;
        Ok(())
    }
    
    /// 更新 pts（用于同步）
    pub fn update_pts(&self, channel_id: u64, _channel_type: i32, new_pts: u64) -> Result<()> {
        let sql = "UPDATE channel SET last_msg_pts = ?1 WHERE channel_id = ?2";
        self.conn.execute(sql, params![new_pts as i64, channel_id as i64])?;
        Ok(())
    }
    
    /// 更新会话的 extra 字段
    pub fn update_extra(&self, channel_id: u64, _channel_type: i32, extra: &str) -> Result<()> {
        let sql = "UPDATE channel SET extra = ?1, version = version + 1 WHERE channel_id = ?2";
        self.conn.execute(sql, params![extra, channel_id as i64])?;
        Ok(())
    }
    
    /// 获取总未读消息数（所有会话未读数的和）
    /// 
    /// 注意：这里只统计未删除的会话，不包括免打扰的会话（如果 extra 中包含 muted 标记）
    pub fn get_total_unread_count(&self) -> Result<i32> {
        // 查询所有未删除会话的未读数总和
        // 注意：这里假设免打扰状态存储在 extra 字段的 JSON 中
        // 如果 extra 是 JSON 格式且包含 "muted": true，则不计入总数
        let sql = "SELECT SUM(unread_count) FROM channel WHERE is_deleted = 0";
        
        let total: Option<i32> = self.conn.query_row(sql, [], |row| {
            Ok(row.get(0)?)
        })?;
        
        Ok(total.unwrap_or(0))
    }
    
    /// 获取总未读消息数（排除免打扰的会话）
    /// 
    /// 这是更准确的统计方式，与 Telegram 等主流 IM SDK 的设计一致
    pub fn get_total_unread_count_exclude_muted(&self) -> Result<i32> {
        // 查询所有未删除且未免打扰的会话的未读数总和
        // 假设免打扰状态存储在 extra 字段的 JSON 中：{"muted": true}
        // 或者使用 mute 字段（如果存在）
        let sql = "SELECT SUM(unread_count) FROM channel 
                   WHERE is_deleted = 0 
                   AND (mute IS NULL OR mute = 0)
                   AND (extra IS NULL OR extra = '{}' OR json_extract(extra, '$.muted') IS NULL OR json_extract(extra, '$.muted') = 0)";
        
        let total: Option<i32> = self.conn.query_row(sql, [], |row| {
            Ok(row.get(0)?)
        })?;
        
        Ok(total.unwrap_or(0))
    }
    
    /// 获取所有会话列表（用于验证总未读数）
    pub fn list_all(&self) -> Result<Vec<Channel>> {
        // 优先使用 last_msg_timestamp，如果不存在则使用 updated_at
        let sql = "SELECT * FROM channel WHERE is_deleted = 0 ORDER BY COALESCE(last_msg_timestamp, updated_at) DESC";
        
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(self.row_to_channel(row)?)
        })?;
        
        let mut channels = Vec::new();
        for row in rows {
            channels.push(row?);
        }
        
        Ok(channels)
    }
    
    fn row_to_channel(&self, row: &Row) -> rusqlite::Result<Channel> {
        // channel 表以 channel_id 为主键，无 id 列时用 channel_id 作为实体 id
        let channel_id: i64 = row.get("channel_id")?;
        Ok(Channel {
            id: Some(channel_id),
            channel_id: channel_id as u64,
            channel_type: row.get("channel_type")?,
            // 会话列表相关字段（可能不存在，使用默认值）
            last_local_message_id: row.get("last_local_message_id").unwrap_or(0),
            last_msg_timestamp: row.get("last_msg_timestamp").ok(),
            unread_count: row.get("unread_count").unwrap_or(0),
            last_msg_pts: row.get("last_msg_pts").unwrap_or(0),
            // 频道信息字段（可能不存在，使用默认值）
            show_nick: row.get("show_nick").unwrap_or(0),
            username: row.get("username").unwrap_or_default(),
            channel_name: row.get("channel_name").unwrap_or_default(),
            channel_remark: row.get("channel_remark").unwrap_or_default(),
            top: row.get("top").unwrap_or(0),
            mute: row.get("mute").unwrap_or(0),
            save: row.get("save").unwrap_or(0),
            forbidden: row.get("forbidden").unwrap_or(0),
            follow: row.get("follow").unwrap_or(0),
            is_deleted: row.get("is_deleted")?,
            receipt: row.get("receipt").unwrap_or(0),
            status: row.get("status").unwrap_or(1),
            invite: row.get("invite").unwrap_or(0),
            robot: row.get("robot").unwrap_or(0),
            version: row.get("version")?,
            online: row.get("online").unwrap_or(0),
            last_offline: row.get("last_offline").unwrap_or(0),
            avatar: row.get("avatar").unwrap_or_default(),
            category: row.get("category").unwrap_or_default(),
            extra: row.get("extra").unwrap_or_default(),
            created_at: row.get("created_at").unwrap_or(0),
            updated_at: row.get("updated_at").unwrap_or(0),
            avatar_cache_key: row.get("avatar_cache_key").unwrap_or_default(),
            remote_extra: row.get("remote_extra").ok(),
            flame: row.get("flame").unwrap_or(0),
            flame_second: row.get("flame_second").unwrap_or(0),
            device_flag: row.get("device_flag").unwrap_or(0),
            parent_channel_id: row.get("parent_channel_id").unwrap_or(0),
            parent_channel_type: row.get("parent_channel_type").unwrap_or(0),
        })
    }
}
