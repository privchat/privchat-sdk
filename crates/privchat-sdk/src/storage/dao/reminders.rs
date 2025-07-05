//! reminders DAO 实现

use rusqlite::Connection;

pub struct RemindersDao<'a> {
    conn: &'a Connection,
}

impl<'a> RemindersDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
