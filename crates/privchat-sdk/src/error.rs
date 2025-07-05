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
    
    // 存储模块错误类型
    #[error("KV存储错误: {0}")]
    KvStore(String),
    
    #[error("队列错误: {0}")]
    Queue(String),
    
    #[error("媒体文件错误: {0}")]
    Media(String),
    
    #[error("数据库升级错误: {0}")]
    Migration(String),
    
    #[error("存储配置错误: {0}")]
    StorageConfig(String),
    
    #[error("文件系统错误: {0}")]
    FileSystem(String),
    
    #[error("加密错误: {0}")]
    Encryption(String),
    
    #[error("压缩错误: {0}")]
    Compression(String),
    
    #[error("网络错误: {0}")]
    Network(String),
    
    #[error("用户不存在: {0}")]
    UserNotFound(String),
    
    #[error("权限错误: {0}")]
    Permission(String),
    
    #[error("资源不足: {0}")]
    ResourceExhausted(String),
    
    #[error("操作超时: {0}")]
    Timeout(String),
    
    #[error("版本不兼容: {0}")]
    VersionMismatch(String),
    
    #[error("数据损坏: {0}")]
    DataCorruption(String),
}

pub type Result<T> = std::result::Result<T, PrivchatSDKError>; 