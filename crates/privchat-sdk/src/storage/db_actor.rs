//! 数据库 Actor - 单线程数据库访问模型
//!
//! 核心设计：
//! - SQLite Connection 永远只在一个专用线程中
//! - 所有数据库操作通过 channel 发送命令
//! - 无跨线程使用，无锁竞争
//! - 完全可预测的行为

use crossbeam_channel::{unbounded, Receiver, Sender};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use tracing::{debug, error, info, warn};

use crate::error::{PrivchatSDKError, Result};
use crate::storage::dao;
use crate::storage::entities::{self, Message};

/// 数据库命令
pub enum DbCommand {
    /// 初始化用户数据库
    InitUser {
        uid: String,
        db_path: PathBuf,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 执行 SQL 更新（通用）
    Execute {
        uid: String,
        sql: String,
        params: Vec<rusqlite::types::Value>,
        respond_to: tokio::sync::oneshot::Sender<Result<usize>>,
    },

    /// 更新消息状态（按 message.id）
    UpdateMessageStatus {
        uid: String,
        id: i64,
        status: i32,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 更新消息的服务端 ID（按 message.id，仅协议层写入）
    UpdateMessageServerId {
        uid: String,
        id: i64,
        server_message_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 保存消息（接收或发送）
    SaveReceivedMessage {
        uid: String,
        message: Message,
        is_outgoing: bool, // true=自己发送的消息，不增加 unread_count
        respond_to: tokio::sync::oneshot::Sender<Result<i64>>,
    },

    /// 查询（返回 JSON）
    Query {
        uid: String,
        sql: String,
        params: Vec<rusqlite::types::Value>,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<serde_json::Value>>>,
    },

    /// 根据 message.id 获取消息
    GetMessageById {
        uid: String,
        id: i64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<Message>>>,
    },

    /// 获取消息的 channel_id（按 message.id）
    GetMessageChannelId {
        uid: String,
        id: i64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<u64>>>,
    },

    /// 获取指定 id（message.id）之前的消息（分页查询，向后加载更早的消息）
    GetMessagesBefore {
        uid: String,
        channel_id: u64,
        before_id: u64,
        limit: u32,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<Message>>>,
    },

    /// 获取频道当前最小的 message.id（用于「加载更早」分页游标）
    GetEarliestId {
        uid: String,
        channel_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<u64>>>,
    },

    /// 获取指定 id（message.id）之后的消息（分页查询，向前加载更新的消息）
    GetMessagesAfter {
        uid: String,
        channel_id: u64,
        after_id: u64,
        limit: u32,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<Message>>>,
    },

    /// 发送消息（插入后返回 message.id）
    SendMessage {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
        message_type: i32,
        respond_to: tokio::sync::oneshot::Sender<Result<i64>>,
    },

    /// 撤回消息（按 message.id）
    RevokeMessage {
        uid: String,
        id: i64,
        revoker_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 编辑消息（按 message.id）
    EditMessage {
        uid: String,
        id: i64,
        new_content: String,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 删除消息（按 message.id）
    DeleteMessage {
        uid: String,
        id: i64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 更新消息内容（按 message.id）
    UpdateMessageContent {
        uid: String,
        id: i64,
        new_content: String,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 添加消息反应（按 message.id）
    AddMessageReaction {
        uid: String,
        id: i64,
        user_id: u64,
        reaction: String,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 保存 Channel
    SaveChannel {
        uid: String,
        channel: entities::Channel,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 获取会话列表
    GetChannels {
        uid: String,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<entities::Channel>>>,
    },

    /// 根据频道获取会话
    GetChannelByChannel {
        uid: String,
        channel_id: u64,
        channel_type: u8,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<entities::Channel>>>,
    },

    /// 按 channel_id 查询私聊会话（channel_type 0 或 1 均视为同一私聊，避免重复插入导致列表两条）
    GetDirectChannelById {
        uid: String,
        channel_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<entities::Channel>>>,
    },

    /// 更新会话的 pts
    UpdateChannelPts {
        uid: String,
        channel_id: u64,
        channel_type: u8,
        new_pts: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 根据用户查找 channel_id
    FindChannelIdByUser {
        uid: String,
        target_user_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<u64>>>,
    },

    /// 更新频道的 save 字段（收藏状态）
    UpdateChannelSave {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        save: i32,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 更新频道的 mute 字段（通知模式）
    UpdateChannelMute {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        mute: i32,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 更新会话的 extra 字段
    UpdateChannelExtra {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        extra: String,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    // ========== User / Group / GroupMember（Entity Model V1）==========
    SaveUser {
        uid: String,
        user: entities::User,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    SaveUsers {
        uid: String,
        users: Vec<entities::User>,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },
    GetUser {
        uid: String,
        user_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<entities::User>>>,
    },
    GetUsersByIds {
        uid: String,
        ids: Vec<u64>,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<entities::User>>>,
    },

    // ========== 好友管理命令（Local-first）==========
    /// 保存单个好友
    SaveFriend {
        uid: String,
        friend: entities::Friend,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 批量保存好友
    SaveFriends {
        uid: String,
        friends: Vec<entities::Friend>,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 获取好友列表（分页）
    GetFriends {
        uid: String,
        limit: u32,
        offset: u32,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<entities::Friend>>>,
    },

    /// 获取好友总数
    GetFriendsCount {
        uid: String,
        respond_to: tokio::sync::oneshot::Sender<Result<u32>>,
    },

    /// 删除好友（user_id = 好友的 user_id）
    DeleteFriend {
        uid: String,
        user_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 保存频道成员
    SaveChannelMember {
        uid: String,
        member: entities::ChannelMember,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 批量保存频道成员
    SaveChannelMembers {
        uid: String,
        members: Vec<entities::ChannelMember>,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 获取群成员列表（按 group_id 关联，channel_member 中 channel_id=group_id, channel_type=2）
    GetGroupMembers {
        uid: String,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<entities::ChannelMember>>>,
    },

    /// 删除频道成员（含群成员 tombstone）
    DeleteChannelMember {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 获取群列表（分页，group 表）
    GetGroups {
        uid: String,
        limit: u32,
        offset: u32,
        respond_to: tokio::sync::oneshot::Sender<Result<Vec<entities::Group>>>,
    },

    /// 按 group_id 获取单个群（ENTITY_SYNC_V1 group tombstone 等）
    GetGroup {
        uid: String,
        group_id: u64,
        respond_to: tokio::sync::oneshot::Sender<Result<Option<entities::Group>>>,
    },

    /// 批量保存群（ENTITY_SYNC_V1 group 同步）
    SaveGroups {
        uid: String,
        groups: Vec<entities::Group>,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 关闭特定用户的数据库
    CloseUser {
        uid: String,
        respond_to: tokio::sync::oneshot::Sender<Result<()>>,
    },

    /// 停止 Actor
    Shutdown,
}

/// 数据库 Actor（运行在独立线程）
#[allow(dead_code)]
pub struct DbActor {
    /// 所有用户的数据库连接（每个用户一个连接）
    connections: std::collections::HashMap<String, Connection>,
    /// 接收命令的 channel
    receiver: Receiver<DbCommand>,
    /// 当前线程 ID（用于调试）
    thread_id: thread::ThreadId,
    /// assets 目录路径（存放 SQL 迁移文件）
    assets_path: Option<PathBuf>,
    /// Snowflake ID 生成器
    snowflake: Arc<snowflake_me::Snowflake>,
}

impl DbActor {
    /// 创建新的 DbActor
    fn new(receiver: Receiver<DbCommand>, assets_path: Option<PathBuf>) -> Self {
        let thread_id = thread::current().id();
        info!(
            "🚀 [Thread {:?}] DbActor 已启动, assets_path={:?}",
            thread_id, assets_path
        );

        // 初始化 Snowflake ID 生成器
        // 注意：使用 StdRng 而不是 thread_rng()，以保持一致性（虽然这里是同步方法，thread_rng 也可以工作）
        use rand::rngs::StdRng;
        use rand::{Rng, SeedableRng};
        let mut rng = StdRng::from_entropy();
        let machine_id: u16 = rng.gen_range(0..32);
        let data_center_id: u16 = rng.gen_range(0..32);

        let snowflake = snowflake_me::Snowflake::builder()
            .machine_id(&|| Ok(machine_id))
            .data_center_id(&|| Ok(data_center_id))
            .finalize()
            .expect("初始化 Snowflake 失败");

        Self {
            connections: std::collections::HashMap::new(),
            receiver,
            thread_id,
            assets_path,
            snowflake: Arc::new(snowflake),
        }
    }

    /// 生成数据库加密密钥（基于 UID）
    fn derive_encryption_key(uid: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"privchat_sdk_encryption_key_v1");
        hasher.update(uid.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }

    /// 执行数据库迁移
    ///
    /// - assets_path 为 None：使用 refinery embed_migrations!（自动扫描 migrations/，按 V1、V2... 执行）
    /// - assets_path 有值：使用外部 assets 目录（开发/覆盖）
    fn migrate_database(&self, conn: &mut Connection, uid: &str) -> Result<()> {
        if self.assets_path.is_some() {
            // 外部 assets 路径：保留原有逻辑（scan_external_assets + execute_batch）
            let current_version = self.get_current_version(conn)?;
            match current_version {
                None => {
                    info!(
                        "🆕 [DbActor Thread {:?}] 全新数据库，执行外部 assets: uid={}",
                        self.thread_id, uid
                    );
                    self.execute_init_sql(conn)?;
                }
                Some(version) => {
                    info!(
                        "📌 [DbActor Thread {:?}] 当前版本: {}, 检查增量: uid={}",
                        self.thread_id, version, uid
                    );
                    self.execute_migrations(conn, &version)?;
                }
            }
        } else {
            // 内置：refinery 自动管理（无需手写 BUILTIN_MIGRATIONS）
            info!(
                "📦 [DbActor Thread {:?}] 使用 refinery embedded migrations: uid={}",
                self.thread_id, uid
            );
            crate::storage::migrate::run_migrations(conn)?;
        }
        Ok(())
    }

    /// 获取当前数据库版本
    fn get_current_version(&self, conn: &Connection) -> Result<Option<String>> {
        // 检查 schema_version 表是否存在
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='schema_version'",
            [],
            |row| row.get(0)
        ).unwrap_or(false);

        if !table_exists {
            return Ok(None);
        }

        // 获取最新版本
        match conn.query_row::<String, _, _>(
            "SELECT version FROM schema_version ORDER BY applied_at DESC LIMIT 1",
            [],
            |row| row.get(0),
        ) {
            Ok(version) => Ok(Some(version)),
            Err(_) => Ok(None),
        }
    }

    /// 执行初始化 SQL
    ///
    /// 从 assets 目录扫描所有 SQL 文件，按时间顺序全部执行
    /// 执行完成后，数据库版本会更新为最后一个文件的版本号
    fn execute_init_sql(&self, conn: &Connection) -> Result<()> {
        // 扫描所有 SQL 文件（已按版本号排序）
        let all_migrations = self.scan_all_migration_files()?;

        if all_migrations.is_empty() {
            return Err(PrivchatSDKError::Database(
                "未找到初始化SQL文件，请确保 assets 目录中有 SQL 文件".to_string(),
            ));
        }

        info!(
            "🔄 [DbActor Thread {:?}] 开始初始化数据库，共 {} 个SQL文件",
            self.thread_id,
            all_migrations.len()
        );

        // 按顺序执行所有 SQL 文件
        for (version, sql_content) in all_migrations {
            info!(
                "📄 [DbActor Thread {:?}] 执行初始化SQL: {}",
                self.thread_id, version
            );

            conn.execute_batch(&sql_content).map_err(|e| {
                error!(
                    "❌ [DbActor Thread {:?}] 执行SQL失败: {}, error={}",
                    self.thread_id, version, e
                );
                PrivchatSDKError::Database(format!("执行SQL {}失败: {}", version, e))
            })?;

            // 记录版本（如果 SQL 文件中没有 INSERT schema_version）
            let _ = conn.execute(
                "INSERT OR IGNORE INTO schema_version (version) VALUES (?1)",
                params![&version],
            );

            info!(
                "✅ [DbActor Thread {:?}] SQL执行成功: {}",
                self.thread_id, version
            );
        }

        info!("✅ [DbActor Thread {:?}] 数据库初始化完成", self.thread_id);
        Ok(())
    }

    /// 执行增量迁移
    fn execute_migrations(&self, conn: &Connection, current_version: &str) -> Result<()> {
        // 扫描所有 SQL 文件
        let all_migrations = self.scan_all_migration_files()?;

        // 提取版本号（去掉描述部分），只比较时间戳部分
        let current_version_prefix = Self::extract_version_prefix(current_version);

        // 过滤出比当前版本新的迁移
        let pending_migrations: Vec<_> = all_migrations
            .into_iter()
            .filter(|(version, _)| {
                let version_prefix = Self::extract_version_prefix(version);
                version_prefix > current_version_prefix
            })
            .collect();

        if pending_migrations.is_empty() {
            info!(
                "✓ [DbActor Thread {:?}] 数据库已是最新版本: {}",
                self.thread_id, current_version
            );
            return Ok(());
        }

        info!(
            "🔄 [DbActor Thread {:?}] 发现 {} 个待执行的迁移脚本",
            self.thread_id,
            pending_migrations.len()
        );

        // 依次执行迁移
        for (version, sql_content) in pending_migrations {
            info!(
                "📄 [DbActor Thread {:?}] 执行迁移: {}",
                self.thread_id, version
            );

            conn.execute_batch(&sql_content).map_err(|e| {
                error!(
                    "❌ [DbActor Thread {:?}] 执行迁移失败: {}, error={}",
                    self.thread_id, version, e
                );
                PrivchatSDKError::Database(format!("执行迁移{}失败: {}", version, e))
            })?;

            // 记录版本（如果 SQL 文件中没有 INSERT schema_version）
            let _ = conn.execute(
                "INSERT OR IGNORE INTO schema_version (version) VALUES (?1)",
                params![&version],
            );

            info!(
                "✅ [DbActor Thread {:?}] 迁移执行成功: {}",
                self.thread_id, version
            );
        }

        Ok(())
    }

    /// 扫描迁移文件（仅用于外部 assets_path 模式）
    ///
    /// 调用前需保证 assets_path 已设置；内置模式使用 refinery 自动管理。
    fn scan_all_migration_files(&self) -> Result<Vec<(String, String)>> {
        let assets_path = self
            .assets_path
            .as_ref()
            .expect("scan_all_migration_files 仅在 assets_path 模式下调用");
        info!(
            "📦 [DbActor Thread {:?}] 使用外部 assets 目录: {}",
            self.thread_id,
            assets_path.display()
        );
        self.scan_external_assets(assets_path)
    }

    /// 从外部 assets 目录扫描 SQL 文件
    fn scan_external_assets(&self, assets_path: &PathBuf) -> Result<Vec<(String, String)>> {
        use std::collections::BTreeMap;

        if !assets_path.exists() {
            return Err(PrivchatSDKError::IO(format!(
                "assets 目录不存在: {}，请确保该目录存在并包含 SQL 迁移文件",
                assets_path.display()
            )));
        }

        let mut migrations = BTreeMap::new();
        let entries = std::fs::read_dir(assets_path)
            .map_err(|e| PrivchatSDKError::IO(format!("读取 assets 目录失败: {}", e)))?;

        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.is_file() {
                    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                        if file_name.ends_with(".sql") {
                            let version = file_name.trim_end_matches(".sql").to_string();
                            if Self::is_valid_version(&version) {
                                let sql_content = std::fs::read_to_string(&path).map_err(|e| {
                                    PrivchatSDKError::IO(format!(
                                        "读取 SQL 文件失败 {}: {}",
                                        file_name, e
                                    ))
                                })?;
                                info!(
                                    "📁 [DbActor Thread {:?}] 发现外部迁移文件: {}",
                                    self.thread_id, version
                                );
                                migrations.insert(version, sql_content);
                            }
                        }
                    }
                }
            }
        }

        if migrations.is_empty() {
            return Err(PrivchatSDKError::Database(
                "assets 目录中未找到有效的 SQL 迁移文件".to_string(),
            ));
        }

        Ok(migrations.into_iter().collect())
    }

    /// 验证版本号格式
    ///
    /// 有效格式：
    /// - 至少 14 位数字（YYYYMMDDHHMMSS）
    /// - 可选的下划线和描述（_description）
    fn is_valid_version(version: &str) -> bool {
        // 提取时间戳部分
        let timestamp_part = version.split('_').next().unwrap_or(version);

        // 必须是 14 位数字
        timestamp_part.len() >= 14 && timestamp_part.chars().take(14).all(|c| c.is_ascii_digit())
    }

    /// 提取版本号的时间戳前缀（用于比较）
    ///
    /// 例如：20240101000001_init -> 20240101000001
    fn extract_version_prefix(version: &str) -> &str {
        version.split('_').next().unwrap_or(version)
    }

    /// 运行 Actor 主循环
    fn run(mut self) {
        info!("🔄 [Thread {:?}] DbActor 开始处理命令", self.thread_id);

        while let Ok(command) = self.receiver.recv() {
            match command {
                DbCommand::Shutdown => {
                    info!("🛑 [Thread {:?}] DbActor 收到停止信号", self.thread_id);
                    break;
                }

                DbCommand::InitUser {
                    uid,
                    db_path,
                    respond_to,
                } => {
                    let result = self.handle_init_user(&uid, &db_path);
                    let _ = respond_to.send(result);
                }

                DbCommand::Execute {
                    uid,
                    sql,
                    params,
                    respond_to,
                } => {
                    let result = self.handle_execute(&uid, &sql, &params);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateMessageStatus {
                    uid,
                    id,
                    status,
                    respond_to,
                } => {
                    let result = self.handle_update_message_status(&uid, id, status);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateMessageServerId {
                    uid,
                    id,
                    server_message_id,
                    respond_to,
                } => {
                    let result = self.handle_update_message_server_id(&uid, id, server_message_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveReceivedMessage {
                    uid,
                    message,
                    is_outgoing,
                    respond_to,
                } => {
                    let result = self.handle_save_received_message(&uid, &message, is_outgoing);
                    let _ = respond_to.send(result);
                }

                DbCommand::Query {
                    uid,
                    sql,
                    params,
                    respond_to,
                } => {
                    let result = self.handle_query(&uid, &sql, &params);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetMessageById {
                    uid,
                    id,
                    respond_to,
                } => {
                    let result = self.handle_get_message_by_id(&uid, id);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetMessageChannelId {
                    uid,
                    id,
                    respond_to,
                } => {
                    let result = self.handle_get_message_channel_id(&uid, id);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetMessagesBefore {
                    uid,
                    channel_id,
                    before_id,
                    limit,
                    respond_to,
                } => {
                    let result =
                        self.handle_get_messages_before(&uid, channel_id, before_id, limit);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetEarliestId {
                    uid,
                    channel_id,
                    respond_to,
                } => {
                    let result = self.handle_get_earliest_id(&uid, channel_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetMessagesAfter {
                    uid,
                    channel_id,
                    after_id,
                    limit,
                    respond_to,
                } => {
                    let result = self.handle_get_messages_after(&uid, channel_id, after_id, limit);
                    let _ = respond_to.send(result);
                }

                DbCommand::SendMessage {
                    uid,
                    channel_id,
                    channel_type,
                    from_uid,
                    content,
                    message_type,
                    respond_to,
                } => {
                    let result = self.handle_send_message(
                        &uid,
                        channel_id,
                        channel_type,
                        from_uid,
                        &content,
                        message_type,
                    );
                    let _ = respond_to.send(result);
                }

                DbCommand::RevokeMessage {
                    uid,
                    id,
                    revoker_id,
                    respond_to,
                } => {
                    let result = self.handle_revoke_message(&uid, id, revoker_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::EditMessage {
                    uid,
                    id,
                    new_content,
                    respond_to,
                } => {
                    let result = self.handle_edit_message(&uid, id, &new_content);
                    let _ = respond_to.send(result);
                }

                DbCommand::DeleteMessage {
                    uid,
                    id,
                    respond_to,
                } => {
                    let result = self.handle_delete_message(&uid, id);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateMessageContent {
                    uid,
                    id,
                    new_content,
                    respond_to,
                } => {
                    let result = self.handle_update_message_content(&uid, id, &new_content);
                    let _ = respond_to.send(result);
                }

                DbCommand::AddMessageReaction {
                    uid,
                    id,
                    user_id,
                    reaction,
                    respond_to,
                } => {
                    let result = self.handle_add_message_reaction(&uid, id, user_id, &reaction);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveChannel {
                    uid,
                    channel,
                    respond_to,
                } => {
                    let result = self.handle_save_channel(&uid, &channel);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetChannels { uid, respond_to } => {
                    let result = self.handle_get_channels(&uid);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetChannelByChannel {
                    uid,
                    channel_id,
                    channel_type,
                    respond_to,
                } => {
                    let result = self.handle_get_channel_by_channel(&uid, channel_id, channel_type);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetDirectChannelById {
                    uid,
                    channel_id,
                    respond_to,
                } => {
                    let result = self.handle_get_direct_channel_by_id(&uid, channel_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateChannelPts {
                    uid,
                    channel_id,
                    channel_type,
                    new_pts,
                    respond_to,
                } => {
                    let result =
                        self.handle_update_channel_pts(&uid, channel_id, channel_type, new_pts);
                    let _ = respond_to.send(result);
                }

                DbCommand::FindChannelIdByUser {
                    uid,
                    target_user_id,
                    respond_to,
                } => {
                    let result = self.handle_find_channel_id_by_user(&uid, target_user_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateChannelSave {
                    uid,
                    channel_id,
                    channel_type,
                    save,
                    respond_to,
                } => {
                    let result =
                        self.handle_update_channel_save(&uid, channel_id, channel_type, save);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateChannelMute {
                    uid,
                    channel_id,
                    channel_type,
                    mute,
                    respond_to,
                } => {
                    let result =
                        self.handle_update_channel_mute(&uid, channel_id, channel_type, mute);
                    let _ = respond_to.send(result);
                }

                DbCommand::UpdateChannelExtra {
                    uid,
                    channel_id,
                    channel_type,
                    extra,
                    respond_to,
                } => {
                    let result =
                        self.handle_update_channel_extra(&uid, channel_id, channel_type, &extra);
                    let _ = respond_to.send(result);
                }

                // ========== User / Group / GroupMember 命令处理 ==========
                DbCommand::SaveUser {
                    uid,
                    user,
                    respond_to,
                } => {
                    let result = self.handle_save_user(&uid, &user);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveUsers {
                    uid,
                    users,
                    respond_to,
                } => {
                    let result = self.handle_save_users(&uid, &users);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetUser {
                    uid,
                    user_id,
                    respond_to,
                } => {
                    let result = self.handle_get_user(&uid, user_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetUsersByIds {
                    uid,
                    ids,
                    respond_to,
                } => {
                    let result = self.handle_get_users_by_ids(&uid, &ids);
                    let _ = respond_to.send(result);
                }

                // ========== 好友管理命令处理 ==========
                DbCommand::SaveFriend {
                    uid,
                    friend,
                    respond_to,
                } => {
                    let result = self.handle_save_friend(&uid, &friend);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveFriends {
                    uid,
                    friends,
                    respond_to,
                } => {
                    let result = self.handle_save_friends(&uid, &friends);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetFriends {
                    uid,
                    limit,
                    offset,
                    respond_to,
                } => {
                    let result = self.handle_get_friends(&uid, limit, offset);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetFriendsCount { uid, respond_to } => {
                    let result = self.handle_get_friends_count(&uid);
                    let _ = respond_to.send(result);
                }

                DbCommand::DeleteFriend {
                    uid,
                    user_id,
                    respond_to,
                } => {
                    let result = self.handle_delete_friend(&uid, user_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveChannelMember {
                    uid,
                    member,
                    respond_to,
                } => {
                    let result = self.handle_save_channel_member(&uid, &member);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveChannelMembers {
                    uid,
                    members,
                    respond_to,
                } => {
                    let result = self.handle_save_channel_members(&uid, &members);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetGroupMembers {
                    uid,
                    group_id,
                    limit,
                    offset,
                    respond_to,
                } => {
                    let result = self.handle_get_group_members(&uid, group_id, limit, offset);
                    let _ = respond_to.send(result);
                }

                DbCommand::DeleteChannelMember {
                    uid,
                    channel_id,
                    channel_type,
                    member_uid,
                    respond_to,
                } => {
                    let result = self.handle_delete_channel_member(
                        &uid,
                        channel_id,
                        channel_type,
                        member_uid,
                    );
                    let _ = respond_to.send(result);
                }

                DbCommand::GetGroups {
                    uid,
                    limit,
                    offset,
                    respond_to,
                } => {
                    let result = self.handle_get_groups(&uid, limit, offset);
                    let _ = respond_to.send(result);
                }

                DbCommand::GetGroup {
                    uid,
                    group_id,
                    respond_to,
                } => {
                    let result = self.handle_get_group(&uid, group_id);
                    let _ = respond_to.send(result);
                }

                DbCommand::SaveGroups {
                    uid,
                    groups,
                    respond_to,
                } => {
                    let result = self.handle_save_groups(&uid, &groups);
                    let _ = respond_to.send(result);
                }

                DbCommand::CloseUser { uid, respond_to } => {
                    let result = self.handle_close_user(&uid);
                    let _ = respond_to.send(result);
                }
            }
        }

        info!("✅ [Thread {:?}] DbActor 已停止", self.thread_id);
    }

    /// 处理：初始化用户数据库
    fn handle_init_user(&mut self, uid: &str, db_path: &PathBuf) -> Result<()> {
        info!(
            "📥 [DbActor Thread {:?}] 接收命令: InitUser(uid={}, path={})",
            self.thread_id,
            uid,
            db_path.display()
        );

        // 检查是否已经初始化
        if self.connections.contains_key(uid) {
            info!(
                "⚠️  [DbActor Thread {:?}] 用户已初始化，跳过: uid={}",
                self.thread_id, uid
            );
            return Ok(());
        }

        info!(
            "🔨 [DbActor Thread {:?}] 开始打开数据库: uid={}",
            self.thread_id, uid
        );

        // 打开数据库
        let mut conn = Connection::open(db_path).map_err(|e| {
            error!(
                "❌ [DbActor Thread {:?}] 打开数据库失败: uid={}, error={}",
                self.thread_id, uid, e
            );
            PrivchatSDKError::Database(format!("打开数据库失败: {}", e))
        })?;

        info!(
            "🔐 [DbActor Thread {:?}] 设置数据库加密: uid={}",
            self.thread_id, uid
        );

        // 🔐 设置 SQLCipher 加密密钥
        let encryption_key = Self::derive_encryption_key(uid);
        conn.pragma_update(None, "key", &encryption_key)
            .map_err(|e| {
                error!(
                    "❌ [DbActor Thread {:?}] 设置加密密钥失败: uid={}, error={}",
                    self.thread_id, uid, e
                );
                PrivchatSDKError::Database(format!("设置加密密钥失败: {}", e))
            })?;

        // 统一初始化：pragmas + migrations + 版本校验（内置路径走 init_db；外部 assets 路径单独 pragmas + migrate_database）
        if self.assets_path.is_none() {
            info!("📦 [DbActor Thread {:?}] 使用内置 init_db（pragmas + refinery + 版本校验）: uid={}", self.thread_id, uid);
            crate::storage::migrate::init_db(&mut conn)?;
        } else {
            info!(
                "🔧 [DbActor Thread {:?}] 设置 PRAGMA（外部 assets 路径）: uid={}",
                self.thread_id, uid
            );
            crate::storage::migrate::enable_pragmas(&conn)?;
            info!(
                "🔄 [DbActor Thread {:?}] 执行外部 assets 迁移: uid={}",
                self.thread_id, uid
            );
            self.migrate_database(&mut conn, uid)?;
        }

        info!(
            "✅ [DbActor Thread {:?}] 数据库迁移完成: uid={}",
            self.thread_id, uid
        );

        // 验证关键表是否存在（检查 message, channel, channel_member 三个核心表）
        let table_count: i32 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('message', 'channel', 'channel_member')",
            [],
            |row| row.get(0)
        ).map_err(|e| {
            error!("❌ [DbActor Thread {:?}] 验证表结构失败: uid={}, error={}", self.thread_id, uid, e);
            PrivchatSDKError::Database(format!("验证表结构失败: {}", e))
        })?;

        if table_count < 3 {
            error!(
                "❌ [DbActor Thread {:?}] 关键表缺失: uid={}, found={}/3",
                self.thread_id, uid, table_count
            );
            return Err(PrivchatSDKError::Database(
                "数据库初始化不完整：关键表缺失".to_string(),
            ));
        }

        info!(
            "✅ [DbActor Thread {:?}] Schema验证通过: uid={}, 核心表数量={}/3",
            self.thread_id, uid, table_count
        );

        info!(
            "💾 [DbActor Thread {:?}] 保存连接到连接池: uid={} (保存前连接数={})",
            self.thread_id,
            uid,
            self.connections.len()
        );

        // 保存连接
        self.connections.insert(uid.to_string(), conn);

        info!(
            "✅ [DbActor Thread {:?}] 用户数据库初始化完成: uid={} (保存后连接数={}, keys={:?})",
            self.thread_id,
            uid,
            self.connections.len(),
            self.connections.keys().collect::<Vec<_>>()
        );

        Ok(())
    }

    /// 处理：执行 SQL（通用）
    fn handle_execute(
        &mut self,
        uid: &str,
        sql: &str,
        params: &[rusqlite::types::Value],
    ) -> Result<usize> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        // 使用 params_from_iter 来转换参数
        let affected = conn
            .execute(sql, rusqlite::params_from_iter(params.iter()))
            .map_err(|e| PrivchatSDKError::Database(format!("执行SQL失败: {}", e)))?;

        Ok(affected)
    }

    /// 处理：更新消息状态（按 message.id）
    fn handle_update_message_status(&mut self, uid: &str, id: i64, status: i32) -> Result<()> {
        info!(
            "📥 [DbActor Thread {:?}] 接收命令: UpdateMessageStatus(uid={}, id={}, status={})",
            self.thread_id, uid, id, status
        );

        // 调试：列出所有连接
        let available_uids: Vec<&String> = self.connections.keys().collect();
        info!(
            "🗂️  [DbActor Thread {:?}] 当前连接池状态: {} 个连接, uids={:?}",
            self.thread_id,
            self.connections.len(),
            available_uids
        );

        if !self.connections.contains_key(uid) {
            error!(
                "❌ [DbActor Thread {:?}] 用户数据库不存在! requested_uid={}, available_uids={:?}",
                self.thread_id, uid, available_uids
            );
            return Err(PrivchatSDKError::Database(format!(
                "用户数据库不存在: requested={}, available={:?}",
                uid, available_uids
            )));
        }

        info!(
            "✓ [DbActor Thread {:?}] 找到数据库连接: uid={}",
            self.thread_id, uid
        );

        let conn = self.connections.get(uid).unwrap();

        // 🔍 调试：验证 message 表是否存在
        match conn.query_row::<i32, _, _>(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='message'",
            [],
            |row| row.get(0),
        ) {
            Ok(count) => {
                if count == 0 {
                    error!(
                        "❌ [DbActor Thread {:?}] message 表不存在！uid={}",
                        self.thread_id, uid
                    );
                    // 列出所有表
                    if let Ok(mut stmt) =
                        conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")
                    {
                        if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                            let tables: Vec<String> = rows.filter_map(|r| r.ok()).collect();
                            error!(
                                "📋 [DbActor Thread {:?}] 现有表列表: {:?}",
                                self.thread_id, tables
                            );
                        }
                    }
                    return Err(PrivchatSDKError::Database(format!(
                        "message 表不存在，数据库可能未正确初始化"
                    )));
                } else {
                    info!(
                        "✓ [DbActor Thread {:?}] message 表存在: uid={}",
                        self.thread_id, uid
                    );
                }
            }
            Err(e) => {
                error!(
                    "❌ [DbActor Thread {:?}] 查询表失败: uid={}, error={}",
                    self.thread_id, uid, e
                );
            }
        }

        let affected = conn
            .execute(
                "UPDATE message SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status, chrono::Utc::now().timestamp_millis(), id],
            )
            .map_err(|e| {
                error!(
                    "❌ [DbActor Thread {:?}] SQL 执行失败: uid={}, error={}",
                    self.thread_id, uid, e
                );
                PrivchatSDKError::Database(format!("更新消息状态失败: {}", e))
            })?;

        info!(
            "✅ [DbActor Thread {:?}] 更新成功: uid={}, affected={} rows",
            self.thread_id, uid, affected
        );

        Ok(())
    }

    /// 处理：更新消息的服务端 ID（按 message.id，仅协议层写入）
    fn handle_update_message_server_id(
        &mut self,
        uid: &str,
        id: i64,
        server_message_id: u64,
    ) -> Result<()> {
        info!(
            "🔍 [DbActor] 更新 message_id: uid={}, id={}, server_message_id={}",
            uid, id, server_message_id
        );

        let conn = self.connections.get(uid).ok_or_else(|| {
            error!("❌ [DbActor] 用户数据库不存在: uid={}", uid);
            PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid))
        })?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.update_server_message_id(id, server_message_id)?;
        Ok(())
    }

    /// 处理：保存接收的消息
    /// 若 message_id 非空且 (channel_id, message_id) 已存在则视为同一条消息，跳过插入并返回已有行 id
    fn handle_save_received_message(
        &mut self,
        uid: &str,
        message: &Message,
        is_outgoing: bool,
    ) -> Result<i64> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let row_id = if let Some(msg_id) = message.server_message_id {
            let existing_id: Option<i64> = match conn.query_row(
                r#"
                SELECT id FROM message
                WHERE channel_id = ?1 AND message_id = ?2 AND is_deleted = 0
                "#,
                rusqlite::params![message.channel_id as i64, msg_id as i64],
                |row| row.get(0),
            ) {
                Ok(id) => Some(id),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(PrivchatSDKError::Database(format!("查重失败: {}", e))),
            };
            if let Some(id) = existing_id {
                debug!(
                    "[Rust SDK] 💾 跳过重复(已存在): channel_id={}, message_id={}, 已有 row_id={}",
                    message.channel_id, msg_id, id
                );
                id
            } else {
                use crate::storage::dao::MessageDao;
                let message_dao = MessageDao::new(conn);
                let id = message_dao.insert(message)?;
                debug!(
                    "[Rust SDK] 💾 写入本地 DB: channel_id={}, message_id={:?}, row_id={}",
                    message.channel_id, message.server_message_id, id
                );
                id
            }
        } else {
            use crate::storage::dao::MessageDao;
            let message_dao = MessageDao::new(conn);
            message_dao.insert(message)?
        };

        // 无论新消息还是重复消息，都确保会话存在，否则 get_channels() 为空、会话列表不显示
        let channel_dao = dao::ChannelDao::new(conn);
        let is_direct = message.channel_type == 0 || message.channel_type == 1;
        if is_direct {
            // 私聊：同一 channel_id 只保留一条记录（type 0 或 1 视为同一私聊），避免回消息时再插一条导致列表两条
            if let Some(mut existing) = channel_dao.get_direct_channel_by_id(message.channel_id)? {
                existing.last_msg_timestamp = message.timestamp;
                existing.last_msg_content = message.content.clone();
                // 只有收到的消息才增加未读数，自己发送的不增加
                if !is_outgoing {
                    existing.unread_count = existing.unread_count.saturating_add(1);
                }
                existing.last_msg_pts = message.pts;
                info!(
                    "[DB] 更新私聊会话: channel_id={}, last_msg_content='{}', timestamp={:?}, is_outgoing={}",
                    message.channel_id, existing.last_msg_content, existing.last_msg_timestamp, is_outgoing
                );
                if let Err(e) = channel_dao.upsert(&existing) {
                    warn!(
                        "接收消息后更新私聊会话失败: channel_id={}, error={:?}",
                        message.channel_id, e
                    );
                } else {
                    info!(
                        "[DB] 私聊会话已更新: channel_id={}, last_msg_content='{}'",
                        message.channel_id, existing.last_msg_content
                    );
                }
            } else {
                let now = chrono::Utc::now().timestamp_millis();
                let new_channel = entities::Channel {
                    id: None,
                    channel_id: message.channel_id,
                    channel_type: 1, // 私聊统一用 1，避免出现 (id,0) 与 (id,1) 两条
                    last_local_message_id: 0,
                    last_msg_timestamp: message.timestamp,
                    last_msg_content: message.content.clone(),
                    unread_count: if is_outgoing { 0 } else { 1 },
                    last_msg_pts: message.pts,
                    show_nick: 0,
                    username: String::new(),
                    channel_name: String::new(),
                    channel_remark: String::new(),
                    top: 0,
                    mute: 0,
                    save: 0,
                    forbidden: 0,
                    follow: 0,
                    is_deleted: 0,
                    receipt: 0,
                    status: 1,
                    invite: 0,
                    robot: 0,
                    version: 1,
                    online: 0,
                    last_offline: 0,
                    avatar: String::new(),
                    category: String::new(),
                    extra: String::new(),
                    created_at: now,
                    updated_at: now,
                    avatar_cache_key: String::new(),
                    remote_extra: None,
                    flame: 0,
                    flame_second: 0,
                    device_flag: 0,
                    parent_channel_id: 0,
                    parent_channel_type: 0,
                };
                if let Err(e) = channel_dao.upsert(&new_channel) {
                    warn!(
                        "保存消息后创建私聊会话失败: channel_id={}, error={:?}",
                        message.channel_id, e
                    );
                } else {
                    info!(
                        "保存消息后已创建私聊会话: channel_id={}",
                        message.channel_id
                    );
                }
            }
        } else {
            // 非私聊（群聊等）
            if let Some(mut existing) =
                channel_dao.get_by_channel(message.channel_id, message.channel_type)?
            {
                existing.last_msg_timestamp = message.timestamp;
                existing.last_msg_content = message.content.clone();
                // 只有收到的消息才增加未读数，自己发送的不增加
                if !is_outgoing {
                    existing.unread_count = existing.unread_count.saturating_add(1);
                }
                existing.last_msg_pts = message.pts;
                if let Err(e) = channel_dao.upsert(&existing) {
                    warn!(
                        "接收消息后更新群聊会话失败: channel_id={}, error={:?}",
                        message.channel_id, e
                    );
                } else {
                    debug!(
                        "接收消息后已更新群聊会话: channel_id={}",
                        message.channel_id
                    );
                }
            } else {
                let now = chrono::Utc::now().timestamp_millis();
                let new_channel = entities::Channel {
                    id: None,
                    channel_id: message.channel_id,
                    channel_type: message.channel_type,
                    last_local_message_id: 0,
                    last_msg_timestamp: message.timestamp,
                    last_msg_content: message.content.clone(),
                    unread_count: if is_outgoing { 0 } else { 1 },
                    last_msg_pts: message.pts,
                    show_nick: 0,
                    username: String::new(),
                    channel_name: String::new(),
                    channel_remark: String::new(),
                    top: 0,
                    mute: 0,
                    save: 0,
                    forbidden: 0,
                    follow: 0,
                    is_deleted: 0,
                    receipt: 0,
                    status: 1,
                    invite: 0,
                    robot: 0,
                    version: 1,
                    online: 0,
                    last_offline: 0,
                    avatar: String::new(),
                    category: String::new(),
                    extra: String::new(),
                    created_at: now,
                    updated_at: now,
                    avatar_cache_key: String::new(),
                    remote_extra: None,
                    flame: 0,
                    flame_second: 0,
                    device_flag: 0,
                    parent_channel_id: 0,
                    parent_channel_type: 0,
                };
                if let Err(e) = channel_dao.upsert(&new_channel) {
                    warn!(
                        "保存消息后创建会话失败: channel_id={}, error={:?}",
                        message.channel_id, e
                    );
                } else {
                    info!(
                        "保存消息后已创建/确保会话: channel_id={}, channel_type={}",
                        message.channel_id, message.channel_type
                    );
                }
            }
        }

        Ok(row_id)
    }

    /// 处理：查询
    fn handle_query(
        &mut self,
        uid: &str,
        sql: &str,
        params: &[rusqlite::types::Value],
    ) -> Result<Vec<serde_json::Value>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备查询失败: {}", e)))?;

        let column_count = stmt.column_count();
        // 先获取所有列名
        let column_names: Vec<String> = (0..column_count)
            .map(|i| stmt.column_name(i).unwrap_or("unknown").to_string())
            .collect();

        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                let mut map = serde_json::Map::new();
                for i in 0..column_count {
                    let value: rusqlite::types::Value = row.get(i)?;
                    map.insert(column_names[i].clone(), rusqlite_value_to_json(value));
                }
                Ok(serde_json::Value::Object(map))
            })
            .map_err(|e| PrivchatSDKError::Database(format!("查询失败: {}", e)))?;

        let mut results = Vec::new();
        for row in rows {
            results
                .push(row.map_err(|e| PrivchatSDKError::Database(format!("读取行失败: {}", e)))?);
        }

        Ok(results)
    }

    /// 处理：根据 message.id 获取消息
    fn handle_get_message_by_id(&mut self, uid: &str, id: i64) -> Result<Option<Message>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.get_by_id(id)
    }

    fn handle_get_message_channel_id(&mut self, uid: &str, id: i64) -> Result<Option<u64>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let result: Option<i64> = match conn.query_row(
            "SELECT channel_id FROM message WHERE id = ?1",
            [id],
            |row| row.get(0),
        ) {
            Ok(val) => Some(val),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };

        Ok(result.map(|cid| cid as u64))
    }

    /// 处理：获取指定 message.id 之前的消息（游标为客户端自增 id）
    fn handle_get_messages_before(
        &mut self,
        uid: &str,
        channel_id: u64,
        before_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        // 当 before_id 为 u64::MAX 时表示“取最新一页”
        let before_i64: i64 = if before_id == u64::MAX {
            i64::MAX
        } else {
            before_id as i64
        };

        let sql = r#"
            SELECT * FROM message
            WHERE channel_id = ?1
              AND is_deleted = 0
              AND id < ?2
            ORDER BY id DESC
            LIMIT ?3
        "#;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备查询失败: {}", e)))?;

        let messages = stmt
            .query_map(
                rusqlite::params![channel_id as i64, before_i64, limit],
                |row| {
                    Ok(Message {
                        id: row.get("id").ok(),
                        server_message_id: row.get("message_id").ok(),
                        pts: row.get("pts")?,
                        channel_id: row.get("channel_id")?,
                        channel_type: row.get("channel_type")?,
                        timestamp: row.get("timestamp").ok(),
                        from_uid: row.get("from_uid")?,
                        message_type: row.get("type")?,
                        content: row.get("content")?,
                        status: row.get("status")?,
                        voice_status: row.get("voice_status")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        searchable_word: row.get("searchable_word")?,
                        local_message_id: row.get("local_message_id")?,
                        is_deleted: row.get("is_deleted")?,
                        setting: row.get("setting")?,
                        order_seq: row.get("order_seq")?,
                        extra: row.get("extra")?,
                        flame: row.get("flame")?,
                        flame_second: row.get("flame_second")?,
                        viewed: row.get("viewed")?,
                        viewed_at: row.get("viewed_at")?,
                        topic_id: row.get("topic_id")?,
                        expire_time: row.get("expire_time").ok(),
                        expire_timestamp: row.get("expire_timestamp").ok(),
                        revoked: row.get("revoked")?,
                        revoked_at: row.get("revoked_at")?,
                        revoked_by: row.get("revoked_by").ok(),
                    })
                },
            )
            .map_err(|e| PrivchatSDKError::Database(format!("查询消息失败: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| PrivchatSDKError::Database(format!("解析消息失败: {}", e)))?;

        debug!(
            "[Rust SDK] 📖 从本地 DB 读取: channel_id={}, before_id={}, limit={}, 返回 {} 条",
            channel_id,
            before_id,
            limit,
            messages.len()
        );
        Ok(messages)
    }

    /// 处理：获取频道当前最小的 message.id（用于「加载更早」分页游标）
    fn handle_get_earliest_id(&mut self, uid: &str, channel_id: u64) -> Result<Option<u64>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let sql = r#"
            SELECT MIN(id) FROM message
            WHERE channel_id = ?1 AND is_deleted = 0
        "#;

        let min_id: Option<i64> = conn
            .query_row(sql, rusqlite::params![channel_id as i64], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .map_err(|e| PrivchatSDKError::Database(format!("查询最早 id 失败: {}", e)))?;

        Ok(min_id.map(|id| id as u64))
    }

    /// 处理：获取指定 message.id 之后的消息（向前分页，加载更新的消息）
    fn handle_get_messages_after(
        &mut self,
        uid: &str,
        channel_id: u64,
        after_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let sql = r#"
            SELECT * FROM message
            WHERE channel_id = ?1
              AND is_deleted = 0
              AND id > ?2
            ORDER BY id ASC
            LIMIT ?3
        "#;

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备查询失败: {}", e)))?;

        let messages = stmt
            .query_map(
                rusqlite::params![channel_id as i64, after_id as i64, limit],
                |row| {
                    Ok(Message {
                        id: row.get("id").ok(),
                        server_message_id: row.get("message_id").ok(),
                        pts: row.get("pts")?,
                        channel_id: row.get("channel_id")?,
                        channel_type: row.get("channel_type")?,
                        timestamp: row.get("timestamp").ok(),
                        from_uid: row.get("from_uid")?,
                        message_type: row.get("type")?,
                        content: row.get("content")?,
                        status: row.get("status")?,
                        voice_status: row.get("voice_status")?,
                        created_at: row.get("created_at")?,
                        updated_at: row.get("updated_at")?,
                        searchable_word: row.get("searchable_word")?,
                        local_message_id: row.get("local_message_id")?,
                        is_deleted: row.get("is_deleted")?,
                        setting: row.get("setting")?,
                        order_seq: row.get("order_seq")?,
                        extra: row.get("extra")?,
                        flame: row.get("flame")?,
                        flame_second: row.get("flame_second")?,
                        viewed: row.get("viewed")?,
                        viewed_at: row.get("viewed_at")?,
                        topic_id: row.get("topic_id")?,
                        expire_time: row.get("expire_time").ok(),
                        expire_timestamp: row.get("expire_timestamp").ok(),
                        revoked: row.get("revoked")?,
                        revoked_at: row.get("revoked_at")?,
                        revoked_by: row.get("revoked_by").ok(),
                    })
                },
            )
            .map_err(|e| PrivchatSDKError::Database(format!("查询消息失败: {}", e)))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| PrivchatSDKError::Database(format!("解析消息失败: {}", e)))?;

        Ok(messages)
    }

    /// 处理：发送消息（插入后返回 message.id，协议字段写 0）
    fn handle_send_message(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: &str,
        message_type: i32,
    ) -> Result<i64> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let now = chrono::Utc::now().timestamp_millis();
        let message = Message {
            id: None,
            server_message_id: None, // 协议字段，无值时写 0（存库用 0 表示未收到服务端 id）
            pts: now,
            channel_id,
            channel_type,
            timestamp: Some(now),
            from_uid,
            message_type,
            content: content.to_string(),
            status: 0,
            voice_status: 0,
            created_at: now,
            updated_at: now,
            searchable_word: content.to_string(),
            local_message_id: 0, // 协议字段，所有操作只用 message.id
            is_deleted: 0,
            setting: 0,
            order_seq: now,
            extra: "{}".to_string(),
            flame: 0,
            flame_second: 0,
            viewed: 0,
            viewed_at: 0,
            topic_id: "".to_string(),
            expire_time: None,
            expire_timestamp: None,
            revoked: 0,
            revoked_at: 0,
            revoked_by: None,
        };

        let message_dao = dao::MessageDao::new(conn);
        let row_id = message_dao.insert(&message)?;
        self.update_channel_after_message(conn, &message)?;
        Ok(row_id)
    }

    /// 更新会话（在消息发送后）
    fn update_channel_after_message(&self, _conn: &Connection, _message: &Message) -> Result<()> {
        // TODO: 实现会话更新逻辑
        // 暂时跳过，避免复杂的 DAO 调用
        Ok(())
    }

    /// 处理：撤回消息（按 message.id）
    fn handle_revoke_message(&mut self, uid: &str, id: i64, revoker_id: u64) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.revoke(id, revoker_id)?;
        Ok(())
    }

    /// 处理：编辑消息（按 message.id）
    fn handle_edit_message(&mut self, uid: &str, id: i64, new_content: &str) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.edit(id, new_content)?;
        Ok(())
    }

    /// 处理：删除消息（按 message.id）
    fn handle_delete_message(&mut self, uid: &str, id: i64) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.soft_delete(id)?;
        Ok(())
    }

    /// 处理：更新消息内容（按 message.id）
    fn handle_update_message_content(
        &mut self,
        uid: &str,
        id: i64,
        new_content: &str,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let message_dao = dao::MessageDao::new(conn);
        message_dao.edit(id, new_content)?;
        Ok(())
    }

    /// 处理：添加消息反应（按 message.id）
    fn handle_add_message_reaction(
        &mut self,
        uid: &str,
        id: i64,
        user_id: u64,
        reaction: &str,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        conn.execute(
            "INSERT OR REPLACE INTO message_reaction (channel_id, channel_type, uid, name, emoji, message_id, created_at) VALUES (0, 0, ?1, '', ?2, ?3, ?4)",
            rusqlite::params![
                user_id as i64,
                reaction,
                id,
                chrono::Utc::now().timestamp_millis()
            ],
        ).map_err(|e| PrivchatSDKError::Database(format!("添加消息反应失败: {}", e)))?;
        Ok(())
    }

    /// 处理：保存 Channel
    fn handle_save_channel(&mut self, uid: &str, channel: &entities::Channel) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.upsert(channel)?;

        Ok(())
    }

    /// 处理：获取会话列表
    fn handle_get_channels(&mut self, uid: &str) -> Result<Vec<entities::Channel>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        info!("[DB] handle_get_channels for uid={}", uid);
        let sql = "SELECT * FROM channel WHERE is_deleted = 0 ORDER BY last_msg_timestamp DESC";
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| PrivchatSDKError::Database(format!("准备查询失败: {}", e)))?;

        let rows = stmt
            .query_map([], |row| {
                let channel_id: i64 = row.get("channel_id")?;
                Ok(entities::Channel {
                    id: Some(channel_id),
                    channel_id: channel_id as u64,
                    channel_type: row.get("channel_type")?,
                    // 会话列表相关字段
                    last_local_message_id: row.get("last_local_message_id").unwrap_or(0),
                    last_msg_timestamp: row.get("last_msg_timestamp").ok(),
                    last_msg_content: row.get("last_msg_content").unwrap_or_default(),
                    unread_count: row.get("unread_count").unwrap_or(0),
                    last_msg_pts: row.get("last_msg_pts").unwrap_or(0),
                    // 频道信息字段（使用默认值）
                    show_nick: row.get("show_nick").unwrap_or(0),
                    username: row.get("username").unwrap_or_default(),
                    channel_name: row.get("channel_name").unwrap_or_default(),
                    channel_remark: row.get("channel_remark").unwrap_or_default(),
                    top: row.get("top").unwrap_or(0),
                    mute: row.get("mute").unwrap_or(0),
                    save: row.get("save").unwrap_or(0),
                    forbidden: row.get("forbidden").unwrap_or(0),
                    follow: row.get("follow").unwrap_or(0),
                    is_deleted: row.get("is_deleted")?,
                    receipt: row.get("receipt").unwrap_or(0),
                    status: row.get("status").unwrap_or(1),
                    invite: row.get("invite").unwrap_or(0),
                    robot: row.get("robot").unwrap_or(0),
                    version: row.get("version")?,
                    online: row.get("online").unwrap_or(0),
                    last_offline: row.get("last_offline").unwrap_or(0),
                    avatar: row.get("avatar").unwrap_or_default(),
                    category: row.get("category").unwrap_or_default(),
                    extra: row.get("extra").unwrap_or_default(),
                    created_at: row.get("created_at").unwrap_or(0),
                    updated_at: row.get("updated_at").unwrap_or(0),
                    avatar_cache_key: row.get("avatar_cache_key").unwrap_or_default(),
                    remote_extra: row.get("remote_extra").ok(),
                    flame: row.get("flame").unwrap_or(0),
                    flame_second: row.get("flame_second").unwrap_or(0),
                    device_flag: row.get("device_flag").unwrap_or(0),
                    parent_channel_id: row.get("parent_channel_id").unwrap_or(0),
                    parent_channel_type: row.get("parent_channel_type").unwrap_or(0),
                })
            })
            .map_err(|e| PrivchatSDKError::Database(format!("查询失败: {}", e)))?;

        let mut raw: Vec<entities::Channel> = Vec::new();
        for row in rows {
            let ch = row.map_err(|e| PrivchatSDKError::Database(format!("解析行失败: {}", e)))?;
            info!(
                "[DB] 读取 channel: id={}, last_msg_content='{}', timestamp={:?}",
                ch.channel_id, ch.last_msg_content, ch.last_msg_timestamp
            );
            raw.push(ch);
        }

        // 私聊（channel_type 0 与 1）按 channel_id 去重，只保留一条，避免同一会话出现两条
        let mut direct_by_id: std::collections::HashMap<u64, entities::Channel> =
            std::collections::HashMap::new();
        let mut others: Vec<entities::Channel> = Vec::new();
        for ch in raw {
            if ch.channel_type == 0 || ch.channel_type == 1 {
                if let Some(existing) = direct_by_id.get_mut(&ch.channel_id) {
                    // 合并：取较新的时间，未读累加，统一为 channel_type=1
                    let ts_new = ch.last_msg_timestamp.unwrap_or(0) as i64;
                    let ts_old = existing.last_msg_timestamp.unwrap_or(0) as i64;
                    if ts_new >= ts_old {
                        existing.last_msg_timestamp = ch.last_msg_timestamp;
                        existing.last_msg_pts = ch.last_msg_pts;
                    }
                    existing.unread_count = existing.unread_count.saturating_add(ch.unread_count);
                    if !ch.channel_name.is_empty() {
                        existing.channel_name = ch.channel_name.clone();
                    }
                    if !ch.avatar.is_empty() {
                        existing.avatar = ch.avatar.clone();
                    }
                    existing.channel_type = 1;
                } else {
                    let mut c = ch;
                    c.channel_type = 1;
                    direct_by_id.insert(c.channel_id, c);
                }
            } else {
                others.push(ch);
            }
        }
        let mut channels: Vec<entities::Channel> = direct_by_id.into_values().collect();
        channels.sort_by(|a, b| {
            let ta = a.last_msg_timestamp.unwrap_or(0) as i64;
            let tb = b.last_msg_timestamp.unwrap_or(0) as i64;
            tb.cmp(&ta)
        });
        channels.extend(others);
        channels.sort_by(|a, b| {
            let ta = a.last_msg_timestamp.unwrap_or(0) as i64;
            let tb = b.last_msg_timestamp.unwrap_or(0) as i64;
            tb.cmp(&ta)
        });

        Ok(channels)
    }

    /// 处理：根据频道获取会话
    fn handle_get_channel_by_channel(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<Option<entities::Channel>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.get_by_channel(channel_id, channel_type as i32)
    }

    /// 处理：按 channel_id 查询私聊会话（type 0 或 1 均视为同一私聊）
    fn handle_get_direct_channel_by_id(
        &mut self,
        uid: &str,
        channel_id: u64,
    ) -> Result<Option<entities::Channel>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.get_direct_channel_by_id(channel_id)
    }

    /// 处理：更新会话的 pts
    fn handle_update_channel_pts(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: u8,
        new_pts: u64,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.update_pts(channel_id, channel_type as i32, new_pts)?;

        Ok(())
    }

    /// 处理：根据用户查找 channel_id
    fn handle_find_channel_id_by_user(
        &mut self,
        uid: &str,
        target_user_id: u64,
    ) -> Result<Option<u64>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);

        if let Some(channel) = channel_dao.find_by_username(&target_user_id.to_string())? {
            Ok(Some(channel.channel_id))
        } else {
            Ok(None)
        }
    }

    /// 处理：更新频道的 save 字段（收藏状态）
    fn handle_update_channel_save(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        save: i32,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.update_save(channel_id, channel_type, save)?;
        Ok(())
    }

    /// 处理：更新频道的 mute 字段（通知模式）
    fn handle_update_channel_mute(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        mute: i32,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.update_mute(channel_id, channel_type, mute)?;
        Ok(())
    }

    /// 处理：更新会话的 extra 字段
    fn handle_update_channel_extra(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        extra: &str,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let channel_dao = dao::ChannelDao::new(conn);
        channel_dao.update_extra(channel_id, channel_type, extra)?;
        Ok(())
    }

    // ========== 好友管理处理方法（Local-first）==========

    /// 处理：保存单个好友（Entity Model V1：仅关系）
    fn handle_save_friend(&mut self, uid: &str, friend: &entities::Friend) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let fd = dao::FriendDao::new(conn);
        fd.upsert(friend)
            .map_err(|e| PrivchatSDKError::Database(format!("保存好友失败: {}", e)))?;
        debug!(
            "✅ [DbActor] 保存好友成功: uid={}, user_id={}",
            uid, friend.user_id
        );
        Ok(())
    }

    /// 处理：批量保存好友（使用事务）
    fn handle_save_friends(&mut self, uid: &str, friends: &[entities::Friend]) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let fd = dao::FriendDao::new(conn);
        fd.upsert_many(friends)
            .map_err(|e| PrivchatSDKError::Database(format!("批量保存好友失败: {}", e)))?;
        info!(
            "✅ [DbActor] 批量保存好友成功: uid={}, count={}",
            uid,
            friends.len()
        );
        Ok(())
    }

    /// 处理：获取好友列表（分页）
    fn handle_get_friends(
        &self,
        uid: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<entities::Friend>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let fd = dao::FriendDao::new(conn);
        let friends = fd
            .list(limit, offset)
            .map_err(|e| PrivchatSDKError::Database(format!("查询好友失败: {}", e)))?;
        debug!(
            "✅ [DbActor] 查询好友成功: uid={}, count={}",
            uid,
            friends.len()
        );
        Ok(friends)
    }

    /// 处理：获取好友总数
    fn handle_get_friends_count(&self, uid: &str) -> Result<u32> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let fd = dao::FriendDao::new(conn);
        let count = fd
            .count()
            .map_err(|e| PrivchatSDKError::Database(format!("查询好友总数失败: {}", e)))?;
        debug!(
            "✅ [DbActor] 查询好友总数成功: uid={}, count={}",
            uid, count
        );
        Ok(count)
    }

    /// 处理：删除好友
    fn handle_delete_friend(&mut self, uid: &str, user_id: u64) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let fd = dao::FriendDao::new(conn);
        fd.delete_by_user_id(user_id)
            .map_err(|e| PrivchatSDKError::Database(format!("删除好友失败: {}", e)))?;
        info!(
            "✅ [DbActor] 删除好友成功: uid={}, user_id={}",
            uid, user_id
        );
        Ok(())
    }

    fn handle_save_user(&mut self, uid: &str, user: &entities::User) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let ud = dao::UserDao::new(conn);
        ud.upsert(user)
            .map_err(|e| PrivchatSDKError::Database(format!("保存用户失败: {}", e)))?;
        debug!(
            "✅ [DbActor] 保存用户成功: uid={}, user_id={}",
            uid, user.user_id
        );
        Ok(())
    }

    fn handle_save_users(&mut self, uid: &str, users: &[entities::User]) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let ud = dao::UserDao::new(conn);
        for u in users {
            ud.upsert(u)
                .map_err(|e| PrivchatSDKError::Database(format!("批量保存用户失败: {}", e)))?;
        }
        info!(
            "✅ [DbActor] 批量保存用户成功: uid={}, count={}",
            uid,
            users.len()
        );
        Ok(())
    }

    fn handle_get_user(&self, uid: &str, user_id: u64) -> Result<Option<entities::User>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let ud = dao::UserDao::new(conn);
        ud.get_by_id(user_id)
            .map_err(|e| PrivchatSDKError::Database(format!("查询用户失败: {}", e)))
    }

    fn handle_get_users_by_ids(&self, uid: &str, ids: &[u64]) -> Result<Vec<entities::User>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let ud = dao::UserDao::new(conn);
        ud.get_by_ids(ids)
            .map_err(|e| PrivchatSDKError::Database(format!("批量查询用户失败: {}", e)))
    }

    /// 处理：保存频道成员
    fn handle_save_channel_member(
        &mut self,
        uid: &str,
        member: &entities::ChannelMember,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let dao = dao::ChannelMemberDao::new(conn);
        dao.upsert(member)
            .map_err(|e| PrivchatSDKError::Database(format!("保存频道成员失败: {}", e)))?;

        debug!(
            "✅ [DbActor] 保存频道成员成功: uid={}, channel_id={}, member_uid={}",
            uid, member.channel_id, member.member_uid
        );
        Ok(())
    }

    /// 处理：批量保存频道成员
    fn handle_save_channel_members(
        &mut self,
        uid: &str,
        members: &Vec<entities::ChannelMember>,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let dao = dao::ChannelMemberDao::new(conn);
        dao.upsert_batch(members)
            .map_err(|e| PrivchatSDKError::Database(format!("批量保存频道成员失败: {}", e)))?;

        info!(
            "✅ [DbActor] 批量保存频道成员成功: uid={}, count={}",
            uid,
            members.len()
        );
        Ok(())
    }

    /// 处理：删除频道成员
    fn handle_delete_channel_member(
        &mut self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let dao = dao::ChannelMemberDao::new(conn);
        dao.delete(channel_id, channel_type, member_uid)
            .map_err(|e| PrivchatSDKError::Database(format!("删除频道成员失败: {}", e)))?;
        Ok(())
    }

    /// 处理：获取群成员列表（按 group_id，查 channel_member 中 channel_id=group_id, channel_type=2）
    fn handle_get_group_members(
        &self,
        uid: &str,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<entities::ChannelMember>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;

        let dao = dao::ChannelMemberDao::new(conn);
        let members = dao
            .list_members(group_id, 2, limit, offset)
            .map_err(|e| PrivchatSDKError::Database(format!("查询群成员失败: {}", e)))?;

        debug!(
            "✅ [DbActor] 查询群成员成功: uid={}, group_id={}, count={}",
            uid,
            group_id,
            members.len()
        );
        Ok(members)
    }

    fn handle_get_groups(
        &self,
        uid: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<entities::Group>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let dao = dao::GroupDao::new(conn);
        let list = dao
            .list(limit, offset)
            .map_err(|e| PrivchatSDKError::Database(format!("查询群列表失败: {}", e)))?;
        debug!(
            "✅ [DbActor] 查询群列表成功: uid={}, count={}",
            uid,
            list.len()
        );
        Ok(list)
    }

    fn handle_get_group(&self, uid: &str, group_id: u64) -> Result<Option<entities::Group>> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let dao = dao::GroupDao::new(conn);
        dao.get_by_id(group_id)
            .map_err(|e| PrivchatSDKError::Database(format!("查询群失败: {}", e)))
    }

    fn handle_save_groups(&mut self, uid: &str, groups: &[entities::Group]) -> Result<()> {
        let conn = self
            .connections
            .get(uid)
            .ok_or_else(|| PrivchatSDKError::Database(format!("用户数据库不存在: {}", uid)))?;
        let dao = dao::GroupDao::new(conn);
        for g in groups {
            dao.upsert(g)
                .map_err(|e| PrivchatSDKError::Database(format!("保存群失败: {}", e)))?;
        }
        info!(
            "✅ [DbActor] 批量保存群成功: uid={}, count={}",
            uid,
            groups.len()
        );
        Ok(())
    }

    fn handle_close_user(&mut self, uid: &str) -> Result<()> {
        if let Some(conn) = self.connections.remove(uid) {
            drop(conn);
            info!(
                "✅ [Thread {:?}] 已关闭用户数据库: uid={}",
                self.thread_id, uid
            );
        }
        Ok(())
    }
}

/// rusqlite::Value 转换为 serde_json::Value
fn rusqlite_value_to_json(value: rusqlite::types::Value) -> serde_json::Value {
    match value {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::Value::Number(i.into()),
        rusqlite::types::Value::Real(f) => serde_json::Value::Number(
            serde_json::Number::from_f64(f).unwrap_or(serde_json::Number::from(0)),
        ),
        rusqlite::types::Value::Text(s) => serde_json::Value::String(s),
        rusqlite::types::Value::Blob(b) => {
            serde_json::Value::String(format!("blob({} bytes)", b.len()))
        }
    }
}

/// 数据库 Actor 句柄（用于异步调用）
#[derive(Clone)]
pub struct DbActorHandle {
    sender: Sender<DbCommand>,
}

impl std::fmt::Debug for DbActorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbActorHandle")
            .field("sender", &"<channel>")
            .finish()
    }
}

impl DbActorHandle {
    /// 启动 DB Actor
    ///
    /// # 参数
    /// - `assets_path`: 可选的 assets 目录路径，用于加载 SQL 迁移文件
    ///   - 如果提供，将从指定目录读取 SQL 文件
    ///   - 如果不提供，默认使用 `./assets/` 目录
    pub fn spawn(assets_path: Option<PathBuf>) -> Self {
        let (sender, receiver) = unbounded();

        // 启动专用线程
        thread::Builder::new()
            .name("db-actor".to_string())
            .spawn(move || {
                let actor = DbActor::new(receiver, assets_path);
                actor.run();
            })
            .expect("无法启动 DB Actor 线程");

        Self { sender }
    }

    /// 初始化用户数据库
    pub async fn init_user(&self, uid: String, db_path: PathBuf) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::InitUser {
                uid,
                db_path,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新消息状态（按 message.id）
    pub async fn update_message_status(&self, uid: String, id: i64, status: i32) -> Result<()> {
        tracing::debug!(
            "📤 [DbActorHandle] 发送命令: UpdateMessageStatus(uid={}, id={}, status={})",
            uid,
            id,
            status
        );

        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateMessageStatus {
                uid: uid.clone(),
                id,
                status,
                respond_to: tx,
            })
            .map_err(|_| {
                tracing::error!("❌ [DbActorHandle] 发送命令失败: uid={}", uid);
                PrivchatSDKError::Other("DB Actor 已停止".to_string())
            })?;

        let result = rx.await.map_err(|_| {
            tracing::error!("❌ [DbActorHandle] 等待响应失败: uid={}", uid);
            PrivchatSDKError::Other("DB Actor 响应失败".to_string())
        })??;

        tracing::debug!("✅ [DbActorHandle] 命令执行成功: uid={}, id={}", uid, id);
        Ok(result)
    }

    /// 更新消息的服务端 ID（按 message.id，仅协议层写入）
    pub async fn update_message_server_id(
        &self,
        uid: String,
        id: i64,
        server_message_id: u64,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateMessageServerId {
                uid,
                id,
                server_message_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 保存接收的消息
    pub async fn save_received_message(
        &self,
        uid: String,
        message: Message,
        is_outgoing: bool,
    ) -> Result<i64> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveReceivedMessage {
                uid,
                message,
                is_outgoing,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 执行 SQL
    pub async fn execute(
        &self,
        uid: String,
        sql: String,
        params: Vec<rusqlite::types::Value>,
    ) -> Result<usize> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::Execute {
                uid,
                sql,
                params,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 查询
    pub async fn query(
        &self,
        uid: String,
        sql: String,
        params: Vec<rusqlite::types::Value>,
    ) -> Result<Vec<serde_json::Value>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::Query {
                uid,
                sql,
                params,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 根据 message.id 获取消息
    pub async fn get_message_by_id(&self, uid: String, id: i64) -> Result<Option<Message>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetMessageById {
                uid,
                id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取消息的 channel_id（按 message.id）
    pub async fn get_message_channel_id(&self, uid: String, id: i64) -> Result<Option<u64>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetMessageChannelId {
                uid,
                id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取指定 message.id 之前的消息（分页查询，游标为客户端 id）
    pub async fn get_messages_before(
        &self,
        uid: String,
        channel_id: u64,
        before_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetMessagesBefore {
                uid,
                channel_id,
                before_id,
                limit,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取频道当前最小的 message.id（用于「加载更早」分页游标）
    pub async fn get_earliest_id(&self, uid: String, channel_id: u64) -> Result<Option<u64>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetEarliestId {
                uid,
                channel_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取指定 message.id 之后的消息（向前分页，加载更新的消息）
    pub async fn get_messages_after(
        &self,
        uid: String,
        channel_id: u64,
        after_id: u64,
        limit: u32,
    ) -> Result<Vec<Message>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetMessagesAfter {
                uid,
                channel_id,
                after_id,
                limit,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 发送消息（返回 message.id）
    pub async fn send_message(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: String,
        message_type: i32,
    ) -> Result<i64> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SendMessage {
                uid,
                channel_id,
                channel_type,
                from_uid,
                content,
                message_type,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 撤回消息（按 message.id，revoker_id 取自 uid）
    pub async fn revoke_message(&self, uid: String, id: i64) -> Result<()> {
        let revoker_id = uid.parse::<u64>().unwrap_or(0);
        self.revoke_message_with_revoker(uid, id, revoker_id).await
    }

    /// 撤回消息（按 message.id，指定 revoker_id）
    pub async fn revoke_message_with_revoker(
        &self,
        uid: String,
        id: i64,
        revoker_id: u64,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::RevokeMessage {
                uid: uid.clone(),
                id,
                revoker_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 编辑消息（按 message.id）
    pub async fn edit_message(&self, uid: String, id: i64, new_content: String) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::EditMessage {
                uid,
                id,
                new_content,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 删除消息（按 message.id）
    pub async fn delete_message(&self, uid: String, id: i64) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::DeleteMessage {
                uid,
                id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新消息内容（按 message.id）
    pub async fn update_message_content(
        &self,
        uid: String,
        id: i64,
        new_content: String,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateMessageContent {
                uid,
                id,
                new_content,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 添加消息反应（按 message.id）
    pub async fn add_message_reaction(
        &self,
        uid: String,
        id: i64,
        user_id: u64,
        reaction: String,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::AddMessageReaction {
                uid,
                id,
                user_id,
                reaction,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 保存 Channel
    pub async fn save_channel(&self, uid: String, channel: entities::Channel) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveChannel {
                uid,
                channel,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取会话列表
    pub async fn get_channels(&self, uid: String) -> Result<Vec<entities::Channel>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetChannels {
                uid,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 根据频道获取会话
    pub async fn get_channel_by_channel(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<Option<entities::Channel>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetChannelByChannel {
                uid,
                channel_id,
                channel_type,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 按 channel_id 查询私聊会话（channel_type 0 或 1 均视为同一私聊，用于避免重复插入）
    pub async fn get_direct_channel_by_id(
        &self,
        uid: String,
        channel_id: u64,
    ) -> Result<Option<entities::Channel>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetDirectChannelById {
                uid,
                channel_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新会话的 pts
    pub async fn update_channel_pts(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: u8,
        new_pts: u64,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateChannelPts {
                uid,
                channel_id,
                channel_type,
                new_pts,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 根据用户查找 channel_id
    pub async fn find_channel_id_by_user(
        &self,
        uid: String,
        target_user_id: u64,
    ) -> Result<Option<u64>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::FindChannelIdByUser {
                uid,
                target_user_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新频道的 save 字段（收藏状态）
    pub async fn update_channel_save(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: u8,
        save: i32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateChannelSave {
                uid,
                channel_id,
                channel_type: channel_type as i32,
                save,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新频道的 mute 字段（通知模式）
    pub async fn update_channel_mute(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: u8,
        mute: i32,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateChannelMute {
                uid,
                channel_id,
                channel_type: channel_type as i32,
                mute,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 更新会话的 extra 字段
    pub async fn update_channel_extra(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: u8,
        extra: String,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::UpdateChannelExtra {
                uid,
                channel_id,
                channel_type: channel_type as i32,
                extra,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    // ========== User / Group / GroupMember（Entity Model V1）==========

    pub async fn save_user(&self, uid: String, user: entities::User) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::SaveUser {
                uid,
                user,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    pub async fn save_users(&self, uid: String, users: Vec<entities::User>) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::SaveUsers {
                uid,
                users,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    pub async fn get_user(&self, uid: String, user_id: u64) -> Result<Option<entities::User>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::GetUser {
                uid,
                user_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    pub async fn get_users_by_ids(
        &self,
        uid: String,
        ids: Vec<u64>,
    ) -> Result<Vec<entities::User>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::GetUsersByIds {
                uid,
                ids,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    // ========== 好友管理方法（Local-first）==========

    /// 保存单个好友
    pub async fn save_friend(&self, uid: String, friend: entities::Friend) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveFriend {
                uid,
                friend,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 批量保存好友
    pub async fn save_friends(&self, uid: String, friends: Vec<entities::Friend>) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveFriends {
                uid,
                friends,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 从本地数据库获取好友列表（分页）
    pub async fn get_friends(
        &self,
        uid: String,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<entities::Friend>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetFriends {
                uid,
                limit,
                offset,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取好友总数
    pub async fn get_friends_count(&self, uid: String) -> Result<u32> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetFriendsCount {
                uid,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 删除好友
    pub async fn delete_friend(&self, uid: String, user_id: u64) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::DeleteFriend {
                uid,
                user_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 保存频道成员
    pub async fn save_channel_member(
        &self,
        uid: String,
        member: entities::ChannelMember,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveChannelMember {
                uid,
                member,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 批量保存频道成员
    pub async fn save_channel_members(
        &self,
        uid: String,
        members: Vec<entities::ChannelMember>,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::SaveChannelMembers {
                uid,
                members,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 删除频道成员（用于 group_member tombstone 等）
    pub async fn delete_channel_member(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::DeleteChannelMember {
                uid,
                channel_id,
                channel_type,
                member_uid,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取群成员列表（按 group_id 关联）
    pub async fn get_group_members(
        &self,
        uid: String,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<entities::ChannelMember>> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::GetGroupMembers {
                uid,
                group_id,
                limit,
                offset,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 获取群列表（分页）
    pub async fn get_groups(
        &self,
        uid: String,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<entities::Group>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::GetGroups {
                uid,
                limit,
                offset,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 按 group_id 获取单个群（ENTITY_SYNC_V1 group tombstone 等）
    pub async fn get_group(&self, uid: String, group_id: u64) -> Result<Option<entities::Group>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::GetGroup {
                uid,
                group_id,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 批量保存群（ENTITY_SYNC_V1）
    pub async fn save_groups(&self, uid: String, groups: Vec<entities::Group>) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.sender
            .send(DbCommand::SaveGroups {
                uid,
                groups,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;
        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 关闭用户数据库
    pub async fn close_user(&self, uid: String) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(DbCommand::CloseUser {
                uid,
                respond_to: tx,
            })
            .map_err(|_| PrivchatSDKError::Other("DB Actor 已停止".to_string()))?;

        rx.await
            .map_err(|_| PrivchatSDKError::Other("DB Actor 响应失败".to_string()))?
    }

    /// 停止 DB Actor
    pub fn shutdown(&self) {
        let _ = self.sender.send(DbCommand::Shutdown);
    }
}
