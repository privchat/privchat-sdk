//! 数据访问层 (DAO) - 每张表一个专门的操作模块
//! 
//! 这里封装了所有数据库操作，确保：
//! - 数据操作的一致性和封装性
//! - 复杂业务逻辑的统一管理
//! - 未来 schema 升级的兼容性
//! - 跨平台数据一致性策略

pub mod message;
pub mod channel;
pub mod channel_member;
pub mod message_extra;
pub mod message_reaction;
pub mod reminder;
pub mod robot;
pub mod channel_extra;
pub mod migration;
pub mod mention;
pub mod user;
pub mod group_dao;
pub mod group_member_dao;
pub mod friend_dao;

// 重新导出核心 DAO 类型
pub use message::MessageDao;
pub use channel::ChannelDao;
pub use channel_member::ChannelMemberDao;
pub use message_extra::MessageExtraDao;
pub use message_reaction::MessageReactionDao;
pub use reminder::RemindersDao;
pub use robot::RobotDao;
pub use channel_extra::ChannelExtraDao;
pub use migration::MigrationDao;
pub use mention::MentionDao;
pub use user::UserDao;
pub use group_dao::GroupDao;
pub use group_member_dao::GroupMemberDao;
pub use friend_dao::FriendDao;

use rusqlite::Connection;
use crate::error::Result;

/// DAO 工厂 - 统一创建各种 DAO 实例
pub struct DaoFactory;

impl DaoFactory {
    /// 创建消息 DAO
    pub fn message_dao(conn: &Connection) -> MessageDao<'_> {
        MessageDao::new(conn)
    }
    
    /// 创建频道 DAO
    pub fn channel_dao(conn: &Connection) -> ChannelDao<'_> {
        ChannelDao::new(conn)
    }
    
    /// 创建频道成员 DAO
    pub fn channel_member_dao(conn: &Connection) -> ChannelMemberDao<'_> {
        ChannelMemberDao::new(conn)
    }
    
    /// 创建消息扩展 DAO
    pub fn message_extra_dao(conn: &Connection) -> MessageExtraDao<'_> {
        MessageExtraDao::new(conn)
    }
    
    /// 创建消息反应 DAO
    pub fn message_reaction_dao(conn: &Connection) -> MessageReactionDao<'_> {
        MessageReactionDao::new(conn)
    }
    
    /// 创建提醒 DAO
    pub fn reminder_dao(conn: &Connection) -> RemindersDao<'_> {
        RemindersDao::new(conn)
    }
    
    /// 创建机器人 DAO
    pub fn robot_dao(conn: &Connection) -> RobotDao<'_> {
        RobotDao::new(conn)
    }
    
    /// 创建会话扩展 DAO
    pub fn channel_extra_dao(conn: &Connection) -> ChannelExtraDao<'_> {
        ChannelExtraDao::new(conn)
    }
    
    /// 创建迁移 DAO
    pub fn migration_dao(conn: &Connection) -> MigrationDao<'_> {
        MigrationDao::new(conn)
    }
    
    /// 创建@提及 DAO
    pub fn mention_dao(conn: &Connection) -> MentionDao<'_> {
        MentionDao::new(conn)
    }

    pub fn user_dao(conn: &Connection) -> UserDao<'_> {
        UserDao::new(conn)
    }

    pub fn group_dao(conn: &Connection) -> GroupDao<'_> {
        GroupDao::new(conn)
    }

    pub fn group_member_dao(conn: &Connection) -> GroupMemberDao<'_> {
        GroupMemberDao::new(conn)
    }

    pub fn friend_dao(conn: &Connection) -> FriendDao<'_> {
        FriendDao::new(conn)
    }
}

/// 事务管理器 - 统一管理跨表操作的事务
pub struct TransactionManager<'a> {
    conn: &'a Connection,
}

impl<'a> TransactionManager<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 执行事务操作
    pub fn execute<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Connection) -> Result<R>,
    {
        let tx = self.conn.unchecked_transaction()
            .map_err(|e| crate::error::PrivchatSDKError::Database(format!("开始事务失败: {}", e)))?;
        
        let result = f(self.conn)?;
        
        tx.commit()
            .map_err(|e| crate::error::PrivchatSDKError::Database(format!("提交事务失败: {}", e)))?;
        
        Ok(result)
    }
} 