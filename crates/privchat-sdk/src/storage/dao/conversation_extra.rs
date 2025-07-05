//! conversation_extra DAO 实现

use rusqlite::Connection;

pub struct ConversationExtraDao<'a> {
    conn: &'a Connection,
}

impl<'a> ConversationExtraDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
