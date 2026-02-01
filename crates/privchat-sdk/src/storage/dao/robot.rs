//! robot DAO 实现

use rusqlite::Connection;

#[allow(dead_code)]
pub struct RobotDao<'a> {
    conn: &'a Connection,
}

impl<'a> RobotDao<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}
