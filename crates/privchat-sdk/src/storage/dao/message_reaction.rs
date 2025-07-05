//! message_reaction DAO 实现

use rusqlite::Connection;

pub struct MessageReactionDao<'a> {
    conn: &'a Connection,
}

impl<'a> MessageReactionDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
