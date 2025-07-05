//! Privchat SDK - 基于 msgtrans 的私聊通信 SDK
//! 
//! 本 SDK 提供了基于 msgtrans 传输层架构的私聊功能，包括：
//! - 用户认证和连接管理
//! - 消息发送和接收
//! - 数据库存储和缓存
//! - 媒体文件管理
//! - 心跳机制
//! - 订阅功能
//! 
//! # 基本使用
//! 
//! ```rust,no_run
//! use privchat_sdk::PrivchatClient;
//! use std::sync::Arc;
//! 
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // 创建传输层实例（实际使用中需要真实的Transport）
//!     // let transport = Arc::new(Transport::new(...));
//!     
//!     // 创建客户端
//!     // let mut client = PrivchatClient::new("/path/to/data", transport).await?;
//!     
//!     // 连接并登录
//!     // client.connect("phone_or_username", "password_or_token").await?;
//!     
//!     // 发送消息
//!     // let message = SendRequest::new();
//!     // client.send_chat_message(&message).await?;
//!     
//!     Ok(())
//! }
//! ```

// 导出核心模块
pub mod error;
pub mod client;
pub mod storage;

// 重新导出核心类型，方便使用
pub use client::{PrivchatClient, UserSession};
pub use error::{PrivchatSDKError, Result};
pub use storage::{StorageManager, StorageStats};

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
        let user_id = "test_user_123";
        let key1 = PrivchatClient::derive_encryption_key(user_id);
        let key2 = PrivchatClient::derive_encryption_key(user_id);
        
        // 相同用户ID应该生成相同的密钥
        assert_eq!(key1, key2);
        
        // 不同用户ID应该生成不同的密钥
        let different_key = PrivchatClient::derive_encryption_key("different_user");
        assert_ne!(key1, different_key);
        
        // 密钥应该有前缀
        assert!(key1.starts_with("privchat_"));
        
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
        let encryption_key = PrivchatClient::derive_encryption_key("test_user");
        
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
        let encryption_key = PrivchatClient::derive_encryption_key("test_user");
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