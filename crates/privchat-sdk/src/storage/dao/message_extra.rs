//! message_extra DAO 实现

use rusqlite::Connection;

pub struct MessageExtraDao<'a> {
    conn: &'a Connection,
}

impl<'a> MessageExtraDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
