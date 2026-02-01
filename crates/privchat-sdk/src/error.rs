use std::fmt;
use rusqlite;
use privchat_protocol::ErrorCode;

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
    Auth(String),       // 添加认证错误（保留用于向后兼容）
    QueueFull(String),
    InvalidInput(String),
    InvalidOperation(String),
    Timeout(String),
    // SDK 相关错误
    Runtime(String),        // 运行时错误
    Config(String),         // 配置错误
    NotInitialized(String), // 未初始化错误
    ShuttingDown(String),   // 正在关闭错误
    ServerError(String),    // 服务器错误（保留用于向后兼容）
    InvalidData(String),    // 无效数据错误
    // RPC 错误 - 使用协议层的 ErrorCode
    Rpc {
        code: ErrorCode,
        message: String,
    },
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
            PrivchatSDKError::QueueFull(e) => write!(f, "Queue is full: {}", e),
            PrivchatSDKError::InvalidInput(e) => write!(f, "Invalid input: {}", e),
            PrivchatSDKError::InvalidOperation(e) => write!(f, "Invalid operation: {}", e),
            PrivchatSDKError::Timeout(e) => write!(f, "Timeout: {}", e),
            PrivchatSDKError::Runtime(e) => write!(f, "Runtime error: {}", e),
            PrivchatSDKError::Config(e) => write!(f, "Config error: {}", e),
            PrivchatSDKError::NotInitialized(e) => write!(f, "Not initialized: {}", e),
            PrivchatSDKError::ShuttingDown(e) => write!(f, "Shutting down: {}", e),
            PrivchatSDKError::ServerError(e) => write!(f, "Server error: {}", e),
            PrivchatSDKError::InvalidData(e) => write!(f, "Invalid data: {}", e),
            PrivchatSDKError::Rpc { code, message } => {
                write!(f, "RPC error [{}]: {}", code.code(), message)
            }
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

impl PrivchatSDKError {
    /// 获取 RPC 错误码（如果这是一个 RPC 错误）
    pub fn rpc_code(&self) -> Option<ErrorCode> {
        match self {
            PrivchatSDKError::Rpc { code, .. } => Some(*code),
            _ => None,
        }
    }
    
    /// 判断是否是 RPC 错误
    pub fn is_rpc_error(&self) -> bool {
        matches!(self, PrivchatSDKError::Rpc { .. })
    }
    
    /// 从 RPC 响应创建 RPC 错误
    pub fn from_rpc_response(code: u32, message: String) -> Self {
        let error_code = ErrorCode::from_code(code)
            .unwrap_or(ErrorCode::SystemError);
        PrivchatSDKError::Rpc {
            code: error_code,
            message,
        }
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