//! Privchat SDK - 现代化即时通讯 SDK
//! 
//! 本 SDK 提供了完整的即时通讯功能，包括：
//! - 🔗 消息发送和接收队列系统
//! - 📡 网络状态监控和智能重试
//! - 🧠 高级特性：已读回执、消息撤回、消息编辑
//! - 💬 实时交互：输入状态指示器、表情反馈
//! - ⚙️ 事件系统：统一的事件管理和回调机制
//! - 🔐 数据安全：SQLCipher 加密存储
//! - 🧵 并发安全：异步优先设计，支持多线程
//! 
//! # 快速开始
//! 
//! ```rust,no_run
//! use privchat_sdk::{PrivchatSDK, PrivchatConfig};
//! 
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 配置 SDK
//!     let config = PrivchatConfig::builder()
//!         .data_dir("/path/to/data")
//!         .user_id("user123")
//!         .server_url("wss://chat.example.com")
//!         .build();
//!     
//!     // 初始化 SDK
//!     let sdk = PrivchatSDK::initialize(config).await?;
//!     
//!     // 注册事件回调
//!     sdk.on_message_received(|message| {
//!         println!("收到消息: {}", message.content);
//!     });
//!     
//!     // 发送消息
//!     let message_id = sdk.send_message("Hello, World!", "session123").await?;
//!     
//!     // 标记已读
//!     sdk.mark_as_read("session123", message_id).await?;
//!     
//!     // 关闭 SDK
//!     sdk.shutdown().await?;
//!     
//!     Ok(())
//! }
//! ```

// 导出核心模块
pub mod error;
pub mod version;
pub mod client;
pub mod storage;
pub mod network;
pub mod events;
pub mod sdk;
pub mod message_type;
pub mod rpc_client;
pub mod utils;
pub mod connection_state;
pub mod presence;
pub mod typing;
pub mod sync;
pub mod rate_limiter;
pub mod http_client;
pub mod lifecycle;

// 重新导出核心类型，方便使用
pub use error::{PrivchatSDKError, Result};
pub use client::{
    PrivchatClient, UserSession,
    RpcResult, RPCMessageRequest, RPCMessageResponse
};
pub use sdk::{PrivchatSDK, PrivchatConfig, ServerConfig, ServerEndpoint, TransportProtocol, HttpClientConfig};
pub use storage::media_preprocess::{VideoProcessHook, MediaProcessOp, SendMode};
pub use http_client::{FileHttpClient, FileUploadResponse};
pub use message_type::{ChatMessageType, ParsedMessage, message_type_from_u32, message_type_str_to_u32};
pub use rpc_client::RpcClientExt;
pub use utils::{TimeFormatter, TimezoneConfig};
pub use connection_state::{
    ConnectionState, ConnectionStateManager, ConnectionProtocol, 
    ConnectionStatus, ServerInfo, UserInfo, PerformanceStats
};
pub use presence::{PresenceManager, PresenceCacheConfig, PresenceCacheStats};
pub use typing::{TypingManager, TypingConfig, TypingStats};
pub use rate_limiter::{
    MessageRateLimiter, MessageRateLimiterConfig, MessageRateLimiterStats,
    RpcRateLimiter, RpcRateLimiterConfig, RpcRateLimiterStats, RpcRateLimitError,
    RpcRequestKey, ReconnectRateLimiter, ReconnectRateLimiterConfig, ReconnectRateLimiterStats,
};
pub use lifecycle::{LifecycleManager, LifecycleHook};
pub use sync::{EntityType, EntitySyncEngine, SyncCursorStore, run_bootstrap_sync, BOOTSTRAP_ENTITY_TYPES};

// 重新导出协议层的类型，避免用户需要单独导入 privchat-protocol
pub use privchat_protocol::*;

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::TempDir;

    #[test]
    fn test_encryption_key_derivation() {
        // 测试密钥派生功能
        let user_id = 123;
        let key1 = PrivchatClient::derive_encryption_key(user_id);
        let key2 = PrivchatClient::derive_encryption_key(user_id);
        
        // 相同用户ID应该生成相同的密钥
        assert_eq!(key1, key2);
        
        // 不同用户ID应该生成不同的密钥
        let different_key = PrivchatClient::derive_encryption_key(456);
        assert_ne!(key1, different_key);
        
        // 密钥应该有前缀
        assert!(key1.starts_with("encryption_key_"));
        
        println!("✅ 密钥派生测试通过");
        println!("   用户ID: {}", user_id);
        println!("   派生密钥: {}", key1);
    }

    #[test]
    fn test_sqlcipher_database() {
        // 测试 SQLCipher 加密数据库
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_encrypted.db");
        
        // 创建加密数据库
        let conn = Connection::open(&db_path).unwrap();
        let encryption_key = PrivchatClient::derive_encryption_key(999);
        
        // 设置加密密钥
        conn.pragma_update(None, "key", &encryption_key).unwrap();
        
        // 创建表
        conn.execute(
            "CREATE TABLE test (id INTEGER PRIMARY KEY, data TEXT)",
            [],
        ).unwrap();
        
        // 插入数据
        conn.execute(
            "INSERT INTO test (data) VALUES (?1)",
            ["加密的测试数据"],
        ).unwrap();
        
        // 查询数据
        let data: String = {
            let mut stmt = conn.prepare("SELECT data FROM test WHERE id = 1").unwrap();
            stmt.query_row([], |row| row.get(0)).unwrap()
        };
        
        assert_eq!(data, "加密的测试数据");
        
        // 关闭连接
        drop(conn);
        
        // 验证数据库文件已创建
        assert!(db_path.exists());
        
        println!("✅ SQLCipher 数据库测试通过");
        println!("   数据库路径: {}", db_path.display());
        println!("   成功写入和读取加密数据");
    }

    #[test]
    fn test_database_tables_creation() {
        // 测试数据库表创建
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test_tables.db");
        
        let conn = Connection::open(&db_path).unwrap();
        let encryption_key = PrivchatClient::derive_encryption_key(999);
        conn.pragma_update(None, "key", &encryption_key).unwrap();
        
        // 创建数据库表
        PrivchatClient::create_database_tables(&conn).unwrap();
        
        // 验证表是否存在
        let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'").unwrap();
        let table_rows = stmt.query_map([], |row| row.get(0)).unwrap();
        let mut tables = Vec::new();
        for table_result in table_rows {
            tables.push(table_result.unwrap());
        }
        
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"channels".to_string()));
        assert!(tables.contains(&"settings".to_string()));
        
        println!("✅ 数据库表创建测试通过");
        println!("   创建的表: {:?}", tables);
    }
}