//! 群 DAO - group 表

use rusqlite::{Connection, params};
use crate::error::Result;
use crate::storage::entities::Group;

pub struct GroupDao<'a> {
    conn: &'a Connection,
}

impl<'a> GroupDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, g: &Group) -> Result<()> {
        let sql = r#"
            INSERT INTO "group" (group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(group_id) DO UPDATE SET
                name = excluded.name,
                avatar = excluded.avatar,
                owner_id = excluded.owner_id,
                is_dismissed = excluded.is_dismissed,
                updated_at = excluded.updated_at
        "#;
        self.conn.execute(
            sql,
            params![
                g.group_id,
                g.name,
                g.avatar,
                g.owner_id,
                g.is_dismissed as i32,
                g.created_at,
                g.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, group_id: u64) -> Result<Option<Group>> {
        let sql = r#"SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at FROM "group" WHERE group_id = ?1"#;
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![group_id], |row| row_to_group(&row))?;
        Ok(rows.next().transpose()?)
    }

    /// 分页列出群列表（按 updated_at 倒序）
    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<Group>> {
        let sql = r#"SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at
            FROM "group" WHERE is_dismissed = 0 ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2"#;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit, offset], |row| row_to_group(&row))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn row_to_group(row: &rusqlite::Row<'_>) -> rusqlite::Result<Group> {
    Ok(Group {
        group_id: row.get::<_, i64>(0)? as u64,
        name: row.get(1)?,
        avatar: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
        owner_id: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
        is_dismissed: row.get::<_, i32>(4)? != 0,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
