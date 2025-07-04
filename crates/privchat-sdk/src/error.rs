use thiserror::Error;
use rusqlite;

#[derive(Error, Debug)]
pub enum PrivchatSDKError {
    #[error("传输错误: {0}")]
    Transport(String),
    
    #[error("序列化错误: {0}")]
    Serialization(String),
    
    #[error("IO错误: {0}")]
    IO(String),
    
    #[error("数据库错误: {0}")]
    Database(String),
    
    #[error("SQLite错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    
    #[error("认证错误: {0}")]
    Auth(String),
    
    #[error("未知消息类型: {0}")]
    UnknownMessageType(u8),
    
    #[error("消息解码错误: {0}")]
    MessageDecode(String),
    
    #[error("未连接到服务器")]
    NotConnected,
    
    #[error("消息类型 {0:?} 没有找到处理器")]
    HandlerNotFound(privchat_protocol::MessageType),
}

pub type Result<T> = std::result::Result<T, PrivchatSDKError>; 