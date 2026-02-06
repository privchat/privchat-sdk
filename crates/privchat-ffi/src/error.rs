//! Error types for FFI layer
//! 
//! These errors are designed to be simple and cross-language friendly.

use std::fmt;

/// Main error type for FFI operations
/// 
/// IMPORTANT: This must match EXACTLY with api.udl definition
/// 
/// UniFFI 0.31: Enhanced with detailed error information
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum PrivchatError {
    #[error("Generic error: {msg}")]
    Generic { msg: String },
    
    #[error("Database error: {msg}")]
    Database { msg: String },
    
    #[error("Network error: {msg} (code: {code})")]
    Network { msg: String, code: i32 },
    
    #[error("Authentication error: {reason}")]
    Authentication { reason: String },
    
    #[error("Invalid parameter: {field} - {msg}")]
    InvalidParameter { field: String, msg: String },
    
    #[error("Operation timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },
    
    #[error("Client is disconnected")]
    Disconnected,
    
    #[error("SDK not initialized")]
    NotInitialized,
}

impl PrivchatError {
    /// Create a generic error (UniFFI 0.31: stores message in error)
    pub fn generic<T: fmt::Display>(msg: T) -> Self {
        let msg_str = msg.to_string();
        tracing::error!("Generic error: {}", msg_str);
        Self::Generic { msg: msg_str }
    }
    
    /// Create a database error (UniFFI 0.31: stores message in error)
    pub fn database<T: fmt::Display>(msg: T) -> Self {
        let msg_str = msg.to_string();
        tracing::error!("Database error: {}", msg_str);
        Self::Database { msg: msg_str }
    }
    
    /// Create a network error (UniFFI 0.31: stores message and code)
    pub fn network<T: fmt::Display>(msg: T, code: i32) -> Self {
        let msg_str = msg.to_string();
        tracing::error!("Network error: {} (code: {})", msg_str, code);
        Self::Network { msg: msg_str, code }
    }
    
    /// Create an authentication error (UniFFI 0.31: stores reason)
    pub fn authentication<T: fmt::Display>(reason: T) -> Self {
        let reason_str = reason.to_string();
        tracing::error!("Authentication error: {}", reason_str);
        Self::Authentication { reason: reason_str }
    }
    
    /// Create an invalid parameter error (UniFFI 0.31: stores field and message)
    pub fn invalid_parameter(field: &str, msg: &str) -> Self {
        tracing::error!("Invalid parameter {}: {}", field, msg);
        Self::InvalidParameter {
            field: field.to_string(),
            msg: msg.to_string(),
        }
    }
    
    /// Create a timeout error (UniFFI 0.31: stores timeout duration)
    pub fn timeout(timeout_secs: u64) -> Self {
        tracing::error!("Operation timed out after {}s", timeout_secs);
        Self::Timeout { timeout_secs }
    }
}

/// 保证错误文案非空，避免 Kotlin/iOS 上显示空白
fn ensure_non_empty(s: String, fallback: &'static str) -> String {
    let t = s.trim();
    if t.is_empty() { fallback.to_string() } else { s }
}

/// Convert SDK errors to FFI errors (UniFFI 0.31: with detailed information)
impl From<privchat_sdk::PrivchatSDKError> for PrivchatError {
    fn from(error: privchat_sdk::PrivchatSDKError) -> Self {
        use privchat_sdk::PrivchatSDKError;
        
        let error_msg = format!("{:?}", error);
        tracing::error!("SDK error: {}", error_msg);
        
        match error {
            // RPC 错误 - 使用协议层的错误码
            PrivchatSDKError::Rpc { code, message } => {
                use privchat_protocol::ErrorCode;
                
                let error_code = code.code() as i32;
                let msg = ensure_non_empty(message, "RPC error");
                
                if code == ErrorCode::AuthRequired
                    || code == ErrorCode::InvalidToken
                    || code == ErrorCode::TokenExpired
                    || code == ErrorCode::TokenRevoked {
                    Self::Authentication { reason: msg }
                }
                else if code == ErrorCode::NetworkError || code == ErrorCode::Timeout {
                    Self::Network { msg, code: error_code }
                }
                else if code == ErrorCode::InvalidParams
                    || code == ErrorCode::MissingRequiredParam
                    || code == ErrorCode::InvalidParamType {
                    Self::InvalidParameter {
                        field: "parameter".to_string(),
                        msg,
                    }
                }
                else {
                    Self::Network { msg, code: error_code }
                }
            },
            PrivchatSDKError::Database(e) => Self::Database {
                msg: ensure_non_empty(e.to_string(), "Database error"),
            },
            PrivchatSDKError::IO(e) => Self::Generic {
                msg: ensure_non_empty(e.to_string(), "IO error"),
            },
            PrivchatSDKError::Serialization(e) => Self::Generic {
                msg: format!("Serialization error: {}", e),
            },
            PrivchatSDKError::Timeout(e) => {
                let timeout_secs = if let Ok(duration) = e.parse::<u64>() { duration } else { 30 };
                Self::Timeout { timeout_secs }
            }
            PrivchatSDKError::Transport(e) => Self::Network {
                msg: ensure_non_empty(e.to_string(), "Transport error"),
                code: -1,
            },
            PrivchatSDKError::Auth(e) => Self::Authentication {
                reason: ensure_non_empty(e.to_string(), "Authentication failed"),
            },
            PrivchatSDKError::InvalidInput(e) => Self::InvalidParameter {
                field: "input".to_string(),
                msg: ensure_non_empty(e.to_string(), "Invalid input"),
            },
            PrivchatSDKError::NotConnected => Self::Disconnected,
            PrivchatSDKError::NotInitialized(_) => Self::NotInitialized,
            _ => Self::Generic {
                msg: ensure_non_empty(error_msg, "Unknown error"),
            },
        }
    }
}

impl From<anyhow::Error> for PrivchatError {
    fn from(error: anyhow::Error) -> Self {
        Self::generic(error)
    }
}

impl From<std::io::Error> for PrivchatError {
    fn from(error: std::io::Error) -> Self {
        Self::generic(error)
    }
}

// Helper for backward compatibility (UniFFI 0.31 migration)
impl PrivchatError {
    /// Check if error is a specific type (for backward compatibility)
    pub fn is_network_error(&self) -> bool {
        matches!(self, Self::Network { .. })
    }
    
    /// Check if error is authentication error
    pub fn is_authentication_error(&self) -> bool {
        matches!(self, Self::Authentication { .. })
    }
    
    /// Get error message (for backward compatibility)，保证非空
    pub fn message(&self) -> String {
        let s = match self {
            Self::Generic { msg } => msg.clone(),
            Self::Database { msg } => msg.clone(),
            Self::Network { msg, .. } => msg.clone(),
            Self::Authentication { reason } => reason.clone(),
            Self::InvalidParameter { field, msg } => format!("{}: {}", field, msg),
            Self::Timeout { timeout_secs } => format!("Operation timed out after {}s", timeout_secs),
            Self::Disconnected => "Client is disconnected".to_string(),
            Self::NotInitialized => "SDK not initialized".to_string(),
        };
        ensure_non_empty(s, "Unknown error")
    }
}
