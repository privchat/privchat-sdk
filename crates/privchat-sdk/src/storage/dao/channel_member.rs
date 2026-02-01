//! channel_member DAO 实现

use rusqlite::{Connection, params, Row};
use crate::error::Result;
use crate::storage::entities::ChannelMember;

pub struct ChannelMemberDao<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelMemberDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 插入或更新频道成员
    pub fn upsert(&self, member: &ChannelMember) -> Result<()> {
        let sql = r#"
            INSERT INTO channel_member (
                channel_id, channel_type, member_uid, member_name, member_remark,
                member_avatar, member_invite_uid, role, status, is_deleted,
                robot, version, created_at, updated_at, extra,
                forbidden_expiration_time, member_avatar_cache_key
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
            )
            ON CONFLICT(channel_id, channel_type, member_uid) DO UPDATE SET
                member_name = excluded.member_name,
                member_remark = excluded.member_remark,
                member_avatar = excluded.member_avatar,
                member_invite_uid = excluded.member_invite_uid,
                role = excluded.role,
                status = excluded.status,
                is_deleted = excluded.is_deleted,
                robot = excluded.robot,
                version = excluded.version,
                updated_at = excluded.updated_at,
                extra = excluded.extra,
                forbidden_expiration_time = excluded.forbidden_expiration_time,
                member_avatar_cache_key = excluded.member_avatar_cache_key
        "#;
        
        self.conn.execute(sql, params![
            member.channel_id, member.channel_type, member.member_uid,
            member.member_name, member.member_remark, member.member_avatar,
            member.member_invite_uid, member.role, member.status,
            member.is_deleted, member.robot, member.version,
            member.created_at, member.updated_at, member.extra,
            member.forbidden_expiration_time, member.member_avatar_cache_key
        ])?;
        
        Ok(())
    }
    
    /// 批量插入或更新频道成员
    pub fn upsert_batch(&self, members: &[ChannelMember]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        
        let sql = r#"
            INSERT INTO channel_member (
                channel_id, channel_type, member_uid, member_name, member_remark,
                member_avatar, member_invite_uid, role, status, is_deleted,
                robot, version, created_at, updated_at, extra,
                forbidden_expiration_time, member_avatar_cache_key
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17
            )
            ON CONFLICT(channel_id, channel_type, member_uid) DO UPDATE SET
                member_name = excluded.member_name,
                member_remark = excluded.member_remark,
                member_avatar = excluded.member_avatar,
                member_invite_uid = excluded.member_invite_uid,
                role = excluded.role,
                status = excluded.status,
                is_deleted = excluded.is_deleted,
                robot = excluded.robot,
                version = excluded.version,
                updated_at = excluded.updated_at,
                extra = excluded.extra,
                forbidden_expiration_time = excluded.forbidden_expiration_time,
                member_avatar_cache_key = excluded.member_avatar_cache_key
        "#;
        
        {
            let mut stmt = tx.prepare(sql)?;
            
            for member in members {
                stmt.execute(params![
                    member.channel_id, member.channel_type, member.member_uid,
                    member.member_name, member.member_remark, member.member_avatar,
                    member.member_invite_uid, member.role, member.status,
                    member.is_deleted, member.robot, member.version,
                    member.created_at, member.updated_at, member.extra,
                    member.forbidden_expiration_time, member.member_avatar_cache_key
                ])?;
            }
        } // stmt 在这里被释放
        
        tx.commit()?;
        Ok(())
    }
    
    /// 按频道查询成员列表（群成员：channel_id=group_id, channel_type=2）
    pub fn list_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<ChannelMember>> {
        let mut sql = "SELECT * FROM channel_member WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0 ORDER BY created_at DESC".to_string();
        if let Some(lim) = limit {
            sql.push_str(&format!(" LIMIT {}", lim));
            if let Some(off) = offset {
                sql.push_str(&format!(" OFFSET {}", off));
            }
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let members = stmt.query_map(rusqlite::params![channel_id as i64, channel_type], |row| {
            self.row_to_member(row).map_err(|e| rusqlite::Error::InvalidColumnType(
                0,
                format!("{}", e).into(),
                rusqlite::types::Type::Null
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
        
        Ok(members)
    }
    
    /// 从 Row 转换为 ChannelMember
    fn row_to_member(&self, row: &Row) -> Result<ChannelMember> {
        Ok(ChannelMember {
            id: Some(row.get(0)?),
            channel_id: row.get::<_, i64>(1)? as u64,
            channel_type: row.get(2)?,
            member_uid: row.get::<_, i64>(3)? as u64,
            member_name: row.get(4)?,
            member_remark: row.get(5)?,
            member_avatar: row.get(6)?,
            member_invite_uid: row.get::<_, i64>(7)? as u64,
            role: row.get(8)?,
            status: row.get(9)?,
            is_deleted: row.get(10)?,
            robot: row.get(11)?,
            version: row.get(12)?,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
            extra: row.get(15)?,
            forbidden_expiration_time: row.get(16)?,
            member_avatar_cache_key: row.get(17)?,
        })
    }
    
    /// 删除频道成员
    pub fn delete(&self, channel_id: u64, channel_type: i32, member_uid: u64) -> Result<()> {
        let sql = "DELETE FROM channel_member WHERE channel_id = ?1 AND channel_type = ?2 AND member_uid = ?3";
        self.conn.execute(sql, params![channel_id, channel_type, member_uid as i64])?;
        Ok(())
    }
    
    /// 获取频道成员数量
    pub fn count(&self, channel_id: u64, channel_type: i32) -> Result<u32> {
        let sql = "SELECT COUNT(*) FROM channel_member WHERE channel_id = ?1 AND channel_type = ?2 AND is_deleted = 0";
        let count: u32 = self.conn.query_row(sql, params![channel_id, channel_type], |row| row.get(0))?;
        Ok(count)
    }
}
