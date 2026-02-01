//! 用户 DAO - user 表（好友/群成员/陌生人共用）

use rusqlite::{Connection, params};
use crate::error::Result;
use crate::storage::entities::User;

pub struct UserDao<'a> {
    conn: &'a Connection,
}

impl<'a> UserDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn upsert(&self, u: &User) -> Result<()> {
        let sql = r#"
            INSERT INTO "user" (user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(user_id) DO UPDATE SET
                username = excluded.username,
                nickname = excluded.nickname,
                alias = excluded.alias,
                avatar = excluded.avatar,
                user_type = excluded.user_type,
                is_deleted = excluded.is_deleted,
                channel_id = excluded.channel_id,
                updated_at = excluded.updated_at
        "#;
        self.conn.execute(
            sql,
            params![
                u.user_id,
                u.username,
                u.nickname,
                u.alias,
                u.avatar,
                u.user_type,
                u.is_deleted as i32,
                u.channel_id,
                u.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_id(&self, user_id: u64) -> Result<Option<User>> {
        let sql = r#"SELECT user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, updated_at FROM "user" WHERE user_id = ?1"#;
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![user_id], |row| row_to_user(&row))?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_by_ids(&self, ids: &[u64]) -> Result<Vec<User>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..ids.len()).map(|i| format!("?{}", i + 1)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            r#"SELECT user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, updated_at FROM "user" WHERE user_id IN ({})"#,
            placeholders
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut out = Vec::new();
        let args: Vec<i64> = ids.iter().map(|&id| id as i64).collect();
        let mut rows = stmt.query(rusqlite::params_from_iter(args.iter()))?;
        while let Some(row) = rows.next()? {
            out.push(row_to_user(&row)?);
        }
        Ok(out)
    }

    pub fn update_channel_id(&self, user_id: u64, channel_id: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        self.conn.execute(
            r#"UPDATE "user" SET channel_id = ?1, updated_at = ?2 WHERE user_id = ?3"#,
            params![channel_id, now, user_id],
        )?;
        Ok(())
    }
}

fn row_to_user(row: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User {
        user_id: row.get::<_, i64>(0)? as u64,
        username: row.get(1)?,
        nickname: row.get(2)?,
        alias: row.get(3)?,
        avatar: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        user_type: row.get(5)?,
        is_deleted: row.get::<_, i32>(6)? != 0,
        channel_id: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        updated_at: row.get(8)?,
    })
}
