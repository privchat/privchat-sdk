//! channel_member DAO 实现

use rusqlite::Connection;

pub struct ChannelMemberDao<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelMemberDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
