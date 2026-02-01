//! 群成员 DAO - group_member 表

use rusqlite::{Connection, params};
use crate::error::Result;
use crate::storage::entities::GroupMember;

pub struct GroupMemberDao<'a> {
    conn: &'a Connection,
}

impl<'a> GroupMemberDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, m: &GroupMember) -> Result<()> {
        let sql = r#"
            INSERT INTO group_member (group_id, user_id, role, status, alias, is_muted, joined_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(group_id, user_id) DO UPDATE SET
                role = excluded.role,
                status = excluded.status,
                alias = excluded.alias,
                is_muted = excluded.is_muted,
                updated_at = excluded.updated_at
        "#;
        self.conn.execute(
            sql,
            params![
                m.group_id,
                m.user_id,
                m.role,
                m.status,
                m.alias,
                m.is_muted as i32,
                m.joined_at,
                m.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_many(&self, members: &[GroupMember]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for m in members {
            let sql = r#"
                INSERT INTO group_member (group_id, user_id, role, status, alias, is_muted, joined_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(group_id, user_id) DO UPDATE SET
                    role = excluded.role,
                    status = excluded.status,
                    alias = excluded.alias,
                    is_muted = excluded.is_muted,
                    updated_at = excluded.updated_at
            "#;
            tx.execute(
                sql,
                params![m.group_id, m.user_id, m.role, m.status, m.alias, m.is_muted as i32, m.joined_at, m.updated_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_by_group(&self, group_id: u64, status: Option<i32>, limit: u32, offset: u32) -> Result<Vec<GroupMember>> {
        let mut out = Vec::new();
        match status {
            Some(s) => {
                let sql = r#"SELECT group_id, user_id, role, status, alias, is_muted, joined_at, updated_at
                    FROM group_member WHERE group_id = ?1 AND status = ?2 ORDER BY role ASC, joined_at ASC LIMIT ?3 OFFSET ?4"#;
                let mut stmt = self.conn.prepare(sql)?;
                let rows = stmt.query_map(params![group_id, s, limit, offset], |row| row_to_member(&row))?;
                for r in rows {
                    out.push(r?);
                }
            }
            None => {
                let sql = r#"SELECT group_id, user_id, role, status, alias, is_muted, joined_at, updated_at
                    FROM group_member WHERE group_id = ?1 ORDER BY role ASC, joined_at ASC LIMIT ?2 OFFSET ?3"#;
                let mut stmt = self.conn.prepare(sql)?;
                let rows = stmt.query_map(params![group_id, limit, offset], |row| row_to_member(&row))?;
                for r in rows {
                    out.push(r?);
                }
            }
        }
        Ok(out)
    }
}

fn row_to_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupMember> {
    Ok(GroupMember {
        group_id: row.get::<_, i64>(0)? as u64,
        user_id: row.get::<_, i64>(1)? as u64,
        role: row.get(2)?,
        status: row.get(3)?,
        alias: row.get(4)?,
        is_muted: row.get::<_, i32>(5)? != 0,
        joined_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}
