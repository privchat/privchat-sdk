use std::fmt;
use rusqlite;

#[derive(Debug)]
pub enum PrivchatSDKError {
    SqliteError(rusqlite::Error),
    JsonError(String),
    InvalidArgument(String),
    NotFound(String),
    AlreadyExists(String),
    Other(String),
    // 添加缺失的错误变体
    KvStore(String),
    Serialization(String),
    IO(String),
    Database(String),
    Migration(String),
    NotConnected,
    Transport(String),  // 添加传输层错误
    Auth(String),       // 添加认证错误
}

impl fmt::Display for PrivchatSDKError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PrivchatSDKError::SqliteError(e) => write!(f, "SQLite error: {}", e),
            PrivchatSDKError::JsonError(e) => write!(f, "JSON error: {}", e),
            PrivchatSDKError::InvalidArgument(e) => write!(f, "Invalid argument: {}", e),
            PrivchatSDKError::NotFound(e) => write!(f, "Not found: {}", e),
            PrivchatSDKError::AlreadyExists(e) => write!(f, "Already exists: {}", e),
            PrivchatSDKError::Other(e) => write!(f, "Other error: {}", e),
            PrivchatSDKError::KvStore(e) => write!(f, "KV store error: {}", e),
            PrivchatSDKError::Serialization(e) => write!(f, "Serialization error: {}", e),
            PrivchatSDKError::IO(e) => write!(f, "IO error: {}", e),
            PrivchatSDKError::Database(e) => write!(f, "Database error: {}", e),
            PrivchatSDKError::Migration(e) => write!(f, "Migration error: {}", e),
            PrivchatSDKError::NotConnected => write!(f, "Not connected"),
            PrivchatSDKError::Transport(e) => write!(f, "Transport error: {}", e),
            PrivchatSDKError::Auth(e) => write!(f, "Authentication error: {}", e),
        }
    }
}

impl std::error::Error for PrivchatSDKError {}

impl From<rusqlite::Error> for PrivchatSDKError {
    fn from(error: rusqlite::Error) -> Self {
        PrivchatSDKError::SqliteError(error)
    }
}

impl From<serde_json::Error> for PrivchatSDKError {
    fn from(error: serde_json::Error) -> Self {
        PrivchatSDKError::JsonError(error.to_string())
    }
}

impl From<std::io::Error> for PrivchatSDKError {
    fn from(error: std::io::Error) -> Self {
        PrivchatSDKError::IO(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, PrivchatSDKError>;

pub enum SendFailureReason {
    NetworkTimeout,     // 网络超时 → 重试
    NetworkUnavailable, // 无网络 → 等待恢复
    ServerError,        // 服务端错误 → 根据错误码
    AuthFailure,        // 认证失败 → 重新登录
    RateLimited,        // 限流 → 延迟重试
    MessageTooLarge,    // 消息过大 → 不重试
    Forbidden,          // 权限不足 → 不重试
} 