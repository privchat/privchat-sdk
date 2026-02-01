//! reminder DAO 实现

use rusqlite::Connection;

#[allow(dead_code)]
pub struct RemindersDao<'a> {
    conn: &'a Connection,
}

impl<'a> RemindersDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
