use std::path::{Path, PathBuf};
use std::sync::Arc;

use refinery::embed_migrations;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Error, LoginResult, MentionInput, NewMessage, Result, SessionSnapshot, StoredBlacklistEntry,
    StoredChannel, StoredChannelExtra, StoredChannelMember, StoredFriend, StoredGroup,
    StoredGroupMember, StoredMessage, StoredMessageExtra, StoredMessageReaction, StoredReminder,
    StoredUser, UnreadMentionCount, UpsertBlacklistInput, UpsertChannelExtraInput,
    UpsertChannelInput, UpsertChannelMemberInput, UpsertFriendInput, UpsertGroupInput,
    UpsertGroupMemberInput, UpsertMessageReactionInput, UpsertReminderInput, UpsertUserInput,
};

mod embedded {
    use super::embed_migrations;
    embed_migrations!("migrations");
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePaths {
    pub user_root: PathBuf,
    pub db_path: PathBuf,
    pub kv_path: PathBuf,
    pub queue_root: PathBuf,
    pub normal_queue_path: PathBuf,
    pub file_queue_paths: Vec<PathBuf>,
    pub media_root: PathBuf,
}

impl StoragePaths {
    pub fn for_user(base: &Path, uid: &str) -> Self {
        let user_root = base.join("users").join(uid);
        let db_path = user_root.join("messages.db");
        let kv_path = user_root.join("kv");
        let queue_root = user_root.join("queues");
        let normal_queue_path = queue_root.join("normal");
        let file_queue_paths = vec![
            queue_root.join("file-0"),
            queue_root.join("file-1"),
            queue_root.join("file-2"),
        ];
        let media_root = user_root.join("media");
        Self {
            user_root,
            db_path,
            kv_path,
            queue_root,
            normal_queue_path,
            file_queue_paths,
            media_root,
        }
    }
}

#[derive(Clone)]
pub struct LocalStore {
    base_dir: Arc<PathBuf>,
}

impl LocalStore {
    fn current_user_file(&self) -> PathBuf {
        self.base_dir().join("current_user")
    }

    pub fn open_at(base: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&base)
            .map_err(|e| Error::Storage(format!("create data dir: {e}")))?;
        Ok(Self {
            base_dir: Arc::new(base),
        })
    }

    pub fn open_default() -> Result<Self> {
        let base = std::env::var("PRIVCHAT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".privchat-rust")
            });
        Self::open_at(base)
    }

    pub fn base_dir(&self) -> &Path {
        self.base_dir.as_path()
    }

    pub fn storage_paths(&self, uid: &str) -> StoragePaths {
        StoragePaths::for_user(self.base_dir(), uid)
    }

    pub fn ensure_user_storage(&self, uid: &str) -> Result<StoragePaths> {
        let paths = self.storage_paths(uid);
        std::fs::create_dir_all(&paths.user_root)
            .map_err(|e| Error::Storage(format!("create user root: {e}")))?;
        std::fs::create_dir_all(&paths.kv_path)
            .map_err(|e| Error::Storage(format!("create kv path: {e}")))?;
        std::fs::create_dir_all(&paths.queue_root)
            .map_err(|e| Error::Storage(format!("create queue root: {e}")))?;
        std::fs::create_dir_all(&paths.media_root)
            .map_err(|e| Error::Storage(format!("create media root: {e}")))?;
        for p in &paths.file_queue_paths {
            std::fs::create_dir_all(p)
                .map_err(|e| Error::Storage(format!("create file queue path: {e}")))?;
        }
        std::fs::create_dir_all(&paths.normal_queue_path)
            .map_err(|e| Error::Storage(format!("create normal queue path: {e}")))?;
        self.init_user_db(uid, &paths.db_path)?;
        Ok(paths)
    }

    fn init_user_db(&self, uid: &str, db_path: &Path) -> Result<()> {
        let mut conn =
            Connection::open(db_path).map_err(|e| Error::Storage(format!("open db: {e}")))?;
        let key = Self::derive_encryption_key(uid);
        conn.pragma_update(None, "key", &key)
            .map_err(|e| Error::Storage(format!("set db key: {e}")))?;

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| Error::Storage(format!("set wal: {e}")))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| Error::Storage(format!("set sync: {e}")))?;

        embedded::migrations::runner()
            .run(&mut conn)
            .map_err(|e| Error::Storage(format!("run migrations: {e}")))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS auth_session (
                id INTEGER PRIMARY KEY CHECK(id=1),
                user_id INTEGER NOT NULL,
                token TEXT NOT NULL,
                device_id TEXT NOT NULL,
                bootstrap_completed INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            );",
        )
        .map_err(|e| Error::Storage(format!("create auth_session: {e}")))?;
        Ok(())
    }

    fn conn_for_user(&self, uid: &str) -> Result<Connection> {
        let paths = self.ensure_user_storage(uid)?;
        let conn =
            Connection::open(paths.db_path).map_err(|e| Error::Storage(format!("open db: {e}")))?;
        let key = Self::derive_encryption_key(uid);
        conn.pragma_update(None, "key", &key)
            .map_err(|e| Error::Storage(format!("set db key: {e}")))?;
        Ok(conn)
    }

    pub fn save_login(&self, uid: &str, login: &LoginResult) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO auth_session(id, user_id, token, device_id, bootstrap_completed, updated_at)
             VALUES (1, ?1, ?2, ?3, COALESCE((SELECT bootstrap_completed FROM auth_session WHERE id=1),0), ?4)
             ON CONFLICT(id) DO UPDATE SET
               user_id=excluded.user_id,
               token=excluded.token,
               device_id=excluded.device_id,
               updated_at=excluded.updated_at",
            params![login.user_id, login.token, login.device_id, now],
        )
        .map_err(|e| Error::Storage(format!("save login: {e}")))?;
        self.save_current_uid(uid)?;
        Ok(())
    }

    pub fn set_bootstrap_completed(&self, uid: &str, completed: bool) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE auth_session SET bootstrap_completed=?1, updated_at=?2 WHERE id=1",
            params![
                if completed { 1 } else { 0 },
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .map_err(|e| Error::Storage(format!("set bootstrap completed: {e}")))?;
        Ok(())
    }

    pub fn load_session(&self, uid: &str) -> Result<Option<SessionSnapshot>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT user_id, token, device_id, bootstrap_completed FROM auth_session WHERE id=1",
            [],
            |row| {
                Ok(SessionSnapshot {
                    user_id: row.get::<_, u64>(0)?,
                    token: row.get::<_, String>(1)?,
                    device_id: row.get::<_, String>(2)?,
                    bootstrap_completed: row.get::<_, i64>(3)? != 0,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("load session: {e}")))
    }

    pub fn clear_session(&self, uid: &str) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute("DELETE FROM auth_session", [])
            .map_err(|e| Error::Storage(format!("clear session: {e}")))?;
        Ok(())
    }

    pub fn create_local_message(&self, uid: &str, input: &NewMessage) -> Result<u64> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO message (
                message_id, channel_id, channel_type, from_uid, type, content,
                status, created_at, updated_at, searchable_word, local_message_id, setting, extra
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                Option::<i64>::None,
                input.channel_id as i64,
                input.channel_type,
                input.from_uid as i64,
                input.message_type,
                input.content,
                0_i32,
                now_ms,
                now_ms,
                input.searchable_word,
                0_i64,
                input.setting,
                input.extra
            ],
        )
        .map_err(|e| Error::Storage(format!("insert message: {e}")))?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn get_message_by_id(&self, uid: &str, message_id: u64) -> Result<Option<StoredMessage>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                id, message_id, channel_id, channel_type, from_uid, type,
                content, status, created_at, updated_at, extra
             FROM message WHERE id = ?1 LIMIT 1",
            params![message_id as i64],
            |row| {
                Ok(StoredMessage {
                    message_id: row.get::<_, i64>(0)? as u64,
                    server_message_id: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    channel_id: row.get::<_, i64>(2)? as u64,
                    channel_type: row.get::<_, i32>(3)?,
                    from_uid: row.get::<_, i64>(4)? as u64,
                    message_type: row.get::<_, i32>(5)?,
                    content: row.get::<_, String>(6)?,
                    status: row.get::<_, i32>(7)?,
                    created_at: row.get::<_, i64>(8)?,
                    updated_at: row.get::<_, i64>(9)?,
                    extra: row.get::<_, String>(10)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get message by id: {e}")))
    }

    pub fn list_messages(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    id, message_id, channel_id, channel_type, from_uid, type,
                    content, status, created_at, updated_at, extra
                 FROM message
                 WHERE channel_id = ?1 AND channel_type = ?2
                 ORDER BY id DESC
                 LIMIT ?3 OFFSET ?4",
            )
            .map_err(|e| Error::Storage(format!("prepare list messages: {e}")))?;
        let rows = stmt
            .query_map(
                params![channel_id as i64, channel_type, limit as i64, offset as i64],
                |row| {
                    Ok(StoredMessage {
                        message_id: row.get::<_, i64>(0)? as u64,
                        server_message_id: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                        channel_id: row.get::<_, i64>(2)? as u64,
                        channel_type: row.get::<_, i32>(3)?,
                        from_uid: row.get::<_, i64>(4)? as u64,
                        message_type: row.get::<_, i32>(5)?,
                        content: row.get::<_, String>(6)?,
                        status: row.get::<_, i32>(7)?,
                        created_at: row.get::<_, i64>(8)?,
                        updated_at: row.get::<_, i64>(9)?,
                        extra: row.get::<_, String>(10)?,
                    })
                },
            )
            .map_err(|e| Error::Storage(format!("query list messages: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list messages row: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_channel(&self, uid: &str, input: &UpsertChannelInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO channel (
                channel_id, channel_type, channel_name, channel_remark, avatar,
                unread_count, top, mute, last_msg_timestamp, last_local_message_id,
                last_msg_content, updated_at, created_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?12
             )
             ON CONFLICT(channel_id) DO UPDATE SET
                channel_type=excluded.channel_type,
                channel_name=excluded.channel_name,
                channel_remark=excluded.channel_remark,
                avatar=excluded.avatar,
                unread_count=excluded.unread_count,
                top=excluded.top,
                mute=excluded.mute,
                last_msg_timestamp=excluded.last_msg_timestamp,
                last_local_message_id=excluded.last_local_message_id,
                last_msg_content=excluded.last_msg_content,
                updated_at=excluded.updated_at",
            params![
                input.channel_id as i64,
                input.channel_type,
                input.channel_name,
                input.channel_remark,
                input.avatar,
                input.unread_count,
                input.top,
                input.mute,
                input.last_msg_timestamp,
                input.last_local_message_id as i64,
                input.last_msg_content,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert channel: {e}")))?;
        Ok(())
    }

    pub fn get_channel_by_id(&self, uid: &str, channel_id: u64) -> Result<Option<StoredChannel>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                channel_id, channel_type, channel_name, channel_remark, avatar,
                unread_count, top, mute, last_msg_timestamp, last_local_message_id,
                last_msg_content, updated_at
             FROM channel
             WHERE channel_id = ?1
             LIMIT 1",
            params![channel_id as i64],
            |row| {
                Ok(StoredChannel {
                    channel_id: row.get::<_, i64>(0)? as u64,
                    channel_type: row.get::<_, i32>(1)?,
                    channel_name: row.get::<_, String>(2)?,
                    channel_remark: row.get::<_, String>(3)?,
                    avatar: row.get::<_, String>(4)?,
                    unread_count: row.get::<_, i32>(5)?,
                    top: row.get::<_, i32>(6)?,
                    mute: row.get::<_, i32>(7)?,
                    last_msg_timestamp: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                    last_local_message_id: row.get::<_, Option<i64>>(9)?.unwrap_or_default() as u64,
                    last_msg_content: row.get::<_, String>(10)?,
                    updated_at: row.get::<_, i64>(11)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get channel by id: {e}")))
    }

    pub fn list_channels(
        &self,
        uid: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannel>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT
                    channel_id, channel_type, channel_name, channel_remark, avatar,
                    unread_count, top, mute, last_msg_timestamp, last_local_message_id,
                    last_msg_content, updated_at
                 FROM channel
                 ORDER BY top DESC, last_msg_timestamp DESC, channel_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list channels: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredChannel {
                    channel_id: row.get::<_, i64>(0)? as u64,
                    channel_type: row.get::<_, i32>(1)?,
                    channel_name: row.get::<_, String>(2)?,
                    channel_remark: row.get::<_, String>(3)?,
                    avatar: row.get::<_, String>(4)?,
                    unread_count: row.get::<_, i32>(5)?,
                    top: row.get::<_, i32>(6)?,
                    mute: row.get::<_, i32>(7)?,
                    last_msg_timestamp: row.get::<_, Option<i64>>(8)?.unwrap_or_default(),
                    last_local_message_id: row.get::<_, Option<i64>>(9)?.unwrap_or_default() as u64,
                    last_msg_content: row.get::<_, String>(10)?,
                    updated_at: row.get::<_, i64>(11)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list channels: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list channels row: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_channel_extra(&self, uid: &str, input: &UpsertChannelExtraInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO channel_extra (
                channel_id, channel_type, browse_to, keep_pts, keep_offset_y,
                draft, version, draft_updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8
             )
             ON CONFLICT(channel_id, channel_type) DO UPDATE SET
                browse_to=excluded.browse_to,
                keep_pts=excluded.keep_pts,
                keep_offset_y=excluded.keep_offset_y,
                draft=excluded.draft,
                version=excluded.version,
                draft_updated_at=excluded.draft_updated_at",
            params![
                input.channel_id as i64,
                input.channel_type,
                input.browse_to as i64,
                input.keep_pts as i64,
                input.keep_offset_y,
                input.draft,
                now_ms,
                input.draft_updated_at as i64
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert channel_extra: {e}")))?;
        Ok(())
    }

    pub fn get_channel_extra(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<StoredChannelExtra>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                channel_id, channel_type, browse_to, keep_pts, keep_offset_y,
                draft, draft_updated_at, version
             FROM channel_extra
             WHERE channel_id = ?1 AND channel_type = ?2
             LIMIT 1",
            params![channel_id as i64, channel_type],
            |row| {
                Ok(StoredChannelExtra {
                    channel_id: row.get::<_, i64>(0)? as u64,
                    channel_type: row.get::<_, i32>(1)?,
                    browse_to: row.get::<_, i64>(2)? as u64,
                    keep_pts: row.get::<_, i64>(3)? as u64,
                    keep_offset_y: row.get::<_, i32>(4)?,
                    draft: row.get::<_, String>(5)?,
                    draft_updated_at: row.get::<_, i64>(6)? as u64,
                    version: row.get::<_, i64>(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get channel_extra: {e}")))
    }

    pub fn upsert_user(&self, uid: &str, input: &UpsertUserInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO user (
                user_id, username, nickname, alias, avatar,
                user_type, is_deleted, channel_id, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(user_id) DO UPDATE SET
                username=excluded.username,
                nickname=excluded.nickname,
                alias=excluded.alias,
                avatar=excluded.avatar,
                user_type=excluded.user_type,
                is_deleted=excluded.is_deleted,
                channel_id=excluded.channel_id,
                updated_at=excluded.updated_at",
            params![
                input.user_id as i64,
                input.username,
                input.nickname,
                input.alias,
                input.avatar,
                input.user_type,
                if input.is_deleted { 1 } else { 0 },
                input.channel_id,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert user: {e}")))?;
        Ok(())
    }

    pub fn get_user_by_id(&self, uid: &str, user_id: u64) -> Result<Option<StoredUser>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, updated_at
             FROM user
             WHERE user_id = ?1
             LIMIT 1",
            params![user_id as i64],
            |row| {
                Ok(StoredUser {
                    user_id: row.get::<_, i64>(0)? as u64,
                    username: row.get::<_, Option<String>>(1)?,
                    nickname: row.get::<_, Option<String>>(2)?,
                    alias: row.get::<_, Option<String>>(3)?,
                    avatar: row.get::<_, String>(4)?,
                    user_type: row.get::<_, i32>(5)?,
                    is_deleted: row.get::<_, i32>(6)? != 0,
                    channel_id: row.get::<_, String>(7)?,
                    updated_at: row.get::<_, i64>(8)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get user by id: {e}")))
    }

    pub fn list_users_by_ids(&self, uid: &str, user_ids: &[u64]) -> Result<Vec<StoredUser>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn_for_user(uid)?;
        let placeholders = std::iter::repeat("?")
            .take(user_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, updated_at
             FROM user
             WHERE user_id IN ({})
             ORDER BY updated_at DESC, user_id DESC",
            placeholders
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::Storage(format!("prepare list users by ids: {e}")))?;
        let params_dyn = rusqlite::params_from_iter(user_ids.iter().map(|v| *v as i64));
        let rows = stmt
            .query_map(params_dyn, |row| {
                Ok(StoredUser {
                    user_id: row.get::<_, i64>(0)? as u64,
                    username: row.get::<_, Option<String>>(1)?,
                    nickname: row.get::<_, Option<String>>(2)?,
                    alias: row.get::<_, Option<String>>(3)?,
                    avatar: row.get::<_, String>(4)?,
                    user_type: row.get::<_, i32>(5)?,
                    is_deleted: row.get::<_, i32>(6)? != 0,
                    channel_id: row.get::<_, String>(7)?,
                    updated_at: row.get::<_, i64>(8)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list users by ids: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list users by ids: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_friend(&self, uid: &str, input: &UpsertFriendInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO friend (user_id, tags, is_pinned, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id) DO UPDATE SET
                tags=excluded.tags,
                is_pinned=excluded.is_pinned,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at",
            params![
                input.user_id as i64,
                input.tags,
                if input.is_pinned { 1 } else { 0 },
                input.created_at,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert friend: {e}")))?;
        Ok(())
    }

    pub fn delete_friend(&self, uid: &str, user_id: u64) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "DELETE FROM friend WHERE user_id = ?1",
            params![user_id as i64],
        )
        .map_err(|e| Error::Storage(format!("delete friend: {e}")))?;
        Ok(())
    }

    pub fn list_friends(
        &self,
        uid: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredFriend>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT user_id, tags, is_pinned, created_at, updated_at
                 FROM friend
                 ORDER BY is_pinned DESC, updated_at DESC, user_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list friends: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredFriend {
                    user_id: row.get::<_, i64>(0)? as u64,
                    tags: row.get::<_, Option<String>>(1)?,
                    is_pinned: row.get::<_, i32>(2)? != 0,
                    created_at: row.get::<_, i64>(3)?,
                    updated_at: row.get::<_, i64>(4)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list friends: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list friends: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_blacklist_entry(&self, uid: &str, input: &UpsertBlacklistInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO blacklist (blocked_user_id, created_at, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(blocked_user_id) DO UPDATE SET
                created_at=excluded.created_at,
                updated_at=excluded.updated_at",
            params![
                input.blocked_user_id as i64,
                input.created_at,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert blacklist entry: {e}")))?;
        Ok(())
    }

    pub fn delete_blacklist_entry(&self, uid: &str, blocked_user_id: u64) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "DELETE FROM blacklist WHERE blocked_user_id = ?1",
            params![blocked_user_id as i64],
        )
        .map_err(|e| Error::Storage(format!("delete blacklist entry: {e}")))?;
        Ok(())
    }

    pub fn list_blacklist_entries(
        &self,
        uid: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredBlacklistEntry>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT blocked_user_id, created_at, updated_at
                 FROM blacklist
                 ORDER BY updated_at DESC, blocked_user_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list blacklist entries: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredBlacklistEntry {
                    blocked_user_id: row.get::<_, i64>(0)? as u64,
                    created_at: row.get::<_, i64>(1)?,
                    updated_at: row.get::<_, i64>(2)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list blacklist entries: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| Error::Storage(format!("decode list blacklist entries: {e}")))?,
            );
        }
        Ok(out)
    }

    pub fn upsert_group(&self, uid: &str, input: &UpsertGroupInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO \"group\" (group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(group_id) DO UPDATE SET
                name=excluded.name,
                avatar=excluded.avatar,
                owner_id=excluded.owner_id,
                is_dismissed=excluded.is_dismissed,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at",
            params![
                input.group_id as i64,
                input.name,
                input.avatar,
                input.owner_id.map(|v| v as i64),
                if input.is_dismissed { 1 } else { 0 },
                input.created_at,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert group: {e}")))?;
        Ok(())
    }

    pub fn get_group_by_id(&self, uid: &str, group_id: u64) -> Result<Option<StoredGroup>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at
             FROM \"group\"
             WHERE group_id = ?1
             LIMIT 1",
            params![group_id as i64],
            |row| {
                Ok(StoredGroup {
                    group_id: row.get::<_, i64>(0)? as u64,
                    name: row.get::<_, Option<String>>(1)?,
                    avatar: row.get::<_, String>(2)?,
                    owner_id: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    is_dismissed: row.get::<_, i32>(4)? != 0,
                    created_at: row.get::<_, i64>(5)?,
                    updated_at: row.get::<_, i64>(6)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get group by id: {e}")))
    }

    pub fn list_groups(&self, uid: &str, limit: usize, offset: usize) -> Result<Vec<StoredGroup>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, updated_at
                 FROM \"group\"
                 ORDER BY updated_at DESC, group_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list groups: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredGroup {
                    group_id: row.get::<_, i64>(0)? as u64,
                    name: row.get::<_, Option<String>>(1)?,
                    avatar: row.get::<_, String>(2)?,
                    owner_id: row.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                    is_dismissed: row.get::<_, i32>(4)? != 0,
                    created_at: row.get::<_, i64>(5)?,
                    updated_at: row.get::<_, i64>(6)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list groups: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list groups: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_group_member(&self, uid: &str, input: &UpsertGroupMemberInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO group_member (
                group_id, user_id, role, status, alias, is_muted, joined_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(group_id, user_id) DO UPDATE SET
                role=excluded.role,
                status=excluded.status,
                alias=excluded.alias,
                is_muted=excluded.is_muted,
                joined_at=excluded.joined_at,
                updated_at=excluded.updated_at",
            params![
                input.group_id as i64,
                input.user_id as i64,
                input.role,
                input.status,
                input.alias,
                if input.is_muted { 1 } else { 0 },
                input.joined_at,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert group member: {e}")))?;
        Ok(())
    }

    pub fn delete_group_member(&self, uid: &str, group_id: u64, user_id: u64) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            params![group_id as i64, user_id as i64],
        )
        .map_err(|e| Error::Storage(format!("delete group member: {e}")))?;
        Ok(())
    }

    pub fn list_group_members(
        &self,
        uid: &str,
        group_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroupMember>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT group_id, user_id, role, status, alias, is_muted, joined_at, updated_at
                 FROM group_member
                 WHERE group_id = ?1
                 ORDER BY updated_at DESC, user_id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| Error::Storage(format!("prepare list group members: {e}")))?;
        let rows = stmt
            .query_map(
                params![group_id as i64, limit as i64, offset as i64],
                |row| {
                    Ok(StoredGroupMember {
                        group_id: row.get::<_, i64>(0)? as u64,
                        user_id: row.get::<_, i64>(1)? as u64,
                        role: row.get::<_, i32>(2)?,
                        status: row.get::<_, i32>(3)?,
                        alias: row.get::<_, Option<String>>(4)?,
                        is_muted: row.get::<_, i32>(5)? != 0,
                        joined_at: row.get::<_, i64>(6)?,
                        updated_at: row.get::<_, i64>(7)?,
                    })
                },
            )
            .map_err(|e| Error::Storage(format!("query list group members: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode list group members: {e}")))?);
        }
        Ok(out)
    }

    pub fn upsert_channel_member(&self, uid: &str, input: &UpsertChannelMemberInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO channel_member (
                channel_id, channel_type, member_uid, member_name, member_remark, member_avatar,
                member_invite_uid, role, status, is_deleted, robot, version, created_at, updated_at,
                extra, forbidden_expiration_time, member_avatar_cache_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(channel_id, channel_type, member_uid) DO UPDATE SET
                member_name=excluded.member_name,
                member_remark=excluded.member_remark,
                member_avatar=excluded.member_avatar,
                member_invite_uid=excluded.member_invite_uid,
                role=excluded.role,
                status=excluded.status,
                is_deleted=excluded.is_deleted,
                robot=excluded.robot,
                version=excluded.version,
                created_at=excluded.created_at,
                updated_at=excluded.updated_at,
                extra=excluded.extra,
                forbidden_expiration_time=excluded.forbidden_expiration_time,
                member_avatar_cache_key=excluded.member_avatar_cache_key",
            params![
                input.channel_id as i64,
                input.channel_type,
                input.member_uid as i64,
                input.member_name,
                input.member_remark,
                input.member_avatar,
                input.member_invite_uid as i64,
                input.role,
                input.status,
                if input.is_deleted { 1 } else { 0 },
                input.robot,
                input.version,
                input.created_at,
                input.updated_at,
                input.extra,
                input.forbidden_expiration_time,
                input.member_avatar_cache_key
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert channel_member: {e}")))?;
        Ok(())
    }

    pub fn list_channel_members(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannelMember>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, channel_type, member_uid, member_name, member_remark, member_avatar,
                        member_invite_uid, role, status, is_deleted, robot, version, created_at, updated_at,
                        extra, forbidden_expiration_time, member_avatar_cache_key
                 FROM channel_member
                 WHERE channel_id = ?1 AND channel_type = ?2
                 ORDER BY updated_at DESC, member_uid DESC
                 LIMIT ?3 OFFSET ?4",
            )
            .map_err(|e| Error::Storage(format!("prepare list channel members: {e}")))?;
        let rows = stmt
            .query_map(
                params![channel_id as i64, channel_type, limit as i64, offset as i64],
                |row| {
                    Ok(StoredChannelMember {
                        channel_id: row.get::<_, i64>(0)? as u64,
                        channel_type: row.get::<_, i32>(1)?,
                        member_uid: row.get::<_, i64>(2)? as u64,
                        member_name: row.get::<_, String>(3)?,
                        member_remark: row.get::<_, String>(4)?,
                        member_avatar: row.get::<_, String>(5)?,
                        member_invite_uid: row.get::<_, i64>(6)? as u64,
                        role: row.get::<_, i32>(7)?,
                        status: row.get::<_, i32>(8)?,
                        is_deleted: row.get::<_, i32>(9)? != 0,
                        robot: row.get::<_, i32>(10)?,
                        version: row.get::<_, i64>(11)?,
                        created_at: row.get::<_, i64>(12)?,
                        updated_at: row.get::<_, i64>(13)?,
                        extra: row.get::<_, String>(14)?,
                        forbidden_expiration_time: row.get::<_, i64>(15)?,
                        member_avatar_cache_key: row.get::<_, String>(16)?,
                    })
                },
            )
            .map_err(|e| Error::Storage(format!("query list channel members: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode channel member row: {e}")))?);
        }
        Ok(out)
    }

    pub fn delete_channel_member(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "DELETE FROM channel_member
             WHERE channel_id = ?1 AND channel_type = ?2 AND member_uid = ?3",
            params![channel_id as i64, channel_type, member_uid as i64],
        )
        .map_err(|e| Error::Storage(format!("delete channel member: {e}")))?;
        Ok(())
    }

    pub fn upsert_message_reaction(
        &self,
        uid: &str,
        input: &UpsertMessageReactionInput,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO message_reaction (
                channel_id, channel_type, uid, name, emoji, message_id, seq, is_deleted, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(message_id, uid, emoji) DO UPDATE SET
                channel_id=excluded.channel_id,
                channel_type=excluded.channel_type,
                name=excluded.name,
                seq=excluded.seq,
                is_deleted=excluded.is_deleted,
                created_at=excluded.created_at",
            params![
                input.channel_id as i64,
                input.channel_type,
                input.uid as i64,
                input.name,
                input.emoji,
                input.message_id as i64,
                input.seq,
                if input.is_deleted { 1 } else { 0 },
                input.created_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert message_reaction: {e}")))?;
        Ok(())
    }

    pub fn list_message_reactions(
        &self,
        uid: &str,
        message_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessageReaction>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT id, channel_id, channel_type, uid, name, emoji, message_id, seq, is_deleted, created_at
                 FROM message_reaction
                 WHERE message_id = ?1
                 ORDER BY seq DESC, id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| Error::Storage(format!("prepare list message_reactions: {e}")))?;
        let rows = stmt
            .query_map(
                params![message_id as i64, limit as i64, offset as i64],
                |row| {
                    Ok(StoredMessageReaction {
                        id: row.get::<_, i64>(0)? as u64,
                        channel_id: row.get::<_, i64>(1)? as u64,
                        channel_type: row.get::<_, i32>(2)?,
                        uid: row.get::<_, i64>(3)? as u64,
                        name: row.get::<_, String>(4)?,
                        emoji: row.get::<_, String>(5)?,
                        message_id: row.get::<_, i64>(6)? as u64,
                        seq: row.get::<_, i64>(7)?,
                        is_deleted: row.get::<_, i32>(8)? != 0,
                        created_at: row.get::<_, i64>(9)?,
                    })
                },
            )
            .map_err(|e| Error::Storage(format!("query list message_reactions: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode message_reaction row: {e}")))?);
        }
        Ok(out)
    }

    pub fn record_mention(&self, uid: &str, input: &MentionInput) -> Result<u64> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT OR IGNORE INTO mention (
                message_id, channel_id, channel_type, mentioned_user_id, sender_id, is_mention_all, created_at, is_read
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            params![
                input.message_id as i64,
                input.channel_id as i64,
                input.channel_type,
                input.mentioned_user_id as i64,
                input.sender_id as i64,
                if input.is_mention_all { 1 } else { 0 },
                input.created_at
            ],
        )
        .map_err(|e| Error::Storage(format!("record mention: {e}")))?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn get_unread_mention_count(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT COUNT(*) FROM mention
             WHERE channel_id = ?1 AND channel_type = ?2
               AND mentioned_user_id = ?3 AND is_read = 0",
            params![channel_id as i64, channel_type, user_id as i64],
            |row| row.get::<_, i32>(0),
        )
        .map_err(|e| Error::Storage(format!("get unread mention count: {e}")))
    }

    pub fn list_unread_mention_message_ids(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
    ) -> Result<Vec<u64>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT message_id FROM mention
                 WHERE channel_id = ?1 AND channel_type = ?2
                   AND mentioned_user_id = ?3 AND is_read = 0
                 ORDER BY created_at DESC
                 LIMIT ?4",
            )
            .map_err(|e| Error::Storage(format!("prepare unread mention message ids: {e}")))?;
        let rows = stmt
            .query_map(
                params![
                    channel_id as i64,
                    channel_type,
                    user_id as i64,
                    limit as i64
                ],
                |row| Ok(row.get::<_, i64>(0)? as u64),
            )
            .map_err(|e| Error::Storage(format!("query unread mention message ids: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode unread mention id: {e}")))?);
        }
        Ok(out)
    }

    pub fn mark_mention_read(&self, uid: &str, message_id: u64, user_id: u64) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE mention SET is_read = 1
             WHERE message_id = ?1 AND mentioned_user_id = ?2",
            params![message_id as i64, user_id as i64],
        )
        .map_err(|e| Error::Storage(format!("mark mention read: {e}")))?;
        Ok(())
    }

    pub fn mark_all_mentions_read(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE mention SET is_read = 1
             WHERE channel_id = ?1 AND channel_type = ?2
               AND mentioned_user_id = ?3 AND is_read = 0",
            params![channel_id as i64, channel_type, user_id as i64],
        )
        .map_err(|e| Error::Storage(format!("mark all mentions read: {e}")))?;
        Ok(())
    }

    pub fn get_all_unread_mention_counts(
        &self,
        uid: &str,
        user_id: u64,
    ) -> Result<Vec<UnreadMentionCount>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT channel_id, channel_type, COUNT(*) as unread_count
                 FROM mention
                 WHERE mentioned_user_id = ?1 AND is_read = 0
                 GROUP BY channel_id, channel_type",
            )
            .map_err(|e| Error::Storage(format!("prepare unread mention counts: {e}")))?;
        let rows = stmt
            .query_map(params![user_id as i64], |row| {
                Ok(UnreadMentionCount {
                    channel_id: row.get::<_, i64>(0)? as u64,
                    channel_type: row.get::<_, i32>(1)?,
                    unread_count: row.get::<_, i32>(2)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query unread mention counts: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(
                row.map_err(|e| Error::Storage(format!("decode unread mention counts row: {e}")))?,
            );
        }
        Ok(out)
    }

    pub fn upsert_reminder(&self, uid: &str, input: &UpsertReminderInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO reminder (
                reminder_id, message_id, pts, channel_id, channel_type, uid, type, text, data,
                is_locate, version, done, need_upload, publisher
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(reminder_id) DO UPDATE SET
                message_id=excluded.message_id,
                pts=excluded.pts,
                channel_id=excluded.channel_id,
                channel_type=excluded.channel_type,
                uid=excluded.uid,
                type=excluded.type,
                text=excluded.text,
                data=excluded.data,
                is_locate=excluded.is_locate,
                version=excluded.version,
                done=excluded.done,
                need_upload=excluded.need_upload,
                publisher=excluded.publisher",
            params![
                input.reminder_id as i64,
                input.message_id as i64,
                input.pts,
                input.channel_id as i64,
                input.channel_type,
                input.uid as i64,
                input.reminder_type,
                input.text,
                input.data,
                if input.is_locate { 1 } else { 0 },
                input.version,
                if input.done { 1 } else { 0 },
                if input.need_upload { 1 } else { 0 },
                input.publisher.map(|v| v as i64)
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert reminder: {e}")))?;
        Ok(())
    }

    pub fn list_pending_reminders(
        &self,
        uid: &str,
        reminder_uid: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredReminder>> {
        let conn = self.conn_for_user(uid)?;
        let mut stmt = conn
            .prepare(
                "SELECT id, reminder_id, message_id, pts, channel_id, channel_type, uid, type, text, data,
                        is_locate, version, done, need_upload, publisher
                 FROM reminder
                 WHERE uid = ?1 AND done = 0
                 ORDER BY version DESC, id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| Error::Storage(format!("prepare list pending reminders: {e}")))?;
        let rows = stmt
            .query_map(
                params![reminder_uid as i64, limit as i64, offset as i64],
                |row| {
                    Ok(StoredReminder {
                        id: row.get::<_, i64>(0)? as u64,
                        reminder_id: row.get::<_, i64>(1)? as u64,
                        message_id: row.get::<_, i64>(2)? as u64,
                        pts: row.get::<_, i64>(3)?,
                        channel_id: row.get::<_, i64>(4)? as u64,
                        channel_type: row.get::<_, i32>(5)?,
                        uid: row.get::<_, i64>(6)? as u64,
                        reminder_type: row.get::<_, i32>(7)?,
                        text: row.get::<_, String>(8)?,
                        data: row.get::<_, String>(9)?,
                        is_locate: row.get::<_, i32>(10)? != 0,
                        version: row.get::<_, i64>(11)?,
                        done: row.get::<_, i32>(12)? != 0,
                        need_upload: row.get::<_, i32>(13)? != 0,
                        publisher: row.get::<_, Option<i64>>(14)?.map(|v| v as u64),
                    })
                },
            )
            .map_err(|e| Error::Storage(format!("query list pending reminders: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| Error::Storage(format!("decode pending reminder row: {e}")))?);
        }
        Ok(out)
    }

    pub fn mark_reminder_done(&self, uid: &str, reminder_id: u64, done: bool) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE reminder SET done = ?1 WHERE reminder_id = ?2",
            params![if done { 1 } else { 0 }, reminder_id as i64],
        )
        .map_err(|e| Error::Storage(format!("mark reminder done: {e}")))?;
        Ok(())
    }

    pub fn mark_message_sent(
        &self,
        uid: &str,
        message_id: u64,
        server_message_id: u64,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = conn
            .execute(
                "UPDATE message
                 SET message_id = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![server_message_id as i64, now_ms, message_id as i64],
            )
            .map_err(|e| Error::Storage(format!("mark message sent: {e}")))?;
        if updated == 0 {
            return Err(Error::Storage(format!(
                "mark message sent failed: message.id={} not found",
                message_id
            )));
        }
        Ok(())
    }

    pub fn update_local_message_id(
        &self,
        uid: &str,
        message_id: u64,
        local_message_id: u64,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = conn
            .execute(
                "UPDATE message
                 SET local_message_id = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![local_message_id as i64, now_ms, message_id as i64],
            )
            .map_err(|e| Error::Storage(format!("update local_message_id: {e}")))?;
        if updated == 0 {
            return Err(Error::Storage(format!(
                "update local_message_id failed: message.id={} not found",
                message_id
            )));
        }
        Ok(())
    }

    pub fn update_message_status(&self, uid: &str, message_id: u64, status: i32) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = conn
            .execute(
                "UPDATE message
                 SET status = ?1, updated_at = ?2
                 WHERE id = ?3",
                params![status, now_ms, message_id as i64],
            )
            .map_err(|e| Error::Storage(format!("update message status: {e}")))?;
        if updated == 0 {
            return Err(Error::Storage(format!(
                "update message status failed: message.id={} not found",
                message_id
            )));
        }
        Ok(())
    }

    pub fn set_message_read(
        &self,
        uid: &str,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let (readed, unread_count, readed_count) = if is_read {
            (1_i32, 0_i32, 1_i32)
        } else {
            (0_i32, 1_i32, 0_i32)
        };
        conn.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, readed, readed_count, unread_count, extra_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(message_id) DO UPDATE SET
                readed=excluded.readed,
                readed_count=excluded.readed_count,
                unread_count=excluded.unread_count,
                extra_version=excluded.extra_version",
            params![
                message_id as i64,
                channel_id as i64,
                channel_type,
                readed,
                readed_count,
                unread_count,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("set message read: {e}")))?;

        let unread_total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(unread_count), 0)
                 FROM message_extra
                 WHERE channel_id = ?1 AND channel_type = ?2",
                params![channel_id as i64, channel_type],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("query unread total: {e}")))?;

        conn.execute(
            "INSERT INTO channel (channel_id, channel_type, unread_count, updated_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(channel_id) DO UPDATE SET
                unread_count=excluded.unread_count,
                updated_at=excluded.updated_at",
            params![channel_id as i64, channel_type, unread_total as i32, now_ms],
        )
        .map_err(|e| Error::Storage(format!("upsert channel unread: {e}")))?;
        Ok(())
    }

    pub fn set_message_revoke(
        &self,
        uid: &str,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let (channel_id, channel_type): (i64, i32) = conn
            .query_row(
                "SELECT channel_id, channel_type FROM message WHERE id = ?1",
                params![message_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::Storage(format!("query message for revoke: {e}")))?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, revoke, revoker, extra_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(message_id) DO UPDATE SET
                revoke=excluded.revoke,
                revoker=excluded.revoker,
                extra_version=excluded.extra_version",
            params![
                message_id as i64,
                channel_id,
                channel_type,
                if revoked { 1 } else { 0 },
                revoker.map(|v| v as i64),
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("set message revoke: {e}")))?;
        Ok(())
    }

    pub fn edit_message(
        &self,
        uid: &str,
        message_id: u64,
        content: &str,
        edited_at: i32,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = conn
            .execute(
                "UPDATE message SET content = ?1, updated_at = ?2 WHERE id = ?3",
                params![content, now_ms, message_id as i64],
            )
            .map_err(|e| Error::Storage(format!("update message content: {e}")))?;
        if updated == 0 {
            return Err(Error::Storage(format!(
                "edit message failed: message.id={} not found",
                message_id
            )));
        }
        let (channel_id, channel_type): (i64, i32) = conn
            .query_row(
                "SELECT channel_id, channel_type FROM message WHERE id = ?1",
                params![message_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::Storage(format!("query message for edit: {e}")))?;
        conn.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, content_edit, edited_at, extra_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(message_id) DO UPDATE SET
                content_edit=excluded.content_edit,
                edited_at=excluded.edited_at,
                extra_version=excluded.extra_version",
            params![
                message_id as i64,
                channel_id,
                channel_type,
                content,
                edited_at,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert message_extra edit: {e}")))?;
        Ok(())
    }

    pub fn set_message_pinned(&self, uid: &str, message_id: u64, is_pinned: bool) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let (channel_id, channel_type): (i64, i32) = conn
            .query_row(
                "SELECT channel_id, channel_type FROM message WHERE id = ?1",
                params![message_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| Error::Storage(format!("query message for pin: {e}")))?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, is_pinned, extra_version
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(message_id) DO UPDATE SET
                is_pinned=excluded.is_pinned,
                extra_version=excluded.extra_version",
            params![
                message_id as i64,
                channel_id,
                channel_type,
                if is_pinned { 1 } else { 0 },
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("set message pinned: {e}")))?;
        Ok(())
    }

    pub fn get_message_extra(
        &self,
        uid: &str,
        message_id: u64,
    ) -> Result<Option<StoredMessageExtra>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT message_id, channel_id, channel_type, readed, readed_count, unread_count, revoke,
                    revoker, extra_version, is_mutual_deleted, content_edit, edited_at, need_upload, is_pinned
             FROM message_extra WHERE message_id = ?1",
            params![message_id as i64],
            |row| {
                Ok(StoredMessageExtra {
                    message_id: row.get::<_, i64>(0)? as u64,
                    channel_id: row.get::<_, i64>(1)? as u64,
                    channel_type: row.get::<_, i32>(2)?,
                    readed: row.get::<_, i32>(3)?,
                    readed_count: row.get::<_, i32>(4)?,
                    unread_count: row.get::<_, i32>(5)?,
                    revoke: row.get::<_, i16>(6)? != 0,
                    revoker: row.get::<_, Option<i64>>(7)?.map(|v| v as u64),
                    extra_version: row.get::<_, i64>(8)?,
                    is_mutual_deleted: row.get::<_, i16>(9)? != 0,
                    content_edit: row.get::<_, Option<String>>(10)?,
                    edited_at: row.get::<_, i32>(11)?,
                    need_upload: row.get::<_, i16>(12)? != 0,
                    is_pinned: row.get::<_, i32>(13)? != 0,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get message extra: {e}")))
    }

    pub fn mark_channel_read(&self, uid: &str, channel_id: u64, channel_type: i32) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "UPDATE message_extra
             SET readed = 1, unread_count = 0, readed_count = 1, extra_version = ?3
             WHERE channel_id = ?1 AND channel_type = ?2",
            params![channel_id as i64, channel_type, now_ms],
        )
        .map_err(|e| Error::Storage(format!("mark channel read message_extra: {e}")))?;
        conn.execute(
            "INSERT INTO channel (channel_id, channel_type, unread_count, updated_at, created_at)
             VALUES (?1, ?2, 0, ?3, ?3)
             ON CONFLICT(channel_id) DO UPDATE SET
                unread_count=0,
                updated_at=excluded.updated_at",
            params![channel_id as i64, channel_type, now_ms],
        )
        .map_err(|e| Error::Storage(format!("mark channel read channel table: {e}")))?;
        Ok(())
    }

    pub fn get_channel_unread_count(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        let conn = self.conn_for_user(uid)?;
        let from_channel = conn
            .query_row(
                "SELECT unread_count FROM channel WHERE channel_id = ?1 LIMIT 1",
                params![channel_id as i64],
                |r| r.get::<_, i32>(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query channel unread_count: {e}")))?;
        if let Some(v) = from_channel {
            return Ok(v);
        }
        let from_extra: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(unread_count), 0)
                 FROM message_extra
                 WHERE channel_id = ?1 AND channel_type = ?2",
                params![channel_id as i64, channel_type],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("query message_extra unread_count: {e}")))?;
        Ok(from_extra as i32)
    }

    pub fn get_total_unread_count(&self, uid: &str, exclude_muted: bool) -> Result<i32> {
        let conn = self.conn_for_user(uid)?;
        let sql = if exclude_muted {
            "SELECT COALESCE(SUM(unread_count), 0) FROM channel WHERE mute = 0"
        } else {
            "SELECT COALESCE(SUM(unread_count), 0) FROM channel"
        };
        let total: i64 = conn
            .query_row(sql, [], |r| r.get(0))
            .map_err(|e| Error::Storage(format!("get total unread count: {e}")))?;
        Ok(total as i32)
    }

    pub fn save_current_uid(&self, uid: &str) -> Result<()> {
        std::fs::write(self.current_user_file(), uid.as_bytes())
            .map_err(|e| Error::Storage(format!("write current uid: {e}")))?;
        Ok(())
    }

    pub fn load_current_uid(&self) -> Result<Option<String>> {
        let path = self.current_user_file();
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::Storage(format!("read current uid: {e}")))?;
        let uid = raw.trim();
        if uid.is_empty() {
            Ok(None)
        } else {
            Ok(Some(uid.to_string()))
        }
    }

    pub fn clear_current_uid(&self) -> Result<()> {
        let path = self.current_user_file();
        if path.exists() {
            std::fs::remove_file(path)
                .map_err(|e| Error::Storage(format!("clear current uid: {e}")))?;
        }
        Ok(())
    }

    fn derive_encryption_key(uid: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"privchat_sdk_encryption_key_v1");
        hasher.update(uid.as_bytes());
        let result = hasher.finalize();
        hex::encode(result)
    }

    pub fn kv_put(&self, uid: &str, key: &str, value: &[u8]) -> Result<()> {
        let paths = self.ensure_user_storage(uid)?;
        let db =
            sled::open(paths.kv_path).map_err(|e| Error::Storage(format!("open sled kv: {e}")))?;
        db.insert(key.as_bytes(), value)
            .map_err(|e| Error::Storage(format!("sled kv put: {e}")))?;
        db.flush()
            .map_err(|e| Error::Storage(format!("sled kv flush: {e}")))?;
        Ok(())
    }

    pub fn kv_get(&self, uid: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let paths = self.ensure_user_storage(uid)?;
        let db =
            sled::open(paths.kv_path).map_err(|e| Error::Storage(format!("open sled kv: {e}")))?;
        let v = db
            .get(key.as_bytes())
            .map_err(|e| Error::Storage(format!("sled kv get: {e}")))?;
        Ok(v.map(|x| x.to_vec()))
    }

    fn open_db(path: &Path) -> Result<sled::Db> {
        sled::open(path)
            .map_err(|e| Error::Storage(format!("open sled db {}: {e}", path.display())))
    }

    pub fn normal_queue_push(&self, uid: &str, message_id: u64, payload: &[u8]) -> Result<u64> {
        let paths = self.ensure_user_storage(uid)?;
        let db = Self::open_db(&paths.normal_queue_path)?;
        let previous = db
            .insert(message_id.to_be_bytes(), payload)
            .map_err(|e| Error::Storage(format!("normal queue push: {e}")))?;
        if previous.is_some() {
            return Err(Error::Storage(format!(
                "normal queue duplicate message_id: {message_id}"
            )));
        }
        db.flush()
            .map_err(|e| Error::Storage(format!("normal queue flush: {e}")))?;
        Ok(message_id)
    }

    pub fn normal_queue_peek(&self, uid: &str, limit: usize) -> Result<Vec<(u64, Vec<u8>)>> {
        let paths = self.ensure_user_storage(uid)?;
        let db = Self::open_db(&paths.normal_queue_path)?;
        let mut out = Vec::new();
        for row in db.iter().take(limit) {
            let (k, v) = row.map_err(|e| Error::Storage(format!("normal queue iter: {e}")))?;
            if let Ok(arr) = <[u8; 8]>::try_from(k.as_ref()) {
                out.push((u64::from_be_bytes(arr), v.to_vec()));
            }
        }
        Ok(out)
    }

    pub fn normal_queue_ack(&self, uid: &str, message_ids: &[u64]) -> Result<usize> {
        let paths = self.ensure_user_storage(uid)?;
        let db = Self::open_db(&paths.normal_queue_path)?;
        let mut removed = 0usize;
        for message_id in message_ids {
            let gone = db
                .remove(message_id.to_be_bytes())
                .map_err(|e| Error::Storage(format!("normal queue ack: {e}")))?;
            if gone.is_some() {
                removed += 1;
            }
        }
        db.flush()
            .map_err(|e| Error::Storage(format!("normal queue flush: {e}")))?;
        Ok(removed)
    }

    pub fn file_queue_count(&self, uid: &str) -> Result<usize> {
        let paths = self.ensure_user_storage(uid)?;
        Ok(paths.file_queue_paths.len())
    }

    pub fn select_file_queue(&self, uid: &str, route_key: &str) -> Result<usize> {
        let n = self.file_queue_count(uid)?;
        if n == 0 {
            return Err(Error::Storage("no file queue configured".to_string()));
        }
        let mut hash: usize = 0;
        for b in route_key.as_bytes() {
            hash = hash.wrapping_mul(16777619) ^ (*b as usize);
        }
        Ok(hash % n)
    }

    pub fn file_queue_push(
        &self,
        uid: &str,
        queue_index: usize,
        message_id: u64,
        payload: &[u8],
    ) -> Result<u64> {
        let paths = self.ensure_user_storage(uid)?;
        let path = paths
            .file_queue_paths
            .get(queue_index)
            .ok_or_else(|| Error::Storage(format!("invalid file queue index: {queue_index}")))?;
        let db = Self::open_db(path)?;
        let previous = db
            .insert(message_id.to_be_bytes(), payload)
            .map_err(|e| Error::Storage(format!("file queue push: {e}")))?;
        if previous.is_some() {
            return Err(Error::Storage(format!(
                "file queue duplicate message_id: {message_id}"
            )));
        }
        db.flush()
            .map_err(|e| Error::Storage(format!("file queue flush: {e}")))?;
        Ok(message_id)
    }

    pub fn file_queue_peek(
        &self,
        uid: &str,
        queue_index: usize,
        limit: usize,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        let paths = self.ensure_user_storage(uid)?;
        let path = paths
            .file_queue_paths
            .get(queue_index)
            .ok_or_else(|| Error::Storage(format!("invalid file queue index: {queue_index}")))?;
        let db = Self::open_db(path)?;
        let mut out = Vec::new();
        for row in db.iter().take(limit) {
            let (k, v) = row.map_err(|e| Error::Storage(format!("file queue iter: {e}")))?;
            if let Ok(arr) = <[u8; 8]>::try_from(k.as_ref()) {
                out.push((u64::from_be_bytes(arr), v.to_vec()));
            }
        }
        Ok(out)
    }

    pub fn file_queue_ack(
        &self,
        uid: &str,
        queue_index: usize,
        message_ids: &[u64],
    ) -> Result<usize> {
        let paths = self.ensure_user_storage(uid)?;
        let path = paths
            .file_queue_paths
            .get(queue_index)
            .ok_or_else(|| Error::Storage(format!("invalid file queue index: {queue_index}")))?;
        let db = Self::open_db(path)?;
        let mut removed = 0usize;
        for message_id in message_ids {
            let gone = db
                .remove(message_id.to_be_bytes())
                .map_err(|e| Error::Storage(format!("file queue ack: {e}")))?;
            if gone.is_some() {
                removed += 1;
            }
        }
        db.flush()
            .map_err(|e| Error::Storage(format!("file queue flush: {e}")))?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::LocalStore;
    use crate::{NewMessage, UpsertChannelExtraInput, UpsertChannelInput};
    use rusqlite::params;
    use std::path::PathBuf;

    fn test_store() -> LocalStore {
        let dir = PathBuf::from(format!(
            "/tmp/privchat-rust-test-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        LocalStore::open_at(dir).expect("open test store")
    }

    #[test]
    fn normal_queue_roundtrip() {
        let store = test_store();
        let uid = "10001";
        let a = store.normal_queue_push(uid, 101, b"a").expect("push a");
        let b = store.normal_queue_push(uid, 102, b"b").expect("push b");
        assert!(a < b);
        let peek = store.normal_queue_peek(uid, 10).expect("peek");
        assert_eq!(peek.len(), 2);
        assert_eq!(peek[0].0, a);
        assert_eq!(peek[1].0, b);
        assert_eq!(peek[0].1, b"a");
        assert_eq!(peek[1].1, b"b");
        let removed = store.normal_queue_ack(uid, &[a]).expect("ack");
        assert_eq!(removed, 1);
        let left = store.normal_queue_peek(uid, 10).expect("peek left");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, b);
    }

    #[test]
    fn file_queue_roundtrip() {
        let store = test_store();
        let uid = "10002";
        let qidx = store
            .select_file_queue(uid, "image:file.jpg")
            .expect("select queue");
        let message_id = store
            .file_queue_push(uid, qidx, 201, b"payload")
            .expect("file push");
        let peek = store.file_queue_peek(uid, qidx, 10).expect("file peek");
        assert_eq!(peek.len(), 1);
        assert_eq!(peek[0].0, message_id);
        assert_eq!(peek[0].1, b"payload");
        let removed = store
            .file_queue_ack(uid, qidx, &[message_id])
            .expect("file ack");
        assert_eq!(removed, 1);
        let left = store
            .file_queue_peek(uid, qidx, 10)
            .expect("file peek left");
        assert!(left.is_empty());
    }

    #[test]
    fn message_lifecycle_uses_message_id_pk() {
        let store = test_store();
        let uid = "10003";
        let input = NewMessage {
            channel_id: 100,
            channel_type: 1,
            from_uid: 200,
            message_type: 1,
            content: "hello".to_string(),
            searchable_word: "hello".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        };

        let message_id = store
            .create_local_message(uid, &input)
            .expect("create local message");
        let msg = store
            .get_message_by_id(uid, message_id)
            .expect("load message")
            .expect("message exists");
        assert_eq!(msg.message_id, message_id);
        assert_eq!(msg.server_message_id, None);
        assert_eq!(msg.channel_id, 100);
        assert_eq!(msg.content, "hello");

        store
            .mark_message_sent(uid, message_id, 900001)
            .expect("mark sent");
        let sent = store
            .get_message_by_id(uid, message_id)
            .expect("load sent")
            .expect("sent exists");
        assert_eq!(sent.message_id, message_id);
        assert_eq!(sent.server_message_id, Some(900001));
    }

    #[test]
    fn list_messages_by_channel() {
        let store = test_store();
        let uid = "10005";
        let mut input = NewMessage {
            channel_id: 100,
            channel_type: 1,
            from_uid: 200,
            message_type: 1,
            content: "hello-1".to_string(),
            searchable_word: "hello".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        };
        let m1 = store
            .create_local_message(uid, &input)
            .expect("create message 1");
        input.content = "hello-2".to_string();
        let m2 = store
            .create_local_message(uid, &input)
            .expect("create message 2");
        input.channel_id = 101;
        input.content = "other-channel".to_string();
        let _m3 = store
            .create_local_message(uid, &input)
            .expect("create message 3");

        let page = store
            .list_messages(uid, 100, 1, 20, 0)
            .expect("list messages");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].message_id, m2);
        assert_eq!(page[1].message_id, m1);
    }

    #[test]
    fn unread_count_lifecycle() {
        let store = test_store();
        let uid = "10006";
        let channel_id = 500_u64;
        let channel_type = 1_i32;
        let input = NewMessage {
            channel_id,
            channel_type,
            from_uid: 200,
            message_type: 1,
            content: "hello-unread".to_string(),
            searchable_word: "hello-unread".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        };
        let message_id = store
            .create_local_message(uid, &input)
            .expect("create message");

        store
            .set_message_read(uid, message_id, channel_id, channel_type, false)
            .expect("set unread");
        let unread1 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread");
        assert_eq!(unread1, 1);

        store
            .set_message_read(uid, message_id, channel_id, channel_type, true)
            .expect("set read");
        let unread2 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread2");
        assert_eq!(unread2, 0);

        store
            .set_message_read(uid, message_id, channel_id, channel_type, false)
            .expect("set unread again");
        let unread3 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread3");
        assert_eq!(unread3, 1);

        store
            .mark_channel_read(uid, channel_id, channel_type)
            .expect("mark channel read");
        let unread4 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread4");
        assert_eq!(unread4, 0);

        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 501,
                    channel_type: 1,
                    channel_name: "c-501".to_string(),
                    channel_remark: "".to_string(),
                    avatar: "".to_string(),
                    unread_count: 5,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 0,
                    last_local_message_id: 0,
                    last_msg_content: "".to_string(),
                },
            )
            .expect("upsert channel 501");
        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 502,
                    channel_type: 1,
                    channel_name: "c-502".to_string(),
                    channel_remark: "".to_string(),
                    avatar: "".to_string(),
                    unread_count: 8,
                    top: 0,
                    mute: 1,
                    last_msg_timestamp: 0,
                    last_local_message_id: 0,
                    last_msg_content: "".to_string(),
                },
            )
            .expect("upsert channel 502");
        let total_all = store
            .get_total_unread_count(uid, false)
            .expect("get total unread all");
        let total_no_mute = store
            .get_total_unread_count(uid, true)
            .expect("get total unread exclude muted");
        assert_eq!(total_all, 13);
        assert_eq!(total_no_mute, 5);
    }

    #[test]
    fn upsert_and_list_channels() {
        let store = test_store();
        let uid = "10007";
        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 9001,
                    channel_type: 2,
                    channel_name: "group-9001".to_string(),
                    channel_remark: "remark-9001".to_string(),
                    avatar: "https://example/avatar.png".to_string(),
                    unread_count: 3,
                    top: 1,
                    mute: 0,
                    last_msg_timestamp: 123456789,
                    last_local_message_id: 42,
                    last_msg_content: "last-msg".to_string(),
                },
            )
            .expect("upsert channel");

        let row = store
            .get_channel_by_id(uid, 9001)
            .expect("get channel")
            .expect("channel exists");
        assert_eq!(row.channel_id, 9001);
        assert_eq!(row.channel_type, 2);
        assert_eq!(row.channel_name, "group-9001");
        assert_eq!(row.unread_count, 3);

        let page = store.list_channels(uid, 20, 0).expect("list channels");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].channel_id, 9001);
    }

    #[test]
    fn upsert_and_get_channel_extra() {
        let store = test_store();
        let uid = "10008";
        store
            .upsert_channel_extra(
                uid,
                &UpsertChannelExtraInput {
                    channel_id: 9100,
                    channel_type: 1,
                    browse_to: 123,
                    keep_pts: 456,
                    keep_offset_y: 20,
                    draft: "hello draft".to_string(),
                    draft_updated_at: 9999,
                },
            )
            .expect("upsert channel_extra");

        let row = store
            .get_channel_extra(uid, 9100, 1)
            .expect("get channel_extra")
            .expect("channel_extra exists");
        assert_eq!(row.channel_id, 9100);
        assert_eq!(row.browse_to, 123);
        assert_eq!(row.keep_pts, 456);
        assert_eq!(row.keep_offset_y, 20);
        assert_eq!(row.draft, "hello draft");
        assert_eq!(row.draft_updated_at, 9999);
    }

    #[test]
    fn upsert_and_list_user_friend_group_entities() {
        let store = test_store();
        let uid = "10009";

        store
            .upsert_user(
                uid,
                &crate::UpsertUserInput {
                    user_id: 20001,
                    username: Some("alice".to_string()),
                    nickname: Some("Alice".to_string()),
                    alias: Some("A".to_string()),
                    avatar: "avatar://alice".to_string(),
                    user_type: 0,
                    is_deleted: false,
                    channel_id: "c-alice".to_string(),
                    updated_at: 1000,
                },
            )
            .expect("upsert user");
        let user = store
            .get_user_by_id(uid, 20001)
            .expect("get user")
            .expect("user exists");
        assert_eq!(user.username.as_deref(), Some("alice"));

        store
            .upsert_friend(
                uid,
                &crate::UpsertFriendInput {
                    user_id: 20001,
                    tags: Some("work".to_string()),
                    is_pinned: true,
                    created_at: 1000,
                    updated_at: 1001,
                },
            )
            .expect("upsert friend");
        let friends = store.list_friends(uid, 20, 0).expect("list friends");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].user_id, 20001);
        assert!(friends[0].is_pinned);
        store.delete_friend(uid, 20001).expect("delete friend");
        let friends = store
            .list_friends(uid, 20, 0)
            .expect("list friends after delete");
        assert!(friends.is_empty());
        store
            .upsert_blacklist_entry(
                uid,
                &crate::UpsertBlacklistInput {
                    blocked_user_id: 20001,
                    created_at: 1003,
                    updated_at: 1004,
                },
            )
            .expect("upsert blacklist entry");
        let blacklist = store
            .list_blacklist_entries(uid, 20, 0)
            .expect("list blacklist entries");
        assert_eq!(blacklist.len(), 1);
        assert_eq!(blacklist[0].blocked_user_id, 20001);
        store
            .delete_blacklist_entry(uid, 20001)
            .expect("delete blacklist entry");
        let blacklist = store
            .list_blacklist_entries(uid, 20, 0)
            .expect("list blacklist entries after delete");
        assert!(blacklist.is_empty());

        store
            .upsert_group(
                uid,
                &crate::UpsertGroupInput {
                    group_id: 30001,
                    name: Some("group-a".to_string()),
                    avatar: "avatar://group-a".to_string(),
                    owner_id: Some(20001),
                    is_dismissed: false,
                    created_at: 1000,
                    updated_at: 1002,
                },
            )
            .expect("upsert group");
        let group = store
            .get_group_by_id(uid, 30001)
            .expect("get group")
            .expect("group exists");
        assert_eq!(group.name.as_deref(), Some("group-a"));

        store
            .upsert_group_member(
                uid,
                &crate::UpsertGroupMemberInput {
                    group_id: 30001,
                    user_id: 20001,
                    role: 0,
                    status: 0,
                    alias: Some("owner".to_string()),
                    is_muted: false,
                    joined_at: 1003,
                    updated_at: 1004,
                },
            )
            .expect("upsert group member");
        let members = store
            .list_group_members(uid, 30001, 20, 0)
            .expect("list group members");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].user_id, 20001);
        store
            .delete_group_member(uid, 30001, 20001)
            .expect("delete group member");
        let members = store
            .list_group_members(uid, 30001, 20, 0)
            .expect("list group members after delete");
        assert!(members.is_empty());

        store
            .upsert_channel_member(
                uid,
                &crate::UpsertChannelMemberInput {
                    channel_id: 30001,
                    channel_type: 2,
                    member_uid: 20001,
                    member_name: "alice".to_string(),
                    member_remark: "owner".to_string(),
                    member_avatar: "avatar://alice".to_string(),
                    member_invite_uid: 20001,
                    role: 1,
                    status: 1,
                    is_deleted: false,
                    robot: 0,
                    version: 1,
                    created_at: 1005,
                    updated_at: 1006,
                    extra: "{}".to_string(),
                    forbidden_expiration_time: 0,
                    member_avatar_cache_key: "cache-alice".to_string(),
                },
            )
            .expect("upsert channel member");
        let ch_members = store
            .list_channel_members(uid, 30001, 2, 20, 0)
            .expect("list channel members");
        assert_eq!(ch_members.len(), 1);
        assert_eq!(ch_members[0].member_uid, 20001);
        store
            .delete_channel_member(uid, 30001, 2, 20001)
            .expect("delete channel member");
        let ch_members_after = store
            .list_channel_members(uid, 30001, 2, 20, 0)
            .expect("list channel members after delete");
        assert!(ch_members_after.is_empty());
    }

    #[test]
    fn reaction_mention_reminder_lifecycle() {
        let store = test_store();
        let uid = "10010";

        store
            .upsert_message_reaction(
                uid,
                &crate::UpsertMessageReactionInput {
                    channel_id: 500,
                    channel_type: 1,
                    uid: 20001,
                    name: "alice".to_string(),
                    emoji: "👍".to_string(),
                    message_id: 70001,
                    seq: 1,
                    is_deleted: false,
                    created_at: 12345,
                },
            )
            .expect("upsert reaction");
        let reactions = store
            .list_message_reactions(uid, 70001, 20, 0)
            .expect("list reactions");
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0].emoji, "👍");

        let mention_id = store
            .record_mention(
                uid,
                &crate::MentionInput {
                    message_id: 70001,
                    channel_id: 500,
                    channel_type: 1,
                    mentioned_user_id: 20001,
                    sender_id: 20002,
                    is_mention_all: false,
                    created_at: 20000,
                },
            )
            .expect("record mention");
        assert!(mention_id > 0);
        let unread = store
            .get_unread_mention_count(uid, 500, 1, 20001)
            .expect("get unread mention");
        assert_eq!(unread, 1);
        let ids = store
            .list_unread_mention_message_ids(uid, 500, 1, 20001, 10)
            .expect("list unread mention ids");
        assert_eq!(ids, vec![70001]);
        store
            .mark_mention_read(uid, 70001, 20001)
            .expect("mark mention read");
        let unread_after = store
            .get_unread_mention_count(uid, 500, 1, 20001)
            .expect("get unread mention after");
        assert_eq!(unread_after, 0);

        store
            .upsert_reminder(
                uid,
                &crate::UpsertReminderInput {
                    reminder_id: 80001,
                    message_id: 70001,
                    pts: 100,
                    channel_id: 500,
                    channel_type: 1,
                    uid: 20001,
                    reminder_type: 1,
                    text: "todo".to_string(),
                    data: "{}".to_string(),
                    is_locate: false,
                    version: 1,
                    done: false,
                    need_upload: true,
                    publisher: Some(20002),
                },
            )
            .expect("upsert reminder");
        let reminders = store
            .list_pending_reminders(uid, 20001, 20, 0)
            .expect("list pending reminders");
        assert_eq!(reminders.len(), 1);
        assert_eq!(reminders[0].reminder_id, 80001);
        store
            .mark_reminder_done(uid, 80001, true)
            .expect("mark reminder done");
        let reminders_after = store
            .list_pending_reminders(uid, 20001, 20, 0)
            .expect("list pending reminders after");
        assert!(reminders_after.is_empty());
    }

    #[test]
    fn update_local_message_id_and_status() {
        let store = test_store();
        let uid = "10004";
        let input = NewMessage {
            channel_id: 10,
            channel_type: 1,
            from_uid: 20,
            message_type: 1,
            content: "local-id".to_string(),
            searchable_word: "local-id".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        };
        let message_id = store
            .create_local_message(uid, &input)
            .expect("create local message");

        store
            .update_local_message_id(uid, message_id, 1234567890)
            .expect("update local_message_id");
        store
            .update_message_status(uid, message_id, 2)
            .expect("update status");

        let conn = store.conn_for_user(uid).expect("open conn");
        let row: (i64, i32) = conn
            .query_row(
                "SELECT local_message_id, status FROM message WHERE id = ?1",
                params![message_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query message");
        assert_eq!(row.0 as u64, 1234567890);
        assert_eq!(row.1, 2);
    }

    #[test]
    fn message_extra_revoke_edit_pin_lifecycle() {
        let store = test_store();
        let uid = "10011";
        let input = NewMessage {
            channel_id: 777,
            channel_type: 1,
            from_uid: 200,
            message_type: 1,
            content: "before-edit".to_string(),
            searchable_word: "before-edit".to_string(),
            setting: 0,
            extra: "{}".to_string(),
        };
        let message_id = store
            .create_local_message(uid, &input)
            .expect("create message");

        store
            .set_message_revoke(uid, message_id, true, Some(30001))
            .expect("set revoke");
        store
            .edit_message(uid, message_id, "after-edit", 123)
            .expect("edit message");
        store
            .set_message_pinned(uid, message_id, true)
            .expect("set pinned");

        let row = store
            .get_message_by_id(uid, message_id)
            .expect("get message")
            .expect("message exists");
        assert_eq!(row.content, "after-edit");

        let extra = store
            .get_message_extra(uid, message_id)
            .expect("get message extra")
            .expect("message extra exists");
        assert!(extra.revoke);
        assert_eq!(extra.revoker, Some(30001));
        assert_eq!(extra.content_edit.as_deref(), Some("after-edit"));
        assert_eq!(extra.edited_at, 123);
        assert!(extra.is_pinned);
    }
}
