//! channel_extra DAO 实现

use rusqlite::Connection;

#[allow(dead_code)]
pub struct ChannelExtraDao<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelExtraDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
