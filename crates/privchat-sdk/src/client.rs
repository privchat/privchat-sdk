use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::RwLock;
use bytes::Bytes;
use rusqlite::Connection;
use uuid::Uuid;
use msgtrans::{Transport, transport::TransportOptions};
use crate::error::{PrivchatSDKError, Result};
use privchat_protocol::{encode_message, decode_message};
use privchat_protocol::{ConnectRequest, ConnectResponse, DisconnectRequest, SubscribeRequest, PingRequest};

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

/// Privchat 客户端 - 连接与会话管理层
/// 
/// 职责范围：
/// - 网络连接与认证
/// - 用户目录和数据库初始化
/// - 会话状态管理
/// - 基础传输层协议处理
/// 
/// 不负责：
/// - 消息发送逻辑（由 MessageSender 处理）
/// - 高级功能（由 AdvancedFeaturesManager 处理）
/// - 事件管理（由 EventManager 处理）
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
    /// 创建新的客户端实例
    pub async fn new<P: AsRef<Path>>(work_dir: P, transport: Arc<Transport>) -> Result<Self> {
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
    
    /// 连接并认证到服务器
    pub async fn connect(&mut self, phone: &str, token: &str) -> Result<UserSession> {
        tracing::info!("正在连接到服务器，手机号: {}", phone);
        
        // 1. 发送认证请求（传输层连接由外部管理）
        let connect_request = ConnectRequest {
            version: 1,
            device_id: self.generate_device_id(),
            device_flag: 1,
            client_timestamp: chrono::Utc::now().timestamp_millis(),
            uid: phone.to_string(),
            token: token.to_string(),
            client_key: format!("key_{}", Uuid::new_v4()),
        };
        
        let request_data = encode_message(&connect_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码连接请求失败: {}", e)))?;
        
        // 发送请求
        let response_data = self.transport.request_with_options(
            Bytes::from(request_data),
            TransportOptions::new()
        ).await
        .map_err(|e| PrivchatSDKError::Transport(format!("发送连接请求失败: {}", e)))?;
        
        // 2. 解析响应
        let connect_response: ConnectResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码连接响应失败: {}", e)))?;
        
        if connect_response.reason_code != 0 {
            return Err(PrivchatSDKError::Auth(format!("认证失败，错误码: {}", connect_response.reason_code)));
        }
        
        // 3. 创建用户会话
        let user_id = format!("u_{}", connect_response.node_id);
        let session = UserSession {
            user_id: user_id.clone(),
            token: token.to_string(),
            device_id: connect_request.device_id.clone(),
            login_time: chrono::Utc::now(),
            server_key: Some(connect_response.server_key),
            node_id: Some(connect_response.node_id.to_string()),
        };
        
        // 4. 初始化用户环境
        self.initialize_user_environment(&session).await?;
        
        // 5. 更新状态
        self.user_id = Some(session.user_id.clone());
        self.session = Some(session.clone());
        *self.connected.write().await = true;
        
        tracing::info!("连接成功，用户ID: {}", session.user_id);
        Ok(session)
    }
    
    /// 断开连接
    pub async fn disconnect(&mut self, reason: &str) -> Result<()> {
        if !self.is_connected().await {
            return Ok(());
        }
        
        tracing::info!("正在断开连接，原因: {}", reason);
        
        // 发送断开连接请求
        let disconnect_request = DisconnectRequest {
            reason_code: 0,
            reason: reason.to_string(),
        };
        
        let request_data = encode_message(&disconnect_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码断开请求失败: {}", e)))?;
        
        // 发送断开请求（忽略错误，因为可能网络已断开）
        let _ = self.transport.send_with_options(
            Bytes::from(request_data),
            TransportOptions::new()
        ).await;
        
        // 清理状态
        self.session = None;
        self.user_id = None;
        self.db = None;
        self.user_dir = None;
        *self.connected.write().await = false;
        
        tracing::info!("连接已断开");
        Ok(())
    }
    
    /// 检查是否已连接
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }
    
    /// 获取当前用户ID
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
    
    /// 获取用户会话信息
    pub fn session(&self) -> Option<&UserSession> {
        self.session.as_ref()
    }
    
    /// 获取用户数据目录
    pub fn user_dir(&self) -> Option<&Path> {
        self.user_dir.as_deref()
    }
    
    /// 获取数据库连接（供上层模块使用）
    pub fn database(&self) -> Option<Arc<Mutex<Connection>>> {
        self.db.clone()
    }
    
    /// 获取传输层引用（供上层模块使用）
    pub fn transport(&self) -> &Arc<Transport> {
        &self.transport
    }
    
    /// 发送心跳
    pub async fn ping(&self) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let ping_request = PingRequest {
            timestamp: chrono::Utc::now().timestamp_millis(),
        };
        
        let request_data = encode_message(&ping_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码心跳请求失败: {}", e)))?;
        
        let response_data = self.transport.request_with_options(
            Bytes::from(request_data),
            TransportOptions::new()
        ).await
        .map_err(|e| PrivchatSDKError::Transport(format!("发送心跳失败: {}", e)))?;
        
        let _pong_response: privchat_protocol::PongResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码心跳响应失败: {}", e)))?;
        
        tracing::debug!("心跳成功");
        Ok(())
    }
    
    /// 订阅频道
    pub async fn subscribe_channel(&self, channel_id: &str) -> Result<()> {
        if !self.is_connected().await {
            return Err(PrivchatSDKError::NotConnected);
        }
        
        let subscribe_request = SubscribeRequest {
            setting: 1,
            client_msg_no: format!("sub_{}", Uuid::new_v4()),
            channel_id: channel_id.to_string(),
            channel_type: 1,
            action: 1,
            param: "".to_string(),
        };
        
        let request_data = encode_message(&subscribe_request)
            .map_err(|e| PrivchatSDKError::Serialization(format!("编码订阅请求失败: {}", e)))?;
        
        let response_data = self.transport.request_with_options(
            Bytes::from(request_data),
            TransportOptions::new()
        ).await
        .map_err(|e| PrivchatSDKError::Transport(format!("发送订阅请求失败: {}", e)))?;
        
        let _subscribe_response: privchat_protocol::SubscribeResponse = decode_message(&response_data)
            .map_err(|e| PrivchatSDKError::Serialization(format!("解码订阅响应失败: {}", e)))?;
        
        tracing::info!("订阅频道成功: {}", channel_id);
        Ok(())
    }
    
    // ========== 内部方法 ==========
    
    /// 初始化用户环境
    async fn initialize_user_environment(&mut self, session: &UserSession) -> Result<()> {
        // 创建用户目录
        let user_dir = self.work_dir.join(format!("u_{}", session.user_id));
        tokio::fs::create_dir_all(&user_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建用户目录失败: {}", e)))?;
        
        // 创建子目录
        for subdir in ["messages", "media", "files", "cache"] {
            let dir_path = user_dir.join(subdir);
            tokio::fs::create_dir_all(&dir_path).await
                .map_err(|e| PrivchatSDKError::IO(format!("创建子目录失败 {}: {}", subdir, e)))?;
        }
        
        // 初始化加密数据库
        let db_path = user_dir.join("messages.db");
        let encryption_key = Self::derive_encryption_key(&session.user_id);
        
        let conn = Connection::open(&db_path)
            .map_err(|e| PrivchatSDKError::Database(format!("打开数据库失败: {}", e)))?;
        
        // 设置加密密钥
        conn.pragma_update(None, "key", &encryption_key)
            .map_err(|e| PrivchatSDKError::Database(format!("设置加密密钥失败: {}", e)))?;
        
        // 启用WAL模式和优化设置
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| PrivchatSDKError::Database(format!("设置WAL模式失败: {}", e)))?;
        
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| PrivchatSDKError::Database(format!("设置同步模式失败: {}", e)))?;
        
        // 创建数据库表
        Self::create_database_tables(&conn)?;
        
        // 保存状态
        self.user_dir = Some(user_dir);
        self.db = Some(Arc::new(Mutex::new(conn)));
        
        tracing::info!("用户环境初始化完成: {}", session.user_id);
        Ok(())
    }
    
    /// 生成设备ID
    fn generate_device_id(&self) -> String {
        format!("device_{}", Uuid::new_v4().to_string())
    }
    
    /// 派生加密密钥
    pub fn derive_encryption_key(user_id: &str) -> String {
        use sha2::{Digest, Sha256};
        
        let mut hasher = Sha256::new();
        hasher.update(b"privchat_encryption_key_");
        hasher.update(user_id.as_bytes());
        hasher.update(b"_v1");
        
        let result = hasher.finalize();
        format!("privchat_{}", hex::encode(result))
    }
    
    /// 创建数据库表
    pub fn create_database_tables(conn: &Connection) -> Result<()> {
        // 消息表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id TEXT NOT NULL UNIQUE,
                channel_id TEXT NOT NULL,
                from_uid TEXT NOT NULL,
                content TEXT NOT NULL,
                message_type INTEGER NOT NULL DEFAULT 0,
                status INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // 频道表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS channels (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel_id TEXT NOT NULL UNIQUE,
                channel_name TEXT NOT NULL,
                channel_type INTEGER NOT NULL DEFAULT 0,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // 设置表
        conn.execute(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // 创建索引
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id)",
            [],
        )?;
        
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at)",
            [],
        )?;
        
        tracing::debug!("数据库表创建完成");
        Ok(())
    }
}

impl Drop for PrivchatClient {
    fn drop(&mut self) {
        tracing::debug!("PrivchatClient 正在释放资源");
    }
} 