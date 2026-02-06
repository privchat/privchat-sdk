//! 会话数据访问层 - 管理会话列表和未读数等

use crate::error::Result;
use crate::storage::entities::Channel;
use rusqlite::{params, Connection, Row};

/// 会话数据访问对象
pub struct ChannelDao<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 插入或更新会话
    pub fn upsert(&self, channel: &Channel) -> Result<i64> {
        let sql = "INSERT OR REPLACE INTO channel (
            channel_id, channel_type, last_local_message_id, last_msg_timestamp,
            last_msg_content, unread_count, is_deleted, version, extra, last_msg_pts,
            parent_channel_id, parent_channel_type
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)";

        self.conn.execute(
            sql,
            params![
                channel.channel_id,
                channel.channel_type,
                channel.last_local_message_id,
                channel.last_msg_timestamp,
                channel.last_msg_content,
                channel.unread_count,
                channel.is_deleted,
                channel.version,
                channel.extra,
                channel.last_msg_pts,
                channel.parent_channel_id,
                channel.parent_channel_type,
            ],
        )?;

        Ok(self.conn.last_insert_rowid())
    }

    /// 根据频道ID获取会话（表内 channel_id 唯一）
    pub fn get_by_channel(&self, channel_id: u64, _channel_type: i32) -> Result<Option<Channel>> {
        let sql = "SELECT * FROM channel WHERE channel_id = ?1 AND is_deleted = 0";

        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![channel_id as i64], |row| {
            Ok(self.row_to_channel(row)?)
        })?;

        match rows.next() {
            Some(Ok(channel)) => Ok(Some(channel)),
            Some(Err(e)) => Err(crate::error::PrivchatSDKError::Database(format!(
                "查询会话失败: {}",
                e
            ))),
            None => Ok(None),
        }
    }

    /// 更新未读数
    pub fn update_unread_count(
        &self,
        channel_id: u64,
        _channel_type: i32,
        count: i32,
    ) -> Result<()> {
        let sql = "UPDATE channel SET unread_count = ?1 WHERE channel_id = ?2";
        self.conn.execute(sql, params![count, channel_id as i64])?;
        Ok(())
    }

    /// 更新 pts（用于同步）
    pub fn update_pts(&self, channel_id: u64, _channel_type: i32, new_pts: u64) -> Result<()> {
        let sql = "UPDATE channel SET last_msg_pts = ?1 WHERE channel_id = ?2";
        self.conn
            .execute(sql, params![new_pts as i64, channel_id as i64])?;
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

        let total: Option<i32> = self.conn.query_row(sql, [], |row| Ok(row.get(0)?))?;

        Ok(total.unwrap_or(0))
    }

    /// 获取总未读消息数（排除免打扰的会话）
    ///
    /// 这是更准确的统计方式，与 Telegram 等主流 IM SDK 的设计一致
    pub fn get_total_unread_count_exclude_muted(&self) -> Result<i32> {
        // 查询所有未删除且未免打扰的会话的未读数总和
        // 假设免打扰状态存储在 extra 字段的 JSON 中：{"muted": true}
        let sql = "SELECT SUM(unread_count) FROM channel
                   WHERE is_deleted = 0
                   AND (extra IS NULL OR extra = '{}' OR json_extract(extra, '$.muted') IS NULL OR json_extract(extra, '$.muted') = 0)";

        let total: Option<i32> = self.conn.query_row(sql, [], |row| Ok(row.get(0)?))?;

        Ok(total.unwrap_or(0))
    }

    /// 获取所有会话列表（用于验证总未读数）
    pub fn list_all(&self) -> Result<Vec<Channel>> {
        let sql = "SELECT * FROM channel WHERE is_deleted = 0 ORDER BY last_msg_timestamp DESC";

        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| Ok(self.row_to_channel(row)?))?;

        let mut channels = Vec::new();
        for row in rows {
            channels.push(row?);
        }

        Ok(channels)
    }

    /// 将行转换为会话实体（channel 表以 channel_id 为主键，无 id 列时用 channel_id 作为实体 id）
    fn row_to_channel(&self, row: &Row) -> rusqlite::Result<Channel> {
        let channel_id: i64 = row.get("channel_id")?;
        Ok(Channel {
            id: Some(channel_id),
            channel_id: channel_id as u64,
            channel_type: row.get("channel_type")?,
            last_local_message_id: row.get("last_local_message_id")?,
            last_msg_timestamp: row.get("last_msg_timestamp")?,
            last_msg_content: row.get("last_msg_content").unwrap_or_default(),
            unread_count: row.get("unread_count")?,
            is_deleted: row.get("is_deleted")?,
            version: row.get("version")?,
            extra: row.get("extra")?,
            last_msg_pts: row.get("last_msg_pts")?, // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: row.get("parent_channel_id")?,
            parent_channel_type: row.get("parent_channel_type")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    fn create_test_db() -> (TempDir, Connection) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();

        // 创建会话表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS channel (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL,
                channel_type INTEGER NOT NULL,
                last_local_message_id TEXT,
                last_msg_timestamp INTEGER,
                unread_count INTEGER DEFAULT 0,
                is_deleted INTEGER DEFAULT 0,
                version INTEGER DEFAULT 1,
                extra TEXT DEFAULT '{}',
                last_msg_pts INTEGER,  -- ⭐ last_msg_seq -> last_msg_pts
                parent_channel_id TEXT DEFAULT '',
                parent_channel_type INTEGER DEFAULT 0,
                UNIQUE(channel_id, channel_type)
            )",
            [],
        )
        .unwrap();

        (temp_dir, conn)
    }

    #[test]
    fn test_get_total_unread_count() {
        let (_temp_dir, conn) = create_test_db();
        let dao = ChannelDao::new(&conn);

        // 创建几个测试会话
        let conv1 = Channel {
            id: None,
            channel_id: 1001,
            channel_type: 1,
            last_local_message_id: "msg1".to_string(),
            last_msg_timestamp: Some(1000),
            unread_count: 5,
            is_deleted: 0,
            version: 1,
            extra: "{}".to_string(),
            last_msg_pts: 1, // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        let conv2 = Channel {
            id: None,
            channel_id: 1002,
            channel_type: 1,
            last_local_message_id: "msg2".to_string(),
            last_msg_timestamp: Some(2000),
            unread_count: 3,
            is_deleted: 0,
            version: 1,
            extra: "{}".to_string(),
            last_msg_pts: 2, // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        let conv3 = Channel {
            id: None,
            channel_id: 1003,
            channel_type: 1,
            last_local_message_id: "msg3".to_string(),
            last_msg_timestamp: Some(3000),
            unread_count: 2,
            is_deleted: 0,
            version: 1,
            extra: "{}".to_string(),
            last_msg_pts: 3, // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        // 插入会话
        dao.upsert(&conv1).unwrap();
        dao.upsert(&conv2).unwrap();
        dao.upsert(&conv3).unwrap();

        // 测试总未读数
        let total = dao.get_total_unread_count().unwrap();
        assert_eq!(total, 10); // 5 + 3 + 2 = 10

        println!("✅ 总未读数测试通过: {}", total);
    }

    #[test]
    fn test_get_total_unread_count_exclude_muted() {
        let (_temp_dir, conn) = create_test_db();
        let dao = ChannelDao::new(&conn);

        // 创建测试会话（包括免打扰的）
        let conv1 = Channel {
            id: None,
            channel_id: 1001,
            channel_type: 1,
            last_local_message_id: "msg1".to_string(),
            last_msg_timestamp: Some(1000),
            unread_count: 5,
            is_deleted: 0,
            version: 1,
            extra: "{}".to_string(), // 未免打扰
            last_msg_pts: 1,         // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        let conv2 = Channel {
            id: None,
            channel_id: 1002,
            channel_type: 1,
            last_local_message_id: "msg2".to_string(),
            last_msg_timestamp: Some(2000),
            unread_count: 3,
            is_deleted: 0,
            version: 1,
            extra: r#"{"muted": true}"#.to_string(), // 免打扰
            last_msg_pts: 2,                         // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        let conv3 = Channel {
            id: None,
            channel_id: 1003,
            channel_type: 1,
            last_local_message_id: "msg3".to_string(),
            last_msg_timestamp: Some(3000),
            unread_count: 2,
            is_deleted: 0,
            version: 1,
            extra: "{}".to_string(), // 未免打扰
            last_msg_pts: 3,         // ⭐ last_msg_seq -> last_msg_pts
            parent_channel_id: 0,
            parent_channel_type: 0,
        };

        // 插入会话
        dao.upsert(&conv1).unwrap();
        dao.upsert(&conv2).unwrap();
        dao.upsert(&conv3).unwrap();

        // 测试总未读数（排除免打扰）
        let total = dao.get_total_unread_count_exclude_muted().unwrap();
        assert_eq!(total, 7); // 5 + 2 = 7 (排除免打扰的 channel2)

        // 测试总未读数（包括免打扰）
        let total_all = dao.get_total_unread_count().unwrap();
        assert_eq!(total_all, 10); // 5 + 3 + 2 = 10

        println!(
            "✅ 排除免打扰测试通过: 排除免打扰={}, 全部={}",
            total, total_all
        );
    }

    #[test]
    fn test_verify_total_unread_count() {
        let (_temp_dir, conn) = create_test_db();
        let dao = ChannelDao::new(&conn);

        // 创建测试会话
        let channels = vec![
            Channel {
                id: None,
                channel_id: 1001,
                channel_type: 1,
                last_local_message_id: "msg1".to_string(),
                last_msg_timestamp: Some(1000),
                unread_count: 5,
                is_deleted: 0,
                version: 1,
                extra: "{}".to_string(),
                last_msg_pts: 1, // ⭐ last_msg_seq -> last_msg_pts
                parent_channel_id: 0,
                parent_channel_type: 0,
            },
            Channel {
                id: None,
                channel_id: 1002,
                channel_type: 1,
                last_local_message_id: "msg2".to_string(),
                last_msg_timestamp: Some(2000),
                unread_count: 3,
                is_deleted: 0,
                version: 1,
                extra: "{}".to_string(),
                last_msg_pts: 2, // ⭐ last_msg_seq -> last_msg_pts
                parent_channel_id: 0,
                parent_channel_type: 0,
            },
        ];

        // 插入会话
        for conv in &channels {
            dao.upsert(conv).unwrap();
        }

        // 获取总未读数
        let total = dao.get_total_unread_count().unwrap();

        // 获取所有会话并计算和
        let all_channels = dao.list_all().unwrap();
        let sum: i32 = all_channels.iter().map(|conv| conv.unread_count).sum();

        // 验证一致性
        assert_eq!(total, sum, "总未读数应该等于会话列表未读数的和");
        assert_eq!(total, 8, "总未读数应该是 5 + 3 = 8");

        println!(
            "✅ 数据一致性验证通过: 总未读数={}, 会话列表和={}",
            total, sum
        );
    }
}
