use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use bytes::Bytes;
use rusqlite::Connection;
use uuid::Uuid;
use msgtrans::{Transport, transport::TransportOptions};
use crate::error::{PrivchatSDKError, Result};
use privchat_protocol::{MessageType, encode_message, decode_message};
use privchat_protocol::{ConnectRequest, ConnectResponse, DisconnectRequest, SendRequest, SubscribeRequest, PingRequest};

/// 用户会话信息
#[derive(Debug, Clone)]
pub struct UserSession {
    pub user_id: String,
    pub token: String,
    pub device_id: String,
    pub login_time: chrono::DateTime<chrono::Utc>,
    pub server_key: Option<String>,
    pub node_id: Option<String>,
}

/// Privchat 客户端主结构
pub struct PrivchatClient {
    /// SDK 工作根目录
    work_dir: PathBuf,
    /// 当前用户目录（连接后创建）
    user_dir: Option<PathBuf>,
    /// 当前用户ID（服务器返回）
    user_id: Option<String>,
    /// 加密数据库连接（延迟初始化）
    db: Option<Arc<Mutex<Connection>>>,
    /// 用户会话信息
    session: Option<UserSession>,
    /// 传输层连接
    transport: Arc<Transport>,
    /// 连接状态
    connected: Arc<RwLock<bool>>,
}

impl PrivchatClient {
    /// 创建新的 PrivchatClient 实例
    /// 
    /// # 参数
    /// - `work_dir`: SDK 工作根目录，所有用户数据都会存储在此目录下
    /// - `transport`: msgtrans 传输层实例
    /// 
    /// # 返回
    /// - `Result<Self, PrivchatSDKError>`: 客户端实例
    pub async fn new(work_dir: impl AsRef<Path>, transport: Arc<Transport>) -> Result<Self> {
        let work_dir = work_dir.as_ref().to_path_buf();
        
        // 确保工作目录存在
        tokio::fs::create_dir_all(&work_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建工作目录失败: {}", e)))?;
        
        Ok(Self {
            work_dir,
            user_dir: None,
            user_id: None,
            db: None,
            session: None,
            transport,
            connected: Arc::new(RwLock::new(false)),
        })
    }

    /// 连接到服务器并登录
    /// 
    /// # 参数
    /// - `login`: 登录凭证（手机号、用户名等）
    /// - `token`: 认证令牌（密码、短信验证码等）
    /// 
    /// # 返回
    /// - `Result<(), PrivchatSDKError>`: 连接结果
    pub async fn connect(&mut self, login: &str, token: &str) -> Result<()> {
        // 创建连接请求
        let device_id = format!("device_{}", Uuid::new_v4());
        let connect_msg = ConnectRequest {
            version: 1,
            device_id: device_id.clone(),
            device_flag: 1,
            client_timestamp: chrono::Utc::now().timestamp_millis(),
            uid: login.to_string(),
            token: token.to_string(),
            client_key: format!("key_{}", Uuid::new_v4()),
        };

        // 发送连接请求并等待响应
        let response_data = self.request_message(MessageType::ConnectRequest, &connect_msg).await?;
        
        // 解码连接响应
        let connect_response: ConnectResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Transport(format!("解码连接响应失败: {}", e)))?;

        // 检查连接是否成功
        if connect_response.reason_code != 0 {
            return Err(PrivchatSDKError::Auth(format!("连接失败，错误码: {}", connect_response.reason_code)));
        }

        // 从响应中获取真实的用户ID
        let user_id = format!("u_{}", connect_response.node_id);
        
        // 创建用户会话
        let session = UserSession {
            user_id: user_id.clone(),
            token: token.to_string(),
            device_id,
            login_time: chrono::Utc::now(),
            server_key: Some(connect_response.server_key),
            node_id: Some(connect_response.node_id.to_string()),
        };

        // 创建用户目录
        let user_dir = self.work_dir.join(&user_id);
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;

        // 创建子目录
        let subdirs = ["messages", "media", "media/images", "media/videos", "media/audios", "files", "cache"];
        for subdir in subdirs {
            let subdir_path = user_dir.join(subdir);
            tokio::fs::create_dir_all(&subdir_path).await
                .map_err(|e| PrivchatSDKError::IO(format!("创建子目录 {} 失败: {}", subdir, e)))?;
        }

        // 初始化加密数据库
        let db_path = user_dir.join("messages.db");
        let db_conn = self.create_encrypted_database(&db_path, &user_id).await?;

        // 更新状态
        self.user_id = Some(user_id);
        self.user_dir = Some(user_dir);
        self.session = Some(session);
        self.db = Some(db_conn);

        let mut connected = self.connected.write().await;
        *connected = true;

        tracing::info!("用户连接成功: {}", login);
        
        Ok(())
    }

    /// 断开连接
    pub async fn disconnect(&mut self, reason: &str) -> Result<()> {
        let connected = self.connected.read().await;
        if !*connected {
            return Err(PrivchatSDKError::NotConnected);
        }
        drop(connected);

        let disconnect_msg = DisconnectRequest {
            reason_code: 0,
            reason: reason.to_string(),
        };

        // 发送断开连接请求
        let _response = self.request_message(MessageType::DisconnectRequest, &disconnect_msg).await?;

        // 清理状态
        self.user_id = None;
        self.user_dir = None;
        self.session = None;
        // 关闭加密数据库连接
        self.db = None;

        let mut connected = self.connected.write().await;
        *connected = false;

        tracing::info!("用户断开连接: {}", reason);
        
        Ok(())
    }

    /// 发送聊天消息
    pub async fn send_chat_message(&self, message: &SendRequest) -> Result<Vec<u8>> {
        self.ensure_connected().await?;
        
        // 保存消息到数据库
        self.save_message_to_db(message).await?;
        
        // 发送消息
        let response = self.request_message(MessageType::SendRequest, message).await?;
        
        tracing::debug!("发送聊天消息成功");
        Ok(response)
    }

    /// 订阅频道
    pub async fn subscribe_channel(&self, channel_id: &str) -> Result<Vec<u8>> {
        self.ensure_connected().await?;
        
        let subscribe_msg = SubscribeRequest {
            setting: 1,
            client_msg_no: format!("sub_{}", Uuid::new_v4()),
            channel_id: channel_id.to_string(),
            channel_type: 1,
            action: 1,
            param: "".to_string(),
        };

        let response = self.request_message(MessageType::SubscribeRequest, &subscribe_msg).await?;
        
        tracing::debug!("订阅频道成功: {}", channel_id);
        Ok(response)
    }

    /// 发送心跳
    pub async fn send_ping(&self) -> Result<Vec<u8>> {
        self.ensure_connected().await?;
        
        let ping_msg = PingRequest {
            timestamp: chrono::Utc::now().timestamp_millis(),
        };

        let response = self.request_message(MessageType::PingRequest, &ping_msg).await?;
        
        tracing::debug!("心跳发送成功");
        Ok(response)
    }

    /// 获取当前用户ID
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }

    /// 获取用户目录
    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }

    /// 获取会话信息
    pub fn session(&self) -> Option<&UserSession> {
        self.session.as_ref()
    }

    /// 检查连接状态
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }

    /// 获取数据库连接
    pub fn database(&self) -> Option<&Arc<Mutex<Connection>>> {
        self.db.as_ref()
    }

    // 私有方法

    /// 确保客户端已连接
    async fn ensure_connected(&self) -> Result<()> {
        let connected = self.connected.read().await;
        if !*connected {
            return Err(PrivchatSDKError::NotConnected);
        }
        Ok(())
    }

    /// 发送请求消息（需要响应）
    async fn request_message<T: serde::Serialize>(&self, message_type: MessageType, message: &T) -> Result<Vec<u8>> {
        let encoded_data = encode_message(message)
            .map_err(|e| PrivchatSDKError::Transport(format!("编码失败: {}", e)))?;
        
        // 使用 msgtrans 的 request 方法，设置 biz_type
        let options = TransportOptions::new().with_biz_type(message_type as u8);
        
        // 发送请求并等待响应
        let response = self.transport.request_with_options(Bytes::from(encoded_data), options).await
            .map_err(|e| PrivchatSDKError::Transport(format!("请求失败: {}", e)))?;
        
        Ok(response.to_vec())
    }

    /// 创建加密数据库
    async fn create_encrypted_database(&self, db_path: &Path, user_id: &str) -> Result<Arc<Mutex<Connection>>> {
        let db_path_str = db_path.to_str()
            .ok_or_else(|| PrivchatSDKError::Database("无效的数据库路径".to_string()))?;
        
        // 在异步上下文中，我们需要使用 spawn_blocking 来执行同步的数据库操作
        let user_id = user_id.to_string();
        let db_path_str = db_path_str.to_string();
        
        let conn = tokio::task::spawn_blocking(move || -> Result<Connection> {
            // 创建 SQLCipher 连接
            let conn = Connection::open(&db_path_str)?;
            
            // 设置加密密钥（使用用户ID派生）
            let encryption_key = Self::derive_encryption_key(&user_id);
            conn.pragma_update(None, "key", &encryption_key)?;
            
            // 创建数据库表
            Self::create_database_tables(&conn)?;
            
            Ok(conn)
        }).await
        .map_err(|e| PrivchatSDKError::Database(format!("数据库初始化任务失败: {}", e)))??;
        
        Ok(Arc::new(Mutex::new(conn)))
    }

    /// 派生加密密钥
    pub fn derive_encryption_key(user_id: &str) -> String {
        // 使用简单的密钥派生（生产环境中应使用 PBKDF2/Scrypt/Argon2）
        // 这里为了演示，使用 SHA256 哈希
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        
        let mut hasher = DefaultHasher::new();
        user_id.hash(&mut hasher);
        "privchat_".to_string() + &hasher.finish().to_string()
    }

    /// 创建数据库表
    pub fn create_database_tables(conn: &Connection) -> Result<()> {
        // 创建消息表
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT UNIQUE,
                channel_id TEXT NOT NULL,
                from_uid TEXT NOT NULL,
                message_type INTEGER NOT NULL,
                content TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            [],
        )?;

        // 创建频道表
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS channels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT UNIQUE NOT NULL,
                channel_name TEXT,
                channel_type INTEGER NOT NULL,
                subscribed_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            [],
        )?;

        // 创建设置表
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
            [],
        )?;

        Ok(())
    }

    /// 保存消息到加密数据库
    async fn save_message_to_db(&self, message: &SendRequest) -> Result<()> {
        let db = self.db.as_ref()
            .ok_or_else(|| PrivchatSDKError::Database("数据库未初始化".to_string()))?;
        
        let db = db.clone();
        let message = message.clone();
        
        tokio::task::spawn_blocking(move || -> Result<()> {
            let conn = db.lock()
                .map_err(|e| PrivchatSDKError::Database(format!("获取数据库锁失败: {}", e)))?;
            
            conn.execute(
                r#"
                INSERT INTO messages (message_id, channel_id, from_uid, message_type, content, timestamp)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                rusqlite::params![
                    message.client_msg_no,
                    message.channel_id,
                    message.from_uid,
                    MessageType::SendRequest as u8,
                    String::from_utf8_lossy(&message.payload),
                    chrono::Utc::now().timestamp_millis()
                ],
            )?;

            Ok(())
        }).await
        .map_err(|e| PrivchatSDKError::Database(format!("保存消息任务失败: {}", e)))??;

        Ok(())
    }
} 