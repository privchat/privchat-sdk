//! 好友 DAO - friend 表（仅关系；展示用 user）

use rusqlite::{Connection, params};
use crate::error::Result;
use crate::storage::entities::Friend;

pub struct FriendDao<'a> {
    conn: &'a Connection,
}

impl<'a> FriendDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, f: &Friend) -> Result<()> {
        let sql = r#"
            INSERT INTO friend (user_id, tags, is_pinned, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(user_id) DO UPDATE SET
                tags = excluded.tags,
                is_pinned = excluded.is_pinned,
                updated_at = excluded.updated_at
        "#;
        self.conn.execute(
            sql,
            params![f.user_id, f.tags, f.is_pinned as i32, f.created_at, f.updated_at],
        )?;
        Ok(())
    }

    pub fn upsert_many(&self, friends: &[Friend]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for f in friends {
            let sql = r#"
                INSERT INTO friend (user_id, tags, is_pinned, created_at, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(user_id) DO UPDATE SET
                    tags = excluded.tags,
                    is_pinned = excluded.is_pinned,
                    updated_at = excluded.updated_at
            "#;
            tx.execute(sql, params![f.user_id, f.tags, f.is_pinned as i32, f.created_at, f.updated_at])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list(&self, limit: u32, offset: u32) -> Result<Vec<Friend>> {
        let sql = r#"
            SELECT user_id, tags, is_pinned, created_at, updated_at
            FROM friend
            ORDER BY updated_at DESC
            LIMIT ?1 OFFSET ?2
        "#;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit, offset], |row| row_to_friend(&row))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn count(&self) -> Result<u32> {
        let n: i64 = self.conn.query_row("SELECT COUNT(*) FROM friend", [], |r| r.get(0))?;
        Ok(n as u32)
    }

    pub fn delete_by_user_id(&self, user_id: u64) -> Result<()> {
        self.conn.execute("DELETE FROM friend WHERE user_id = ?1", params![user_id])?;
        Ok(())
    }
}

fn row_to_friend(row: &rusqlite::Row<'_>) -> rusqlite::Result<Friend> {
    Ok(Friend {
        user_id: row.get::<_, i64>(0)? as u64,
        tags: row.get(1)?,
        is_pinned: row.get::<_, i32>(2)? != 0,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}
