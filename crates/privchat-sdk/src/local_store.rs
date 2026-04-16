// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hkdf::Hkdf;
use rand::RngCore;
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
    UpsertGroupMemberInput, UpsertMessageReactionInput, UpsertReminderInput,
    UpsertRemoteMessageInput, UpsertRemoteMessageResult, UpsertUserInput,
};

mod embedded {
    use super::embed_migrations;
    embed_migrations!("migrations");
}

const STORAGE_SCHEMA_VERSION: i32 = 1;
const WRAP_VERSION: i32 = 1;
const WRAP_ALG: &str = "HKDF-SHA256+AES-256-GCM";
const TOKEN_ALG: &str = "AES-256-GCM";

const GLOBAL_TREE_INSTALL: &str = "install";
const GLOBAL_TREE_ACCOUNTS: &str = "accounts";

const ACCOUNT_TREE_META: &str = "meta";
const ACCOUNT_TREE_PROFILE: &str = "profile";
const ACCOUNT_TREE_WRAP: &str = "wrap";
const ACCOUNT_TREE_AUTH: &str = "auth";
const ACCOUNT_TREE_KV: &str = "kv";

const K_SCHEMA_VERSION: &[u8] = b"schema_version";
const K_DEVICE_ID: &[u8] = b"device_id";
const K_INSTALL_SECRET: &[u8] = b"install_secret";
const K_CREATED_AT: &[u8] = b"created_at";
const K_ACTIVE_UID: &[u8] = b"active_uid";
const K_UID: &[u8] = b"uid";
const K_LAST_LOGIN_AT: &[u8] = b"last_login_at";
const K_BOOTSTRAP_COMPLETED: &[u8] = b"bootstrap_completed";
const K_WRAP_VERSION: &[u8] = b"wrap_version";
const K_WRAP_ALG: &[u8] = b"wrap_alg";
const K_USER_WRAP_ENABLED: &[u8] = b"user_wrap_enabled";
const K_USER_WRAP_VERSION: &[u8] = b"user_wrap_version";
const K_PROFILE_UPDATED_AT: &[u8] = b"profile_updated_at";
const K_MASTER_KEY_WRAPPED: &[u8] = b"master_key_wrapped";
const K_MASTER_KEY_NONCE: &[u8] = b"master_key_nonce";
const K_ACCESS_TOKEN_ALG: &[u8] = b"access_token_alg";
const K_ACCESS_TOKEN_ENC: &[u8] = b"access_token_enc";
const K_ACCESS_TOKEN_NONCE: &[u8] = b"access_token_nonce";
const K_REFRESH_TOKEN_ALG: &[u8] = b"refresh_token_alg";
const K_REFRESH_TOKEN_ENC: &[u8] = b"refresh_token_enc";
const K_REFRESH_TOKEN_NONCE: &[u8] = b"refresh_token_nonce";
const K_TOKEN_EXPIRE_AT: &[u8] = b"token_expire_at";
const K_DEVICE_ID_CURRENT: &[u8] = b"device_id";

#[derive(Debug, Clone)]
struct InstallState {
    device_id: String,
    install_secret: [u8; 32],
}

#[derive(Debug, Clone)]
struct EncryptedBlob {
    ciphertext: Vec<u8>,
    nonce: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalAccountEntry {
    pub uid: String,
    pub created_at: i64,
    pub last_login_at: i64,
}

#[cfg(unix)]
fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)
        .map_err(|e| Error::Storage(format!("create dir {}: {e}", path.display())))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| Error::Storage(format!("set dir mode 700 {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|e| Error::Storage(format!("create dir {}: {e}", path.display())))?;
    Ok(())
}

fn get_string(tree: &sled::Tree, key: &[u8]) -> Result<Option<String>> {
    let value = tree
        .get(key)
        .map_err(|e| Error::Storage(format!("read key {}: {e}", String::from_utf8_lossy(key))))?;
    match value {
        Some(bytes) => {
            let s = String::from_utf8(bytes.to_vec()).map_err(|e| {
                Error::Storage(format!(
                    "key {} is not valid utf8: {e}",
                    String::from_utf8_lossy(key)
                ))
            })?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

fn random_hex(num_bytes: usize) -> String {
    let mut buf = vec![0u8; num_bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

fn i32_to_be_bytes(v: i32) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

fn i64_to_be_bytes(v: i64) -> Vec<u8> {
    v.to_be_bytes().to_vec()
}

fn i32_from_be_slice(raw: &[u8]) -> Result<i32> {
    let arr: [u8; 4] = raw
        .try_into()
        .map_err(|_| Error::Storage("invalid i32 bytes length".to_string()))?;
    Ok(i32::from_be_bytes(arr))
}

fn derive_wrap_key(install: &InstallState, uid: &str) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(
        Some(install.device_id.as_bytes()),
        install.install_secret.as_slice(),
    );
    let mut out = [0u8; 32];
    let info = format!("privchat.wrap.master_key|v=1|uid={uid}");
    hk.expand(info.as_bytes(), &mut out)
        .map_err(|e| Error::Storage(format!("derive wrap key: {e}")))?;
    Ok(out)
}

fn wrap_aad(uid: &str) -> String {
    format!(
        "privchat|purpose=wrap_master_key|uid={uid}|wrap_version={WRAP_VERSION}|schema={STORAGE_SCHEMA_VERSION}"
    )
}

fn token_aad(uid: &str, purpose: &str) -> String {
    format!("privchat|purpose={purpose}|uid={uid}|schema={STORAGE_SCHEMA_VERSION}")
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
        let db_path = user_root.join("privchat.db");
        let kv_path = user_root.join("account.kv");
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
    account_dbs: Arc<Mutex<HashMap<String, sled::Db>>>,
    global_db: Arc<Mutex<Option<sled::Db>>>,
    sqlite_conns: Arc<Mutex<HashMap<String, Connection>>>,
}

/// A guard that returns the connection to the cache when dropped
pub struct ConnGuard<'a> {
    conn: Option<Connection>,
    uid: String,
    store: &'a LocalStore,
}

impl<'a> std::ops::Deref for ConnGuard<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().unwrap()
    }
}

impl<'a> std::ops::DerefMut for ConnGuard<'a> {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().unwrap()
    }
}

impl<'a> Drop for ConnGuard<'a> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if let Ok(mut cache) = self.store.sqlite_conns.lock() {
                cache.insert(self.uid.clone(), conn);
            }
        }
    }
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
            account_dbs: Arc::new(Mutex::new(HashMap::new())),
            global_db: Arc::new(Mutex::new(None)),
            sqlite_conns: Arc::new(Mutex::new(HashMap::new())),
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

    fn global_root(&self) -> PathBuf {
        self.base_dir().join("global")
    }

    fn global_kv_path(&self) -> PathBuf {
        self.global_root().join("global.kv")
    }

    fn open_global_db(&self) -> Result<sled::Db> {
        let mut guard = self
            .global_db
            .lock()
            .map_err(|_| Error::Storage("lock global_db failed".to_string()))?;
        if let Some(db) = guard.as_ref() {
            return Ok(db.clone());
        }
        ensure_private_dir(&self.global_root())?;
        let path = self.global_kv_path();
        ensure_private_dir(&path)?;
        let db = Self::open_db(&path)?;
        *guard = Some(db.clone());
        Ok(db)
    }

    fn open_account_db(&self, uid: &str) -> Result<sled::Db> {
        let mut guard = self
            .account_dbs
            .lock()
            .map_err(|_| Error::Storage("lock account_dbs failed".to_string()))?;
        if let Some(db) = guard.get(uid) {
            return Ok(db.clone());
        }
        let paths = self.ensure_user_storage(uid)?;
        let db = Self::open_db(&paths.kv_path)?;
        guard.insert(uid.to_string(), db.clone());
        Ok(db)
    }

    fn account_tree(&self, uid: &str, tree_name: &str) -> Result<sled::Tree> {
        let db = self.open_account_db(uid)?;
        db.open_tree(tree_name)
            .map_err(|e| Error::Storage(format!("open account tree {tree_name}: {e}")))
    }

    fn get_install_state(&self) -> Result<InstallState> {
        let db = self.open_global_db()?;
        let install = db
            .open_tree(GLOBAL_TREE_INSTALL)
            .map_err(|e| Error::Storage(format!("open install tree: {e}")))?;
        let device_id = get_string(&install, K_DEVICE_ID)?;
        let install_secret = install
            .get(K_INSTALL_SECRET)
            .map_err(|e| Error::Storage(format!("read install secret: {e}")))?;
        if let (Some(device_id), Some(secret)) = (device_id, install_secret) {
            let install_secret: [u8; 32] = secret.as_ref().try_into().map_err(|_| {
                Error::Storage("invalid install secret length, expect 32 bytes".to_string())
            })?;
            return Ok(InstallState {
                device_id,
                install_secret,
            });
        }

        let now = chrono::Utc::now().timestamp_millis();
        let device_id = random_hex(16);
        let mut install_secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut install_secret);
        install
            .insert(K_SCHEMA_VERSION, i32_to_be_bytes(STORAGE_SCHEMA_VERSION))
            .map_err(|e| Error::Storage(format!("save install schema: {e}")))?;
        install
            .insert(K_DEVICE_ID, device_id.as_bytes())
            .map_err(|e| Error::Storage(format!("save install device_id: {e}")))?;
        install
            .insert(K_INSTALL_SECRET, install_secret.as_slice())
            .map_err(|e| Error::Storage(format!("save install secret: {e}")))?;
        install
            .insert(K_CREATED_AT, i64_to_be_bytes(now))
            .map_err(|e| Error::Storage(format!("save install created_at: {e}")))?;
        Ok(InstallState {
            device_id,
            install_secret,
        })
    }

    pub fn storage_paths(&self, uid: &str) -> StoragePaths {
        StoragePaths::for_user(self.base_dir(), uid)
    }

    pub fn ensure_user_storage(&self, uid: &str) -> Result<StoragePaths> {
        let paths = self.storage_paths(uid);
        std::fs::create_dir_all(&paths.user_root)
            .map_err(|e| Error::Storage(format!("create user root: {e}")))?;
        let legacy_db_path = paths.user_root.join("messages.db");
        if legacy_db_path.exists() && !paths.db_path.exists() {
            std::fs::rename(&legacy_db_path, &paths.db_path)
                .map_err(|e| Error::Storage(format!("migrate db path: {e}")))?;
            let legacy_wal = paths.user_root.join("messages.db-wal");
            let legacy_shm = paths.user_root.join("messages.db-shm");
            let new_wal = paths.user_root.join("privchat.db-wal");
            let new_shm = paths.user_root.join("privchat.db-shm");
            if legacy_wal.exists() {
                let _ = std::fs::rename(&legacy_wal, &new_wal);
            }
            if legacy_shm.exists() {
                let _ = std::fs::rename(&legacy_shm, &new_shm);
            }
        }
        let legacy_kv_path = paths.user_root.join("kv");
        if legacy_kv_path.exists() && !paths.kv_path.exists() {
            std::fs::rename(&legacy_kv_path, &paths.kv_path)
                .map_err(|e| Error::Storage(format!("migrate kv path: {e}")))?;
        }
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

    fn conn_for_user(&self, uid: &str) -> Result<ConnGuard<'_>> {
        // Try to take a cached connection first
        let cached = self
            .sqlite_conns
            .lock()
            .ok()
            .and_then(|mut cache| cache.remove(uid));
        let conn = if let Some(c) = cached {
            c
        } else {
            // No cached connection, open a new one
            let paths = self.ensure_user_storage(uid)?;
            let c = Connection::open(paths.db_path)
                .map_err(|e| Error::Storage(format!("open db: {e}")))?;
            let key = Self::derive_encryption_key(uid);
            c.pragma_update(None, "key", &key)
                .map_err(|e| Error::Storage(format!("set db key: {e}")))?;
            c
        };
        Ok(ConnGuard {
            conn: Some(conn),
            uid: uid.to_string(),
            store: self,
        })
    }

    pub fn save_login(&self, uid: &str, login: &LoginResult) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let bootstrap_completed = self.load_bootstrap_completed(uid)?;
        let install = self.get_install_state()?;
        let master_key = self.get_or_create_master_key(uid, &install)?;
        let access_blob =
            self.encrypt_user_blob(uid, "access_token", &master_key, login.token.as_bytes())?;
        let refresh_blob = match login.refresh_token.as_deref() {
            Some(token) => {
                Some(self.encrypt_user_blob(uid, "refresh_token", &master_key, token.as_bytes())?)
            }
            None => None,
        };
        let account_db = self.open_account_db(uid)?;
        let meta = account_db
            .open_tree(ACCOUNT_TREE_META)
            .map_err(|e| Error::Storage(format!("open meta tree: {e}")))?;
        let profile = account_db
            .open_tree(ACCOUNT_TREE_PROFILE)
            .map_err(|e| Error::Storage(format!("open profile tree: {e}")))?;
        let auth = account_db
            .open_tree(ACCOUNT_TREE_AUTH)
            .map_err(|e| Error::Storage(format!("open auth tree: {e}")))?;

        meta.insert(K_SCHEMA_VERSION, i32_to_be_bytes(STORAGE_SCHEMA_VERSION))
            .map_err(|e| Error::Storage(format!("save schema version: {e}")))?;
        meta.insert(K_WRAP_VERSION, i32_to_be_bytes(WRAP_VERSION))
            .map_err(|e| Error::Storage(format!("save wrap version: {e}")))?;
        meta.insert(K_WRAP_ALG, WRAP_ALG.as_bytes())
            .map_err(|e| Error::Storage(format!("save wrap alg: {e}")))?;
        meta.insert(K_LAST_LOGIN_AT, i64_to_be_bytes(now))
            .map_err(|e| Error::Storage(format!("save last_login_at: {e}")))?;
        meta.insert(
            K_BOOTSTRAP_COMPLETED,
            i32_to_be_bytes(if bootstrap_completed { 1 } else { 0 }),
        )
        .map_err(|e| Error::Storage(format!("save bootstrap completed: {e}")))?;
        meta.insert(K_USER_WRAP_ENABLED, i32_to_be_bytes(0))
            .map_err(|e| Error::Storage(format!("save user_wrap_enabled: {e}")))?;
        meta.insert(K_USER_WRAP_VERSION, i32_to_be_bytes(0))
            .map_err(|e| Error::Storage(format!("save user_wrap_version: {e}")))?;

        profile
            .insert(K_UID, uid.as_bytes())
            .map_err(|e| Error::Storage(format!("save profile uid: {e}")))?;
        profile
            .insert(K_PROFILE_UPDATED_AT, i64_to_be_bytes(now))
            .map_err(|e| Error::Storage(format!("save profile_updated_at: {e}")))?;

        auth.insert(K_ACCESS_TOKEN_ALG, TOKEN_ALG.as_bytes())
            .map_err(|e| Error::Storage(format!("save access token alg: {e}")))?;
        auth.insert(K_ACCESS_TOKEN_ENC, access_blob.ciphertext)
            .map_err(|e| Error::Storage(format!("save access token enc: {e}")))?;
        auth.insert(K_ACCESS_TOKEN_NONCE, access_blob.nonce)
            .map_err(|e| Error::Storage(format!("save access token nonce: {e}")))?;
        auth.insert(K_DEVICE_ID_CURRENT, login.device_id.as_bytes())
            .map_err(|e| Error::Storage(format!("save device_id: {e}")))?;
        if let Some(refresh_blob) = refresh_blob {
            auth.insert(K_REFRESH_TOKEN_ALG, TOKEN_ALG.as_bytes())
                .map_err(|e| Error::Storage(format!("save refresh token alg: {e}")))?;
            auth.insert(K_REFRESH_TOKEN_ENC, refresh_blob.ciphertext)
                .map_err(|e| Error::Storage(format!("save refresh token enc: {e}")))?;
            auth.insert(K_REFRESH_TOKEN_NONCE, refresh_blob.nonce)
                .map_err(|e| Error::Storage(format!("save refresh token nonce: {e}")))?;
        } else {
            let _ = auth.remove(K_REFRESH_TOKEN_ALG);
            let _ = auth.remove(K_REFRESH_TOKEN_ENC);
            let _ = auth.remove(K_REFRESH_TOKEN_NONCE);
        }
        auth.insert(
            K_TOKEN_EXPIRE_AT,
            i64_to_be_bytes(i64::try_from(login.expires_at).unwrap_or(i64::MAX)),
        )
            .map_err(|e| Error::Storage(format!("save token expire_at: {e}")))?;
        drop(auth);
        drop(profile);
        drop(meta);
        drop(account_db);

        let global_db = self.open_global_db()?;
        let accounts = global_db
            .open_tree(GLOBAL_TREE_ACCOUNTS)
            .map_err(|e| Error::Storage(format!("open accounts tree: {e}")))?;
        accounts
            .insert(K_SCHEMA_VERSION, i32_to_be_bytes(STORAGE_SCHEMA_VERSION))
            .map_err(|e| Error::Storage(format!("save accounts schema: {e}")))?;
        accounts
            .insert(K_ACTIVE_UID, uid.as_bytes())
            .map_err(|e| Error::Storage(format!("save active uid: {e}")))?;
        let created_key = Self::k_acct_created_at(uid);
        if accounts
            .get(created_key.as_bytes())
            .map_err(|e| Error::Storage(format!("load account created_at: {e}")))?
            .is_none()
        {
            accounts
                .insert(created_key.as_bytes(), i64_to_be_bytes(now))
                .map_err(|e| Error::Storage(format!("save account created_at: {e}")))?;
        }
        let last_login_key = Self::k_acct_last_login(uid);
        accounts
            .insert(last_login_key.as_bytes(), i64_to_be_bytes(now))
            .map_err(|e| Error::Storage(format!("save account last_login_at: {e}")))?;

        self.clear_legacy_session(uid)?;
        self.save_current_uid(uid)?;
        Ok(())
    }

    pub fn set_bootstrap_completed(&self, uid: &str, completed: bool) -> Result<()> {
        let meta = self.account_tree(uid, ACCOUNT_TREE_META)?;
        meta.insert(
            K_BOOTSTRAP_COMPLETED,
            i32_to_be_bytes(if completed { 1 } else { 0 }),
        )
        .map_err(|e| Error::Storage(format!("set bootstrap completed: {e}")))?;
        Ok(())
    }

    pub fn load_session(&self, uid: &str) -> Result<Option<SessionSnapshot>> {
        if let Some(snapshot) = self.load_session_from_account(uid)? {
            return Ok(Some(snapshot));
        }
        let Some(legacy) = self.load_legacy_session(uid)? else {
            return Ok(None);
        };
        let login = LoginResult {
            user_id: legacy.user_id,
            token: legacy.token.clone(),
            device_id: legacy.device_id.clone(),
            refresh_token: None,
            expires_at: 0,
        };
        self.save_login(uid, &login)?;
        self.set_bootstrap_completed(uid, legacy.bootstrap_completed)?;
        Ok(Some(legacy))
    }

    pub fn clear_session(&self, uid: &str) -> Result<()> {
        let auth = self.account_tree(uid, ACCOUNT_TREE_AUTH)?;
        auth.remove(K_ACCESS_TOKEN_ENC)
            .map_err(|e| Error::Storage(format!("clear access token: {e}")))?;
        auth.remove(K_ACCESS_TOKEN_NONCE)
            .map_err(|e| Error::Storage(format!("clear access token nonce: {e}")))?;
        auth.remove(K_REFRESH_TOKEN_ENC)
            .map_err(|e| Error::Storage(format!("clear refresh token: {e}")))?;
        auth.remove(K_REFRESH_TOKEN_NONCE)
            .map_err(|e| Error::Storage(format!("clear refresh token nonce: {e}")))?;
        auth.remove(K_REFRESH_TOKEN_ALG)
            .map_err(|e| Error::Storage(format!("clear refresh token alg: {e}")))?;
        auth.remove(K_DEVICE_ID_CURRENT)
            .map_err(|e| Error::Storage(format!("clear device_id: {e}")))?;
        auth.remove(K_TOKEN_EXPIRE_AT)
            .map_err(|e| Error::Storage(format!("clear token expire_at: {e}")))?;
        // 清除 active_uid，防止重启后 listLocalAccounts 仍返回 isActive=true
        let global_db = self.open_global_db()?;
        let accounts = global_db
            .open_tree(GLOBAL_TREE_ACCOUNTS)
            .map_err(|e| Error::Storage(format!("open accounts tree: {e}")))?;
        if let Some(active) = get_string(&accounts, K_ACTIVE_UID)? {
            if active == uid {
                accounts
                    .remove(K_ACTIVE_UID)
                    .map_err(|e| Error::Storage(format!("clear active uid: {e}")))?;
                global_db
                    .flush()
                    .map_err(|e| Error::Storage(format!("flush global db: {e}")))?;
            }
        }
        self.clear_legacy_session(uid)?;
        Ok(())
    }

    fn k_acct_last_login(uid: &str) -> String {
        format!("acct/{uid}/last_login_at")
    }

    fn k_acct_created_at(uid: &str) -> String {
        format!("acct/{uid}/created_at")
    }

    fn load_bootstrap_completed(&self, uid: &str) -> Result<bool> {
        let meta = self.account_tree(uid, ACCOUNT_TREE_META)?;
        if let Some(v) = meta
            .get(K_BOOTSTRAP_COMPLETED)
            .map_err(|e| Error::Storage(format!("load bootstrap completed: {e}")))?
        {
            return Ok(i32_from_be_slice(v.as_ref())? != 0);
        }
        Ok(self
            .load_legacy_session(uid)?
            .map(|s| s.bootstrap_completed)
            .unwrap_or(false))
    }

    fn get_or_create_master_key(&self, uid: &str, install: &InstallState) -> Result<[u8; 32]> {
        let wrap = self.account_tree(uid, ACCOUNT_TREE_WRAP)?;
        let wrapped = wrap
            .get(K_MASTER_KEY_WRAPPED)
            .map_err(|e| Error::Storage(format!("load wrapped master key: {e}")))?;
        let nonce = wrap
            .get(K_MASTER_KEY_NONCE)
            .map_err(|e| Error::Storage(format!("load wrapped master key nonce: {e}")))?;
        if let (Some(wrapped), Some(nonce)) = (wrapped, nonce) {
            return self.unwrap_master_key(uid, install, wrapped.as_ref(), nonce.as_ref());
        }
        let mut master_key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut master_key);
        let wrap_key = derive_wrap_key(install, uid)?;
        let cipher = Aes256Gcm::new_from_slice(&wrap_key)
            .map_err(|e| Error::Storage(format!("init wrap cipher: {e}")))?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let aad = wrap_aad(uid);
        let wrapped = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &master_key,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| Error::Storage(format!("wrap master key: {e}")))?;
        wrap.insert(K_MASTER_KEY_WRAPPED, wrapped)
            .map_err(|e| Error::Storage(format!("save wrapped master key: {e}")))?;
        wrap.insert(K_MASTER_KEY_NONCE, nonce.as_slice())
            .map_err(|e| Error::Storage(format!("save wrapped master key nonce: {e}")))?;
        Ok(master_key)
    }

    fn unwrap_master_key(
        &self,
        uid: &str,
        install: &InstallState,
        wrapped: &[u8],
        nonce: &[u8],
    ) -> Result<[u8; 32]> {
        if nonce.len() != 12 {
            return Err(Error::Storage(
                "invalid wrapped master key nonce length, expect 12 bytes".to_string(),
            ));
        }
        let wrap_key = derive_wrap_key(install, uid)?;
        let cipher = Aes256Gcm::new_from_slice(&wrap_key)
            .map_err(|e| Error::Storage(format!("init unwrap cipher: {e}")))?;
        let aad = wrap_aad(uid);
        let plain = cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: wrapped,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| Error::Storage(format!("unwrap master key: {e}")))?;
        plain.as_slice().try_into().map_err(|_| {
            Error::Storage("invalid master key length after unwrap, expect 32 bytes".to_string())
        })
    }

    fn load_master_key(&self, uid: &str) -> Result<[u8; 32]> {
        let install = self.get_install_state()?;
        let wrap = self.account_tree(uid, ACCOUNT_TREE_WRAP)?;
        let wrapped = wrap
            .get(K_MASTER_KEY_WRAPPED)
            .map_err(|e| Error::Storage(format!("load wrapped master key: {e}")))?
            .ok_or_else(|| Error::Storage("master key not initialized".to_string()))?;
        let nonce = wrap
            .get(K_MASTER_KEY_NONCE)
            .map_err(|e| Error::Storage(format!("load wrapped master key nonce: {e}")))?
            .ok_or_else(|| Error::Storage("master key nonce not initialized".to_string()))?;
        self.unwrap_master_key(uid, &install, wrapped.as_ref(), nonce.as_ref())
    }

    fn encrypt_user_blob(
        &self,
        uid: &str,
        purpose: &str,
        master_key: &[u8; 32],
        plain: &[u8],
    ) -> Result<EncryptedBlob> {
        let cipher = Aes256Gcm::new_from_slice(master_key)
            .map_err(|e| Error::Storage(format!("init token cipher: {e}")))?;
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let aad = token_aad(uid, purpose);
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| Error::Storage(format!("encrypt {purpose}: {e}")))?;
        Ok(EncryptedBlob {
            ciphertext,
            nonce: nonce.to_vec(),
        })
    }

    fn decrypt_user_blob(
        &self,
        uid: &str,
        purpose: &str,
        master_key: &[u8; 32],
        ciphertext: &[u8],
        nonce: &[u8],
    ) -> Result<Vec<u8>> {
        if nonce.len() != 12 {
            return Err(Error::Storage(format!(
                "invalid nonce length for {purpose}, expect 12 bytes"
            )));
        }
        let cipher = Aes256Gcm::new_from_slice(master_key)
            .map_err(|e| Error::Storage(format!("init token decipher: {e}")))?;
        let aad = token_aad(uid, purpose);
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| Error::Storage(format!("decrypt {purpose}: {e}")))
    }

    fn load_session_from_account(&self, uid: &str) -> Result<Option<SessionSnapshot>> {
        let account_db = self.open_account_db(uid)?;
        let auth = account_db
            .open_tree(ACCOUNT_TREE_AUTH)
            .map_err(|e| Error::Storage(format!("open auth tree: {e}")))?;
        let profile = account_db
            .open_tree(ACCOUNT_TREE_PROFILE)
            .map_err(|e| Error::Storage(format!("open profile tree: {e}")))?;
        let meta = account_db
            .open_tree(ACCOUNT_TREE_META)
            .map_err(|e| Error::Storage(format!("open meta tree: {e}")))?;
        let token_enc = auth
            .get(K_ACCESS_TOKEN_ENC)
            .map_err(|e| Error::Storage(format!("load access token: {e}")))?;
        let token_nonce = auth
            .get(K_ACCESS_TOKEN_NONCE)
            .map_err(|e| Error::Storage(format!("load access token nonce: {e}")))?;
        let device_id = get_string(&auth, K_DEVICE_ID_CURRENT)?;
        let profile_uid = get_string(&profile, K_UID)?;
        if token_enc.is_none()
            || token_nonce.is_none()
            || device_id.is_none()
            || profile_uid.is_none()
        {
            return Ok(None);
        }
        let master_key = self.load_master_key(uid)?;
        let token_plain = self.decrypt_user_blob(
            uid,
            "access_token",
            &master_key,
            token_enc.as_ref().expect("checked").as_ref(),
            token_nonce.as_ref().expect("checked").as_ref(),
        )?;
        let token = String::from_utf8(token_plain)
            .map_err(|e| Error::Storage(format!("decode access token utf8: {e}")))?;
        let user_id = profile_uid
            .expect("checked")
            .parse::<u64>()
            .map_err(|e| Error::Storage(format!("parse profile uid: {e}")))?;
        let bootstrap_completed = meta
            .get(K_BOOTSTRAP_COMPLETED)
            .map_err(|e| Error::Storage(format!("load bootstrap completed: {e}")))?
            .map(|v| i32_from_be_slice(v.as_ref()))
            .transpose()?
            .unwrap_or(0)
            != 0;
        Ok(Some(SessionSnapshot {
            user_id,
            token,
            device_id: device_id.expect("checked"),
            bootstrap_completed,
        }))
    }

    fn load_legacy_session(&self, uid: &str) -> Result<Option<SessionSnapshot>> {
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
        .map_err(|e| Error::Storage(format!("load legacy session: {e}")))
    }

    fn clear_legacy_session(&self, uid: &str) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute("DELETE FROM auth_session", [])
            .map_err(|e| Error::Storage(format!("clear legacy session: {e}")))?;
        Ok(())
    }

    pub fn list_local_accounts(&self) -> Result<(Option<String>, Vec<LocalAccountEntry>)> {
        let global_db = self.open_global_db()?;
        let accounts = global_db
            .open_tree(GLOBAL_TREE_ACCOUNTS)
            .map_err(|e| Error::Storage(format!("open accounts tree: {e}")))?;
        let active_uid = get_string(&accounts, K_ACTIVE_UID)?;
        let mut out = Vec::new();
        for item in accounts.iter() {
            let (k, v) = item.map_err(|e| Error::Storage(format!("iterate accounts: {e}")))?;
            let key = String::from_utf8(k.to_vec())
                .map_err(|e| Error::Storage(format!("decode account key utf8: {e}")))?;
            let Some(uid) = key
                .strip_prefix("acct/")
                .and_then(|s| s.strip_suffix("/last_login_at"))
            else {
                continue;
            };
            let last_login_at = {
                let arr: [u8; 8] = v
                    .as_ref()
                    .try_into()
                    .map_err(|_| Error::Storage("invalid last_login_at bytes".to_string()))?;
                i64::from_be_bytes(arr)
            };
            let created_key = Self::k_acct_created_at(uid);
            let created_at = match accounts
                .get(created_key.as_bytes())
                .map_err(|e| Error::Storage(format!("read account created_at: {e}")))?
            {
                Some(raw) => {
                    let arr: [u8; 8] = raw
                        .as_ref()
                        .try_into()
                        .map_err(|_| Error::Storage("invalid created_at bytes".to_string()))?;
                    i64::from_be_bytes(arr)
                }
                None => last_login_at,
            };
            out.push(LocalAccountEntry {
                uid: uid.to_string(),
                created_at,
                last_login_at,
            });
        }
        out.sort_by(|a, b| {
            b.last_login_at
                .cmp(&a.last_login_at)
                .then_with(|| a.uid.cmp(&b.uid))
        });
        Ok((active_uid, out))
    }

    pub fn flush_user(&self, uid: &str) -> Result<()> {
        let account_db = self.open_account_db(uid)?;
        account_db
            .flush()
            .map_err(|e| Error::Storage(format!("flush account db: {e}")))?;
        let global_db = self.open_global_db()?;
        global_db
            .flush()
            .map_err(|e| Error::Storage(format!("flush global db: {e}")))?;
        Ok(())
    }

    pub fn create_local_message(
        &self,
        uid: &str,
        input: &NewMessage,
        local_message_id: u64,
    ) -> Result<u64> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO message (
                server_message_id, channel_id, channel_type, from_uid, type, content,
                status, created_at, updated_at, searchable_word, local_message_id, setting, extra,
                mime_type, media_downloaded, thumb_status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                local_message_id as i64,
                input.setting,
                input.extra,
                input.mime_type,
                input.media_downloaded as i32,
                input.thumb_status,
            ],
        )
        .map_err(|e| Error::Storage(format!("insert message: {e}")))?;
        Ok(conn.last_insert_rowid() as u64)
    }

    pub fn upsert_remote_message_with_result(
        &self,
        uid: &str,
        input: &UpsertRemoteMessageInput,
    ) -> Result<UpsertRemoteMessageResult> {
        if input.server_message_id == 0 {
            return Err(Error::InvalidState(
                "server_message_id is required".to_string(),
            ));
        }
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let current_uid = uid.parse::<u64>().ok();
        let from_self = current_uid
            .map(|current| current == input.from_uid)
            .unwrap_or(false);
        let existed: Option<(i64, i64, i64)> = conn
            .query_row(
                "SELECT id, COALESCE(order_seq, 0), COALESCE(pts, 0)
                 FROM message WHERE server_message_id = ?1 LIMIT 1",
                params![input.server_message_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query remote message: {e}")))?;
        let existed_by_local: Option<(i64, i64, i64)> = if from_self && input.local_message_id > 0 {
            conn.query_row(
                "SELECT id, COALESCE(order_seq, 0), COALESCE(pts, 0)
                 FROM message WHERE from_uid = ?1 AND local_message_id = ?2 LIMIT 1",
                params![input.from_uid as i64, input.local_message_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query local message by local_message_id: {e}")))?
        } else {
            None
        };

        let update_row = |conn: &Connection, row_id: i64| -> Result<()> {
            conn.execute(
                "UPDATE message
                 SET channel_id = ?1, channel_type = ?2, timestamp = ?3, from_uid = ?4,
                     type = ?5, content = ?6, status = ?7, updated_at = ?8, searchable_word = ?9,
                     local_message_id = ?10, setting = ?11, extra = ?12, pts = ?13, order_seq = ?14,
                     server_message_id = ?15,
                     mime_type = COALESCE(?17, mime_type)
                 WHERE id = ?16",
                params![
                    input.channel_id as i64,
                    input.channel_type,
                    input.timestamp,
                    input.from_uid as i64,
                    input.message_type,
                    input.content,
                    input.status,
                    now_ms,
                    input.searchable_word,
                    input.local_message_id as i64,
                    input.setting,
                    input.extra,
                    input.pts,
                    input.order_seq,
                    input.server_message_id as i64,
                    row_id,
                    input.mime_type,
                ],
            )
            .map_err(|e| Error::Storage(format!("update remote message: {e}")))?;
            Ok(())
        };

        if let Some((row_id, current_order_seq, current_pts)) = existed {
            // Guard against out-of-order replay regressing a newer row.
            if input.order_seq < current_order_seq
                || (input.order_seq == current_order_seq && input.pts < current_pts)
            {
                return Ok(UpsertRemoteMessageResult {
                    message_id: row_id as u64,
                    inserted_new: false,
                });
            }
            if let Some((local_row_id, _, _)) = existed_by_local {
                if local_row_id != row_id {
                    // Keep the local row as canonical when local_message_id matches self-send.
                    conn.execute("DELETE FROM message WHERE id = ?1", params![row_id])
                        .map_err(|e| {
                            Error::Storage(format!("delete duplicate remote message: {e}"))
                        })?;
                    update_row(&conn, local_row_id)?;
                    return Ok(UpsertRemoteMessageResult {
                        message_id: local_row_id as u64,
                        inserted_new: false,
                    });
                }
            }
            update_row(&conn, row_id)?;
            return Ok(UpsertRemoteMessageResult {
                message_id: row_id as u64,
                inserted_new: false,
            });
        }

        if let Some((row_id, current_order_seq, current_pts)) = existed_by_local {
            if input.order_seq < current_order_seq
                || (input.order_seq == current_order_seq && input.pts < current_pts)
            {
                return Ok(UpsertRemoteMessageResult {
                    message_id: row_id as u64,
                    inserted_new: false,
                });
            }
            update_row(&conn, row_id)?;
            return Ok(UpsertRemoteMessageResult {
                message_id: row_id as u64,
                inserted_new: false,
            });
        }

        conn.execute(
            "INSERT INTO message (
                server_message_id, pts, channel_id, channel_type, timestamp, from_uid, type,
                content, status, created_at, updated_at, searchable_word, local_message_id,
                setting, order_seq, extra, mime_type
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                input.server_message_id as i64,
                input.pts,
                input.channel_id as i64,
                input.channel_type,
                input.timestamp,
                input.from_uid as i64,
                input.message_type,
                input.content,
                input.status,
                now_ms,
                input.searchable_word,
                input.local_message_id as i64,
                input.setting,
                input.order_seq,
                input.extra,
                input.mime_type,
            ],
        )
        .map_err(|e| Error::Storage(format!("insert remote message: {e}")))?;
        Ok(UpsertRemoteMessageResult {
            message_id: conn.last_insert_rowid() as u64,
            inserted_new: true,
        })
    }

    /// Batch upsert remote messages within a single SQLite transaction.
    ///
    /// Each message is processed with the same dedup logic as `upsert_remote_message`,
    /// but all writes share a single fsync (transaction commit). This provides 5-10x
    /// throughput improvement during sync.
    ///
    /// On transaction failure: rolls back the batch and retries each item individually.
    /// Returns a Vec of (index, Result<u64>) so callers can map results back.
    pub fn batch_upsert_remote_messages(
        &self,
        uid: &str,
        inputs: &[UpsertRemoteMessageInput],
    ) -> Vec<Result<UpsertRemoteMessageResult>> {
        if inputs.is_empty() {
            return vec![];
        }
        // Try batch within a single transaction first
        match self.batch_upsert_inner(uid, inputs) {
            Ok(results) => results,
            Err(_) => {
                // Transaction failed — fallback to individual upserts
                inputs
                    .iter()
                    .map(|input| self.upsert_remote_message_with_result(uid, input))
                    .collect()
            }
        }
    }

    fn batch_upsert_inner(
        &self,
        uid: &str,
        inputs: &[UpsertRemoteMessageInput],
    ) -> std::result::Result<Vec<Result<UpsertRemoteMessageResult>>, Error> {
        let mut conn = self.conn_for_user(uid)?;
        let tx = conn
            .transaction()
            .map_err(|e| Error::Storage(format!("begin batch tx: {e}")))?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let current_uid = uid.parse::<u64>().ok();

        let mut results = Vec::with_capacity(inputs.len());
        for input in inputs {
            let r = Self::upsert_one_in_tx(&tx, input, now_ms, current_uid);
            results.push(r);
        }

        tx.commit()
            .map_err(|e| Error::Storage(format!("commit batch tx: {e}")))?;
        Ok(results)
    }

    fn upsert_one_in_tx(
        tx: &rusqlite::Transaction<'_>,
        input: &UpsertRemoteMessageInput,
        now_ms: i64,
        current_uid: Option<u64>,
    ) -> Result<UpsertRemoteMessageResult> {
        if input.server_message_id == 0 {
            return Err(Error::InvalidState(
                "server_message_id is required".to_string(),
            ));
        }
        let from_self = current_uid
            .map(|current| current == input.from_uid)
            .unwrap_or(false);

        let existed: Option<(i64, i64, i64)> = tx
            .query_row(
                "SELECT id, COALESCE(order_seq, 0), COALESCE(pts, 0)
                 FROM message WHERE server_message_id = ?1 LIMIT 1",
                params![input.server_message_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query remote message: {e}")))?;

        let existed_by_local: Option<(i64, i64, i64)> = if from_self && input.local_message_id > 0 {
            tx.query_row(
                "SELECT id, COALESCE(order_seq, 0), COALESCE(pts, 0)
                 FROM message WHERE from_uid = ?1 AND local_message_id = ?2 LIMIT 1",
                params![input.from_uid as i64, input.local_message_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query local message by local_message_id: {e}")))?
        } else {
            None
        };

        let update_row = |row_id: i64| -> Result<()> {
            tx.execute(
                "UPDATE message
                 SET channel_id = ?1, channel_type = ?2, timestamp = ?3, from_uid = ?4,
                     type = ?5, content = ?6, status = ?7, updated_at = ?8, searchable_word = ?9,
                     local_message_id = ?10, setting = ?11, extra = ?12, pts = ?13, order_seq = ?14,
                     server_message_id = ?15,
                     mime_type = COALESCE(?17, mime_type)
                 WHERE id = ?16",
                params![
                    input.channel_id as i64,
                    input.channel_type,
                    input.timestamp,
                    input.from_uid as i64,
                    input.message_type,
                    input.content,
                    input.status,
                    now_ms,
                    input.searchable_word,
                    input.local_message_id as i64,
                    input.setting,
                    input.extra,
                    input.pts,
                    input.order_seq,
                    input.server_message_id as i64,
                    row_id,
                    input.mime_type,
                ],
            )
            .map_err(|e| Error::Storage(format!("update remote message: {e}")))?;
            Ok(())
        };

        if let Some((row_id, current_order_seq, current_pts)) = existed {
            if input.order_seq < current_order_seq
                || (input.order_seq == current_order_seq && input.pts < current_pts)
            {
                return Ok(UpsertRemoteMessageResult {
                    message_id: row_id as u64,
                    inserted_new: false,
                });
            }
            if let Some((local_row_id, _, _)) = existed_by_local {
                if local_row_id != row_id {
                    tx.execute("DELETE FROM message WHERE id = ?1", params![row_id])
                        .map_err(|e| {
                            Error::Storage(format!("delete duplicate remote message: {e}"))
                        })?;
                    update_row(local_row_id)?;
                    return Ok(UpsertRemoteMessageResult {
                        message_id: local_row_id as u64,
                        inserted_new: false,
                    });
                }
            }
            update_row(row_id)?;
            return Ok(UpsertRemoteMessageResult {
                message_id: row_id as u64,
                inserted_new: false,
            });
        }

        if let Some((row_id, current_order_seq, current_pts)) = existed_by_local {
            if input.order_seq < current_order_seq
                || (input.order_seq == current_order_seq && input.pts < current_pts)
            {
                return Ok(UpsertRemoteMessageResult {
                    message_id: row_id as u64,
                    inserted_new: false,
                });
            }
            update_row(row_id)?;
            return Ok(UpsertRemoteMessageResult {
                message_id: row_id as u64,
                inserted_new: false,
            });
        }

        tx.execute(
            "INSERT INTO message (
                server_message_id, pts, channel_id, channel_type, timestamp, from_uid, type,
                content, status, created_at, updated_at, searchable_word, local_message_id,
                setting, order_seq, extra, mime_type
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                input.server_message_id as i64,
                input.pts,
                input.channel_id as i64,
                input.channel_type,
                input.timestamp,
                input.from_uid as i64,
                input.message_type,
                input.content,
                input.status,
                now_ms,
                input.searchable_word,
                input.local_message_id as i64,
                input.setting,
                input.order_seq,
                input.extra,
                input.mime_type,
            ],
        )
        .map_err(|e| Error::Storage(format!("insert remote message: {e}")))?;
        Ok(UpsertRemoteMessageResult {
            message_id: tx.last_insert_rowid() as u64,
            inserted_new: true,
        })
    }

    pub fn get_message_by_id(&self, uid: &str, message_id: u64) -> Result<Option<StoredMessage>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                m.id, m.server_message_id, m.channel_id, m.channel_type, m.from_uid, m.type,
                m.content, m.status, m.created_at, m.updated_at, m.extra, m.local_message_id,
                COALESCE(me.revoke, 0), me.revoker,
                m.mime_type, m.media_downloaded, m.thumb_status,
                COALESCE(me.delivered, 0),
                m.pts
             FROM message m
             LEFT JOIN message_extra me ON me.message_id = m.id
             WHERE m.id = ?1 LIMIT 1",
            params![message_id as i64],
            |row| {
                Ok(StoredMessage {
                    message_id: row.get::<_, i64>(0)? as u64,
                    server_message_id: row.get::<_, Option<i64>>(1)?.map(|v| v as u64),
                    local_message_id: row
                        .get::<_, Option<i64>>(11)?
                        .filter(|&v| v > 0)
                        .map(|v| v as u64),
                    channel_id: row.get::<_, i64>(2)? as u64,
                    channel_type: row.get::<_, i32>(3)?,
                    from_uid: row.get::<_, i64>(4)? as u64,
                    message_type: row.get::<_, i32>(5)?,
                    content: row.get::<_, String>(6)?,
                    status: row.get::<_, i32>(7)?,
                    created_at: row.get::<_, i64>(8)?,
                    updated_at: row.get::<_, i64>(9)?,
                    extra: row.get::<_, String>(10)?,
                    revoked: row.get::<_, i32>(12)? != 0,
                    revoked_by: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                    mime_type: row.get::<_, Option<String>>(14)?,
                    media_downloaded: row.get::<_, i32>(15).unwrap_or(0) != 0,
                    thumb_status: row.get::<_, i32>(16).unwrap_or(0),
                    delivered: row.get::<_, i32>(17).unwrap_or(0) != 0,
                    pts: row.get::<_, Option<i64>>(18)?.filter(|&v| v > 0).map(|v| v as u64),
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get message by id: {e}")))
    }

    pub fn get_message_id_by_server_message_id(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) -> Result<Option<u64>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT id FROM message
             WHERE server_message_id = ?1 AND channel_id = ?2 AND channel_type = ?3
             LIMIT 1",
            params![server_message_id as i64, channel_id as i64, channel_type],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get message id by server_message_id: {e}")))
    }

    pub fn set_message_revoke_by_server_message_id(
        &self,
        uid: &str,
        server_message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<Option<StoredMessage>> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = if revoked {
            conn.execute(
                "UPDATE message
                 SET type = 0, content = ?1, searchable_word = ?1, updated_at = ?2
                 WHERE server_message_id = ?3",
                params!["消息已撤回", now_ms, server_message_id as i64],
            )
            .map_err(|e| Error::Storage(format!("update revoked message by server id: {e}")))?
        } else {
            0
        };
        if updated == 0 {
            return Ok(None);
        }
        let (message_id, channel_id, channel_type): (i64, i64, i32) = conn
            .query_row(
                "SELECT id, channel_id, channel_type
                 FROM message
                 WHERE server_message_id = ?1
                 LIMIT 1",
                params![server_message_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| Error::Storage(format!("query revoked message by server id: {e}")))?;
        conn.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, revoke, revoker, extra_version
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(message_id) DO UPDATE SET
                revoke=excluded.revoke,
                revoker=excluded.revoker,
                extra_version=MAX(message_extra.extra_version, excluded.extra_version)",
            params![
                message_id,
                channel_id,
                channel_type,
                if revoked { 1 } else { 0 },
                revoker.map(|v| v as i64),
                now_ms,
            ],
        )
        .map_err(|e| Error::Storage(format!("set message revoke by server id: {e}")))?;
        self.get_message_by_id(uid, message_id as u64)
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
                    m.id, m.server_message_id, m.channel_id, m.channel_type, m.from_uid, m.type,
                    m.content, m.status, m.created_at, m.updated_at, m.extra, m.local_message_id,
                    COALESCE(me.revoke, 0), me.revoker,
                    m.mime_type, m.media_downloaded, m.thumb_status,
                    COALESCE(me.delivered, 0),
                    m.pts
                 FROM message m
                 LEFT JOIN message_extra me ON me.message_id = m.id
                 WHERE m.channel_id = ?1 AND m.channel_type = ?2
                 ORDER BY m.id DESC
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
                        local_message_id: row
                            .get::<_, Option<i64>>(11)?
                            .filter(|&v| v > 0)
                            .map(|v| v as u64),
                        channel_id: row.get::<_, i64>(2)? as u64,
                        channel_type: row.get::<_, i32>(3)?,
                        from_uid: row.get::<_, i64>(4)? as u64,
                        message_type: row.get::<_, i32>(5)?,
                        content: row.get::<_, String>(6)?,
                        status: row.get::<_, i32>(7)?,
                        created_at: row.get::<_, i64>(8)?,
                        updated_at: row.get::<_, i64>(9)?,
                        extra: row.get::<_, String>(10)?,
                        revoked: row.get::<_, i32>(12)? != 0,
                        revoked_by: row.get::<_, Option<i64>>(13)?.map(|v| v as u64),
                        mime_type: row.get::<_, Option<String>>(14)?,
                        media_downloaded: row.get::<_, i32>(15).unwrap_or(0) != 0,
                        thumb_status: row.get::<_, i32>(16).unwrap_or(0),
                        delivered: row.get::<_, i32>(17).unwrap_or(0) != 0,
                        pts: row.get::<_, Option<i64>>(18)?.filter(|&v| v > 0).map(|v| v as u64),
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

    /// 更新消息的缩略图状态：0=missing, 1=ready, 2=failed
    pub fn update_thumb_status(
        &self,
        uid: &str,
        message_id: u64,
        thumb_status: i32,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE message SET thumb_status = ?1, updated_at = ?3 WHERE id = ?2",
            params![thumb_status, message_id as i64, chrono::Utc::now().timestamp_millis()],
        )
        .map_err(|e| Error::Storage(format!("update thumb_status: {e}")))?;
        Ok(())
    }

    /// 更新消息的主文件下载状态
    pub fn update_media_downloaded(
        &self,
        uid: &str,
        message_id: u64,
        downloaded: bool,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE message SET media_downloaded = ?1, updated_at = ?3 WHERE id = ?2",
            params![downloaded as i32, message_id as i64, chrono::Utc::now().timestamp_millis()],
        )
        .map_err(|e| Error::Storage(format!("update media_downloaded: {e}")))?;
        Ok(())
    }

    pub fn max_message_pts(&self, uid: &str, channel_id: u64, channel_type: i32) -> Result<u64> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT COALESCE(MAX(COALESCE(pts, 0)), 0)
             FROM message
             WHERE channel_id = ?1 AND channel_type = ?2",
            params![channel_id as i64, channel_type],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .map_err(|e| Error::Storage(format!("query max message pts: {e}")))
    }

    pub fn upsert_channel(&self, uid: &str, input: &UpsertChannelInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO channel (
                channel_id, channel_type, channel_name, channel_remark, avatar,
                unread_count, top, mute, last_msg_timestamp, last_local_message_id,
                last_msg_content, version, updated_at, created_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?13
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
                version=excluded.version,
                updated_at=excluded.updated_at
             WHERE excluded.version >= channel.version",
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
                input.version,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert channel: {e}")))?;
        Ok(())
    }

    pub fn get_channel_by_id(&self, uid: &str, channel_id: u64) -> Result<Option<StoredChannel>> {
        let conn = self.conn_for_user(uid)?;
        let uid_i64 = uid.parse::<i64>().unwrap_or_default();
        let mut channel = conn.query_row(
            "SELECT
                c.channel_id,
                c.channel_type,
                CASE
                        WHEN c.channel_type = 1 THEN COALESCE(
                            (
                                SELECT COALESCE(
                                    NULLIF(u.alias, ''),
                                    NULLIF(u.nickname, ''),
                                    NULLIF(u.username, ''),
                                    CASE
                                        WHEN peer.peer_user_id = 1 THEN 'System Message'
                                        ELSE CAST(peer.peer_user_id AS TEXT)
                                    END
                                )
                                FROM (
                                    SELECT COALESCE(
                                        (
                                            SELECT cm.member_uid
                                            FROM channel_member cm
                                            WHERE cm.channel_id = c.channel_id
                                              AND cm.channel_type = c.channel_type
                                              AND cm.member_uid != ?2
                                            ORDER BY cm.role ASC, cm.member_uid ASC
                                            LIMIT 1
                                        ),
                                        CASE
                                            WHEN c.channel_name GLOB '[0-9]*' AND c.channel_name <> ''
                                            THEN CAST(c.channel_name AS INTEGER)
                                            ELSE NULL
                                        END
                                    ) AS peer_user_id
                                ) peer
                                LEFT JOIN friend f ON f.user_id = peer.peer_user_id
                                LEFT JOIN \"user\" u ON u.user_id = COALESCE(f.user_id, peer.peer_user_id)
                                WHERE peer.peer_user_id IS NOT NULL
                                LIMIT 1
                            ),
                            CASE
                                WHEN NULLIF(c.channel_name, '') IN ('1', '__system_1__') THEN 'System Message'
                                ELSE NULLIF(c.channel_name, '')
                            END,
                            ''
                        )
                    WHEN c.channel_type = 2 THEN COALESCE(
                        NULLIF(c.channel_name, ''),
                        (
                            SELECT NULLIF(g.name, '')
                            FROM \"group\" g
                            WHERE g.group_id = c.channel_id
                            LIMIT 1
                        ),
                        (
                            SELECT NULLIF(group_concat(name_part, '、'), '')
                            FROM (
                                SELECT COALESCE(
                                    NULLIF(gm.alias, ''),
                                    NULLIF(u.alias, ''),
                                    NULLIF(u.nickname, ''),
                                    NULLIF(u.username, ''),
                                    CAST(gm.user_id AS TEXT)
                                ) AS name_part
                                FROM group_member gm
                                LEFT JOIN \"user\" u ON u.user_id = gm.user_id
                                WHERE gm.group_id = c.channel_id
                                  AND gm.status = 0
                                ORDER BY gm.role ASC, gm.joined_at ASC, gm.user_id ASC
                                LIMIT 3
                            )
                        ),
                        CAST(c.channel_id AS TEXT)
                    )
                    ELSE c.channel_name
                END AS resolved_channel_name,
                c.channel_remark,
                c.avatar,
                unread_count, top, mute,
                COALESCE(
                    (
                        SELECT m.created_at
                        FROM message m
                        WHERE m.channel_id = c.channel_id
                          AND m.channel_type = c.channel_type
                        ORDER BY m.created_at DESC, m.id DESC
                        LIMIT 1
                    ),
                    NULLIF(c.last_msg_timestamp, 0),
                    c.updated_at,
                    0
                ) AS resolved_last_msg_timestamp,
                COALESCE(
                    (
                        SELECT m.id
                        FROM message m
                        WHERE m.channel_id = c.channel_id
                          AND m.channel_type = c.channel_type
                        ORDER BY m.created_at DESC, m.id DESC
                        LIMIT 1
                    ),
                    c.last_local_message_id
                ) AS resolved_last_local_message_id,
                COALESCE(
                    (
                        SELECT m.content
                        FROM message m
                        WHERE m.channel_id = c.channel_id
                          AND m.channel_type = c.channel_type
                        ORDER BY m.created_at DESC, m.id DESC
                        LIMIT 1
                    ),
                    c.last_msg_content,
                    ''
                ) AS resolved_last_msg_content,
                c.version,
                c.updated_at
             FROM channel c
             WHERE c.channel_id = ?1
             LIMIT 1",
            params![channel_id as i64, uid_i64],
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
                    version: row.get::<_, i64>(11)?,
                    updated_at: row.get::<_, i64>(12)?,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get channel by id: {e}")))?;
        if let Some(existing) = channel.as_mut() {
            existing.unread_count =
                self.resolve_channel_unread_on_read(&conn, uid_i64, existing)?;
        }
        Ok(channel)
    }

    pub fn list_channels(
        &self,
        uid: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannel>> {
        let conn = self.conn_for_user(uid)?;
        let uid_i64 = uid.parse::<i64>().unwrap_or_default();
        let mut stmt = conn
            .prepare(
                "SELECT
                    c.channel_id,
                    c.channel_type,
                    CASE
                        WHEN c.channel_type = 1 THEN COALESCE(
                            (
                                SELECT COALESCE(
                                    NULLIF(u.alias, ''),
                                    NULLIF(u.nickname, ''),
                                    NULLIF(u.username, ''),
                                    CASE
                                        WHEN peer.peer_user_id = 1 THEN 'System Message'
                                        ELSE CAST(peer.peer_user_id AS TEXT)
                                    END
                                )
                                FROM (
                                    SELECT COALESCE(
                                        (
                                            SELECT cm.member_uid
                                            FROM channel_member cm
                                            WHERE cm.channel_id = c.channel_id
                                              AND cm.channel_type = c.channel_type
                                              AND cm.member_uid != ?3
                                            ORDER BY cm.role ASC, cm.member_uid ASC
                                            LIMIT 1
                                        ),
                                        CASE
                                            WHEN c.channel_name GLOB '[0-9]*' AND c.channel_name <> ''
                                            THEN CAST(c.channel_name AS INTEGER)
                                            ELSE NULL
                                        END
                                    ) AS peer_user_id
                                ) peer
                                LEFT JOIN friend f ON f.user_id = peer.peer_user_id
                                LEFT JOIN \"user\" u ON u.user_id = COALESCE(f.user_id, peer.peer_user_id)
                                WHERE peer.peer_user_id IS NOT NULL
                                LIMIT 1
                            ),
                            CASE
                                WHEN NULLIF(c.channel_name, '') IN ('1', '__system_1__') THEN 'System Message'
                                ELSE NULLIF(c.channel_name, '')
                            END,
                            ''
                        )
                        WHEN c.channel_type = 2 THEN COALESCE(
                            NULLIF(c.channel_name, ''),
                            (
                                SELECT NULLIF(g.name, '')
                                FROM \"group\" g
                                WHERE g.group_id = c.channel_id
                                LIMIT 1
                            ),
                            (
                                SELECT NULLIF(group_concat(name_part, '、'), '')
                                FROM (
                                    SELECT COALESCE(
                                        NULLIF(gm.alias, ''),
                                        NULLIF(u.alias, ''),
                                        NULLIF(u.nickname, ''),
                                        NULLIF(u.username, ''),
                                        CAST(gm.user_id AS TEXT)
                                    ) AS name_part
                                    FROM group_member gm
                                    LEFT JOIN \"user\" u ON u.user_id = gm.user_id
                                    WHERE gm.group_id = c.channel_id
                                      AND gm.status = 0
                                    ORDER BY gm.role ASC, gm.joined_at ASC, gm.user_id ASC
                                    LIMIT 3
                                )
                            ),
                            CAST(c.channel_id AS TEXT)
                        )
                        ELSE c.channel_name
                    END AS resolved_channel_name,
                    c.channel_remark,
                    c.avatar,
                    unread_count, top, mute,
                    COALESCE(
                        (
                            SELECT m.created_at
                            FROM message m
                            WHERE m.channel_id = c.channel_id
                              AND m.channel_type = c.channel_type
                            ORDER BY m.created_at DESC, m.id DESC
                            LIMIT 1
                        ),
                        NULLIF(c.last_msg_timestamp, 0),
                        c.updated_at,
                        0
                    ) AS resolved_last_msg_timestamp,
                    COALESCE(
                        (
                            SELECT m.id
                            FROM message m
                            WHERE m.channel_id = c.channel_id
                              AND m.channel_type = c.channel_type
                            ORDER BY m.created_at DESC, m.id DESC
                            LIMIT 1
                        ),
                        c.last_local_message_id
                    ) AS resolved_last_local_message_id,
                    COALESCE(
                        (
                            SELECT m.content
                            FROM message m
                            WHERE m.channel_id = c.channel_id
                              AND m.channel_type = c.channel_type
                            ORDER BY m.created_at DESC, m.id DESC
                            LIMIT 1
                        ),
                        c.last_msg_content,
                        ''
                    ) AS resolved_last_msg_content,
                    c.version,
                    c.updated_at
                 FROM channel c
                 ORDER BY c.top DESC, resolved_last_msg_timestamp DESC, c.channel_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list channels: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64, uid_i64], |row| {
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
                    version: row.get::<_, i64>(11)?,
                    updated_at: row.get::<_, i64>(12)?,
                })
            })
            .map_err(|e| Error::Storage(format!("query list channels: {e}")))?;

        let mut out = Vec::new();
        for row in rows {
            let mut channel =
                row.map_err(|e| Error::Storage(format!("decode list channels row: {e}")))?;
            channel.unread_count = self.resolve_channel_unread_on_read(&conn, uid_i64, &channel)?;
            out.push(channel);
        }
        Ok(out)
    }

    fn resolve_channel_unread_on_read(
        &self,
        conn: &Connection,
        uid_i64: i64,
        channel: &StoredChannel,
    ) -> Result<i32> {
        let keep_pts: Option<i64> = conn
            .query_row(
                "SELECT keep_pts
                 FROM channel_extra
                 WHERE channel_id = ?1 AND channel_type = ?2
                 LIMIT 1",
                params![channel.channel_id as i64, channel.channel_type],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("resolve channel unread query keep_pts: {e}")))?;
        let Some(keep_pts) = keep_pts else {
            return Ok(channel.unread_count.max(0));
        };

        let max_pts: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(pts), 0)
                 FROM message
                 WHERE channel_id = ?1 AND channel_type = ?2",
                params![channel.channel_id as i64, channel.channel_type],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("resolve channel unread query max_pts: {e}")))?;
        if max_pts == 0 || keep_pts < max_pts {
            return Ok(channel.unread_count.max(0));
        }

        let exact_unread: i32 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM message
                 WHERE channel_id = ?1
                   AND channel_type = ?2
                   AND from_uid != ?3
                   AND COALESCE(pts, 0) > ?4",
                params![
                    channel.channel_id as i64,
                    channel.channel_type,
                    uid_i64,
                    keep_pts
                ],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("resolve channel unread exact count: {e}")))?;
        let exact_unread = exact_unread.max(0);

        // Guard against regressed/non-monotonic message pts from inbound push:
        // in that case exact_unread may be computed as 0 while channel.unread_count
        // has just been bumped by realtime ingress, causing badge to "flash then disappear".
        //
        // We only suppress this specific self-heal path when:
        // - keep_pts has already reached max_pts (read cursor appears up-to-date),
        // - exact_unread is 0,
        // - but channel currently carries unread > 0.
        //
        // This keeps unread stable until a real read projection clears it.
        if keep_pts >= max_pts && exact_unread == 0 && channel.unread_count.max(0) > 0 {
            return Ok(channel.unread_count.max(0));
        }

        if exact_unread != channel.unread_count.max(0) {
            conn.execute(
                "UPDATE channel
                 SET unread_count = ?2, updated_at = ?3
                 WHERE channel_id = ?1",
                params![
                    channel.channel_id as i64,
                    exact_unread,
                    chrono::Utc::now().timestamp_millis()
                ],
            )
            .map_err(|e| {
                Error::Storage(format!("resolve channel unread persist self-heal: {e}"))
            })?;
        }

        Ok(exact_unread)
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
                draft, draft_updated_at, version, peer_read_pts
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
                    peer_read_pts: row.get::<_, i64>(8)? as u64,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get channel_extra: {e}")))
    }

    /// Persist peer read pts (monotonic max) into channel_extra.
    pub fn save_peer_read_pts(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        peer_read_pts: u64,
    ) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO channel_extra (channel_id, channel_type, peer_read_pts)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(channel_id, channel_type) DO UPDATE SET
                peer_read_pts = MAX(channel_extra.peer_read_pts, excluded.peer_read_pts)",
            params![channel_id as i64, channel_type, peer_read_pts as i64],
        )
        .map_err(|e| Error::Storage(format!("save_peer_read_pts: {e}")))?;
        Ok(())
    }

    /// Get peer read pts for a channel (cold start query).
    pub fn get_peer_read_pts(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<u64>> {
        let conn = self.conn_for_user(uid)?;
        let pts: Option<i64> = conn
            .query_row(
                "SELECT peer_read_pts FROM channel_extra
                 WHERE channel_id = ?1 AND channel_type = ?2
                 LIMIT 1",
                params![channel_id as i64, channel_type],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("get_peer_read_pts: {e}")))?;
        Ok(pts.filter(|&v| v > 0).map(|v| v as u64))
    }

    pub fn upsert_user(&self, uid: &str, input: &UpsertUserInput) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "INSERT INTO user (
                user_id, username, nickname, alias, avatar,
                user_type, is_deleted, channel_id, version, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(user_id) DO UPDATE SET
                username=excluded.username,
                nickname=excluded.nickname,
                alias=excluded.alias,
                avatar=excluded.avatar,
                user_type=excluded.user_type,
                is_deleted=excluded.is_deleted,
                channel_id=excluded.channel_id,
                version=excluded.version,
                updated_at=excluded.updated_at
             WHERE excluded.version >= user.version",
            params![
                input.user_id as i64,
                input.username,
                input.nickname,
                input.alias,
                input.avatar,
                input.user_type,
                if input.is_deleted { 1 } else { 0 },
                input.channel_id,
                input.version,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert user: {e}")))?;
        Ok(())
    }

    pub fn update_user_alias(&self, uid: &str, user_id: u64, alias: Option<String>) -> Result<()> {
        let conn = self.conn_for_user(uid)?;
        conn.execute(
            "UPDATE user SET alias = ?2, updated_at = ?3 WHERE user_id = ?1",
            params![
                user_id as i64,
                alias,
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .map_err(|e| Error::Storage(format!("update_user_alias: {e}")))?;
        Ok(())
    }

    pub fn get_user_by_id(&self, uid: &str, user_id: u64) -> Result<Option<StoredUser>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT
                user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, version, updated_at
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
                    version: row.get::<_, i64>(8)?,
                    updated_at: row.get::<_, i64>(9)?,
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
            "SELECT user_id, username, nickname, alias, avatar, user_type, is_deleted, channel_id, version, updated_at
             FROM user
             WHERE user_id IN ({})
             ORDER BY version DESC, user_id DESC",
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
                    version: row.get::<_, i64>(8)?,
                    updated_at: row.get::<_, i64>(9)?,
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
            "INSERT INTO friend (user_id, tags, is_pinned, created_at, version, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id) DO UPDATE SET
                tags=excluded.tags,
                is_pinned=excluded.is_pinned,
                created_at=excluded.created_at,
                version=excluded.version,
                updated_at=excluded.updated_at
             WHERE excluded.version >= friend.version",
            params![
                input.user_id as i64,
                input.tags,
                if input.is_pinned { 1 } else { 0 },
                input.created_at,
                input.version,
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
                "SELECT
                    f.user_id,
                    u.username,
                    u.nickname,
                    u.alias,
                    COALESCE(u.avatar, ''),
                    f.tags,
                    f.is_pinned,
                    f.created_at,
                    f.version,
                    f.updated_at
                 FROM friend f
                 LEFT JOIN \"user\" u ON u.user_id = f.user_id
                 ORDER BY f.is_pinned DESC, f.version DESC, f.user_id DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| Error::Storage(format!("prepare list friends: {e}")))?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                Ok(StoredFriend {
                    user_id: row.get::<_, i64>(0)? as u64,
                    username: row.get::<_, Option<String>>(1)?,
                    nickname: row.get::<_, Option<String>>(2)?,
                    alias: row.get::<_, Option<String>>(3)?,
                    avatar: row.get::<_, String>(4)?,
                    tags: row.get::<_, Option<String>>(5)?,
                    is_pinned: row.get::<_, i32>(6)? != 0,
                    created_at: row.get::<_, i64>(7)?,
                    version: row.get::<_, i64>(8)?,
                    updated_at: row.get::<_, i64>(9)?,
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
            "INSERT INTO \"group\" (group_id, name, avatar, owner_id, is_dismissed, created_at, version, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(group_id) DO UPDATE SET
                name=excluded.name,
                avatar=excluded.avatar,
                owner_id=excluded.owner_id,
                is_dismissed=excluded.is_dismissed,
                created_at=excluded.created_at,
                version=excluded.version,
                updated_at=excluded.updated_at
             WHERE excluded.version >= \"group\".version",
            params![
                input.group_id as i64,
                input.name,
                input.avatar,
                input.owner_id.map(|v| v as i64),
                if input.is_dismissed { 1 } else { 0 },
                input.created_at,
                input.version,
                input.updated_at
            ],
        )
        .map_err(|e| Error::Storage(format!("upsert group: {e}")))?;
        Ok(())
    }

    pub fn get_group_by_id(&self, uid: &str, group_id: u64) -> Result<Option<StoredGroup>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, version, updated_at
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
                    version: row.get::<_, i64>(6)?,
                    updated_at: row.get::<_, i64>(7)?,
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
                "SELECT group_id, name, avatar, owner_id, is_dismissed, created_at, version, updated_at
                 FROM \"group\"
                 ORDER BY version DESC, group_id DESC
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
                    version: row.get::<_, i64>(6)?,
                    updated_at: row.get::<_, i64>(7)?,
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
                group_id, user_id, role, status, alias, is_muted, joined_at, version, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(group_id, user_id) DO UPDATE SET
                role=excluded.role,
                status=excluded.status,
                alias=excluded.alias,
                is_muted=excluded.is_muted,
                joined_at=excluded.joined_at,
                version=excluded.version,
                updated_at=excluded.updated_at
             WHERE excluded.version >= group_member.version",
            params![
                input.group_id as i64,
                input.user_id as i64,
                input.role,
                input.status,
                input.alias,
                if input.is_muted { 1 } else { 0 },
                input.joined_at,
                input.version,
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
                "SELECT group_id, user_id, role, status, alias, is_muted, joined_at, version, updated_at
                 FROM group_member
                 WHERE group_id = ?1
                 ORDER BY version DESC, user_id DESC
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
                        version: row.get::<_, i64>(7)?,
                        updated_at: row.get::<_, i64>(8)?,
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
        let mut conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let tx = conn
            .transaction()
            .map_err(|e| Error::Storage(format!("mark message sent begin tx: {e}")))?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT id FROM message WHERE id = ?1 LIMIT 1",
                params![message_id as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("mark message sent find message: {e}")))?;
        let Some(_) = exists else {
            return Err(Error::Storage(format!(
                "mark message sent failed: message.id={} not found",
                message_id
            )));
        };
        tx.execute(
            "DELETE FROM message
             WHERE server_message_id = ?1 AND id != ?2",
            params![server_message_id as i64, message_id as i64],
        )
        .map_err(|e| Error::Storage(format!("mark message sent dedupe: {e}")))?;
        let updated = tx
            .execute(
                "UPDATE message
                 SET server_message_id = ?1, status = 2, updated_at = ?2
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
        tx.commit()
            .map_err(|e| Error::Storage(format!("mark message sent commit: {e}")))?;
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

    pub fn get_local_message_id(&self, uid: &str, message_id: u64) -> Result<Option<u64>> {
        let conn = self.conn_for_user(uid)?;
        conn.query_row(
            "SELECT local_message_id FROM message WHERE id = ?1 LIMIT 1",
            params![message_id as i64],
            |row| Ok(row.get::<_, Option<i64>>(0)?.map(|v| v as u64)),
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get local_message_id: {e}")))
        .map(|v| v.flatten())
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
        if revoked {
            let updated = conn
                .execute(
                    "UPDATE message
                     SET type = 0, content = ?1, searchable_word = ?1, updated_at = ?2
                     WHERE id = ?3",
                    params!["消息已撤回", now_ms, message_id as i64],
                )
                .map_err(|e| Error::Storage(format!("update revoked message row: {e}")))?;
            if updated == 0 {
                return Err(Error::Storage(format!(
                    "set message revoke failed: message.id={} not found",
                    message_id
                )));
            }
        }
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
                    revoker, extra_version, is_mutual_deleted, content_edit, edited_at, need_upload, is_pinned,
                    delivered, delivered_at
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
                    delivered: row.get::<_, i32>(14)? != 0,
                    delivered_at: row.get::<_, i64>(15)? as u64,
                })
            },
        )
        .optional()
        .map_err(|e| Error::Storage(format!("get message extra: {e}")))
    }

    /// Mark a message as delivered (only upgrades 0→1, never downgrades).
    /// Returns true if the row was actually updated (first receipt).
    pub fn mark_message_delivered(
        &self,
        uid: &str,
        server_message_id: u64,
        delivered_at: u64,
    ) -> Result<bool> {
        let conn = self.conn_for_user(uid)?;
        // Resolve local message_id from server_message_id
        let message_row: Option<(i64, i64, i32)> = conn
            .query_row(
                "SELECT id, channel_id, channel_type FROM message WHERE server_message_id = ?1 LIMIT 1",
                params![server_message_id as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("mark_delivered lookup: {e}")))?;
        let Some((message_id, channel_id, channel_type)) = message_row else {
            return Ok(false);
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let updated = conn
            .execute(
                "INSERT INTO message_extra (
                    message_id, channel_id, channel_type, delivered, delivered_at, extra_version
                 ) VALUES (?1, ?2, ?3, 1, ?4, ?5)
                 ON CONFLICT(message_id) DO UPDATE SET
                    delivered = 1,
                    delivered_at = excluded.delivered_at,
                    extra_version = excluded.extra_version
                 WHERE message_extra.delivered = 0",
                params![message_id, channel_id, channel_type, delivered_at as i64, now_ms],
            )
            .map_err(|e| Error::Storage(format!("mark_delivered upsert: {e}")))?;
        Ok(updated > 0)
    }

    /// 基于 read cursor 精确投影本地读状态：
    /// - message.pts <= last_read_pts 视为已读
    /// - message.pts > last_read_pts 且非自己发送 视为未读
    pub fn project_channel_read_cursor(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
        last_read_pts: u64,
    ) -> Result<()> {
        let mut conn = self.conn_for_user(uid)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let uid_num = uid.parse::<u64>().unwrap_or(0);
        let tx = conn
            .transaction()
            .map_err(|e| Error::Storage(format!("project channel cursor begin tx: {e}")))?;
        let existing_keep_pts: u64 = tx
            .query_row(
                "SELECT keep_pts
                 FROM channel_extra
                 WHERE channel_id = ?1 AND channel_type = ?2
                 LIMIT 1",
                params![channel_id as i64, channel_type],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("project channel cursor query keep_pts: {e}")))?
            .unwrap_or(0);
        let effective_read_pts = existing_keep_pts.max(last_read_pts);

        if effective_read_pts == existing_keep_pts {
            tx.commit()
                .map_err(|e| Error::Storage(format!("project channel cursor commit tx: {e}")))?;
            return Ok(());
        }

        tx.execute(
            "INSERT INTO channel_extra (
                channel_id, channel_type, browse_to, keep_pts, keep_offset_y,
                draft, draft_updated_at, version
            ) VALUES (?1, ?2, ?3, ?3, 0, '', 0, ?4)
            ON CONFLICT(channel_id, channel_type) DO UPDATE SET
                browse_to = MAX(channel_extra.browse_to, excluded.browse_to),
                keep_pts = MAX(channel_extra.keep_pts, excluded.keep_pts),
                version = excluded.version",
            params![
                channel_id as i64,
                channel_type,
                effective_read_pts as i64,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("project channel cursor upsert channel_extra: {e}")))?;

        tx.execute(
            "INSERT INTO message_extra (
                message_id, channel_id, channel_type, readed, readed_count, unread_count, extra_version
            )
            SELECT
                m.id AS message_id,
                m.channel_id,
                m.channel_type,
                1 AS readed,
                1 AS readed_count,
                0 AS unread_count,
                ?5 AS extra_version
            FROM message m
            WHERE m.channel_id = ?1
              AND m.channel_type = ?2
              AND COALESCE(m.pts, 0) > ?3
              AND COALESCE(m.pts, 0) <= ?4
            ON CONFLICT(message_id) DO UPDATE SET
                readed = 1,
                readed_count = CASE
                    WHEN message_extra.readed_count < 1 THEN 1
                    ELSE message_extra.readed_count
                END,
                unread_count = 0,
                extra_version = excluded.extra_version",
            params![
                channel_id as i64,
                channel_type,
                existing_keep_pts as i64,
                effective_read_pts as i64,
                now_ms
            ],
        )
        .map_err(|e| Error::Storage(format!("project channel cursor upsert message_extra: {e}")))?;

        let newly_read_unread_count: i64 = tx
            .query_row(
                "SELECT COUNT(1)
                 FROM message m
                 WHERE m.channel_id = ?1
                   AND m.channel_type = ?2
                   AND m.from_uid != ?3
                   AND COALESCE(m.pts, 0) > ?4
                   AND COALESCE(m.pts, 0) <= ?5",
                params![
                    channel_id as i64,
                    channel_type,
                    uid_num as i64,
                    existing_keep_pts as i64,
                    effective_read_pts as i64
                ],
                |r| r.get(0),
            )
            .map_err(|e| {
                Error::Storage(format!(
                    "project channel cursor query newly read count: {e}"
                ))
            })?;

        let existing_unread: Option<i64> = tx
            .query_row(
                "SELECT unread_count
                 FROM channel
                 WHERE channel_id = ?1
                 LIMIT 1",
                params![channel_id as i64],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| {
                Error::Storage(format!("project channel cursor query existing unread: {e}"))
            })?;

        let unread_total: i64 = if let Some(current) = existing_unread {
            std::cmp::max(0, current.saturating_sub(newly_read_unread_count))
        } else {
            tx.query_row(
                "SELECT COUNT(1)
                 FROM message m
                 WHERE m.channel_id = ?1
                   AND m.channel_type = ?2
                   AND m.from_uid != ?3
                   AND COALESCE(m.pts, 0) > ?4",
                params![
                    channel_id as i64,
                    channel_type,
                    uid_num as i64,
                    effective_read_pts as i64
                ],
                |r| r.get(0),
            )
            .map_err(|e| {
                Error::Storage(format!("project channel cursor query unread_total: {e}"))
            })?
        };

        tx.execute(
            "INSERT INTO channel (channel_id, channel_type, unread_count, updated_at, created_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(channel_id) DO UPDATE SET
                unread_count=excluded.unread_count,
                updated_at=excluded.updated_at",
            params![channel_id as i64, channel_type, unread_total as i32, now_ms],
        )
        .map_err(|e| Error::Storage(format!("project channel cursor upsert channel: {e}")))?;
        tx.commit()
            .map_err(|e| Error::Storage(format!("project channel cursor commit tx: {e}")))?;
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

    pub fn count_materialized_unread(
        &self,
        uid: &str,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        let conn = self.conn_for_user(uid)?;
        let uid_num = uid.parse::<u64>().unwrap_or(0);
        let keep_pts: i64 = conn
            .query_row(
                "SELECT keep_pts
                 FROM channel_extra
                 WHERE channel_id = ?1 AND channel_type = ?2
                 LIMIT 1",
                params![channel_id as i64, channel_type],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| Error::Storage(format!("query channel keep_pts: {e}")))?
            .unwrap_or(0);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM message m
                 WHERE m.channel_id = ?1
                   AND m.channel_type = ?2
                   AND m.from_uid != ?3
                   AND COALESCE(m.pts, 0) > ?4",
                params![channel_id as i64, channel_type, uid_num as i64, keep_pts],
                |r| r.get(0),
            )
            .map_err(|e| Error::Storage(format!("count materialized unread: {e}")))?;
        Ok(count as i32)
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

    #[allow(dead_code)]
    pub fn wipe_user_full(&self, uid: &str) -> Result<()> {
        if let Ok(mut guard) = self.account_dbs.lock() {
            let _ = guard.remove(uid);
        }
        let paths = self.storage_paths(uid);
        if paths.user_root.exists() {
            std::fs::remove_dir_all(&paths.user_root)
                .map_err(|e| Error::Storage(format!("remove user dir: {e}")))?;
        }
        let global_db = self.open_global_db()?;
        let accounts = global_db
            .open_tree(GLOBAL_TREE_ACCOUNTS)
            .map_err(|e| Error::Storage(format!("open accounts tree: {e}")))?;
        accounts
            .remove(Self::k_acct_last_login(uid).as_bytes())
            .map_err(|e| Error::Storage(format!("remove account last_login_at: {e}")))?;
        accounts
            .remove(Self::k_acct_created_at(uid).as_bytes())
            .map_err(|e| Error::Storage(format!("remove account created_at: {e}")))?;
        if let Some(active_uid) = get_string(&accounts, K_ACTIVE_UID)? {
            if active_uid == uid {
                accounts
                    .remove(K_ACTIVE_UID)
                    .map_err(|e| Error::Storage(format!("remove active uid: {e}")))?;
            }
        }
        global_db
            .flush()
            .map_err(|e| Error::Storage(format!("flush global db: {e}")))?;
        if self
            .load_current_uid()?
            .as_ref()
            .map(|v| v == uid)
            .unwrap_or(false)
        {
            self.clear_current_uid()?;
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
        let tree = self.account_tree(uid, ACCOUNT_TREE_KV)?;
        tree.insert(key.as_bytes(), value)
            .map_err(|e| Error::Storage(format!("sled kv put: {e}")))?;
        Ok(())
    }

    pub fn kv_get(&self, uid: &str, key: &str) -> Result<Option<Vec<u8>>> {
        let tree = self.account_tree(uid, ACCOUNT_TREE_KV)?;
        let v = tree
            .get(key.as_bytes())
            .map_err(|e| Error::Storage(format!("sled kv get: {e}")))?;
        Ok(v.map(|x| x.to_vec()))
    }

    pub fn kv_delete(&self, uid: &str, key: &str) -> Result<()> {
        let tree = self.account_tree(uid, ACCOUNT_TREE_KV)?;
        tree.remove(key.as_bytes())
            .map_err(|e| Error::Storage(format!("sled kv delete: {e}")))?;
        Ok(())
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
    use crate::{
        LoginResult, NewMessage, UpsertChannelExtraInput, UpsertChannelInput,
        UpsertRemoteMessageInput,
    };
    use rand::RngCore;
    use rusqlite::params;
    use std::path::PathBuf;

    fn test_store() -> LocalStore {
        let mut rand_bytes = [0u8; 6];
        rand::thread_rng().fill_bytes(&mut rand_bytes);
        let dir = PathBuf::from(format!(
            "/tmp/privchat-rust-test-{}-{}",
            chrono::Utc::now().timestamp_micros(),
            hex::encode(rand_bytes)
        ));
        LocalStore::open_at(dir).expect("open test store")
    }

    #[test]
    fn account_kv_path_uses_dot_kv_suffix() {
        let store = test_store();
        let paths = store.storage_paths("20001");
        assert_eq!(
            paths.kv_path.file_name().and_then(|v| v.to_str()),
            Some("account.kv")
        );
    }

    #[test]
    fn db_path_migrates_messages_db_to_privchat_db() {
        let store = test_store();
        let uid = "20000";
        let paths = store.storage_paths(uid);
        std::fs::create_dir_all(&paths.user_root).expect("create user root");
        let legacy = paths.user_root.join("messages.db");
        let conn = rusqlite::Connection::open(&legacy).expect("open legacy db");
        let key = LocalStore::derive_encryption_key(uid);
        conn.pragma_update(None, "key", &key)
            .expect("set legacy db key");
        conn.execute("CREATE TABLE t(id INTEGER PRIMARY KEY)", [])
            .expect("init legacy db");
        drop(conn);

        let out = store.ensure_user_storage(uid).expect("ensure user storage");
        assert_eq!(
            out.db_path.file_name().and_then(|v| v.to_str()),
            Some("privchat.db")
        );
        assert!(out.db_path.exists());
        assert!(!legacy.exists());
    }

    #[test]
    fn session_roundtrip_uses_encrypted_account_store() {
        let store = test_store();
        let uid = "20002";
        let login = LoginResult {
            user_id: uid.parse().expect("uid"),
            token: "token-secret-value".to_string(),
            device_id: "device-a".to_string(),
            refresh_token: Some("refresh-secret-value".to_string()),
            expires_at: 0,
        };
        store.save_login(uid, &login).expect("save login");
        let snap = store
            .load_session(uid)
            .expect("load session")
            .expect("session exists");
        assert_eq!(snap.user_id, login.user_id);
        assert_eq!(snap.token, login.token);
        assert_eq!(snap.device_id, login.device_id);

        let account_db = store.open_account_db(uid).expect("open account db");
        let auth = account_db.open_tree("auth").expect("open auth tree");
        let token_enc = auth.get("access_token_enc").expect("get access enc");
        assert!(token_enc.is_some());
        let plain = token_enc.expect("access enc exists");
        assert_ne!(plain.as_ref(), login.token.as_bytes());
    }

    #[test]
    fn load_session_migrates_legacy_sqlite_row() {
        let store = test_store();
        let uid = "20003";
        let conn = store.conn_for_user(uid).expect("open user db");
        conn.execute(
            "INSERT OR REPLACE INTO auth_session(id, user_id, token, device_id, bootstrap_completed, updated_at)
             VALUES(1, ?1, ?2, ?3, 1, ?4)",
            params![
                uid.parse::<u64>().expect("uid parse"),
                "legacy-token",
                "legacy-device",
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .expect("insert legacy session");

        let snap = store
            .load_session(uid)
            .expect("load session")
            .expect("session exists");
        assert_eq!(snap.token, "legacy-token");
        assert!(snap.bootstrap_completed);

        let remain: i64 = conn
            .query_row("SELECT COUNT(1) FROM auth_session", [], |r| r.get(0))
            .expect("count auth_session");
        assert_eq!(remain, 0);
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
            .create_local_message(uid, &input, 0)
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
        assert_eq!(sent.status, 2);

        let revoked = store
            .set_message_revoke_by_server_message_id(uid, 900001, true, Some(30001))
            .expect("revoke by server message id")
            .expect("message exists");
        assert_eq!(revoked.message_id, message_id);
        assert_eq!(revoked.message_type, 0);
        assert_eq!(revoked.content, "消息已撤回");
        assert!(revoked.revoked);
        assert_eq!(revoked.revoked_by, Some(30001));
    }

    #[test]
    fn mark_message_sent_merges_duplicate_server_row_into_local_row() {
        let store = test_store();
        let uid = "10003-merge";
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

        let local_id = store
            .create_local_message(uid, &input, 123456)
            .expect("create local message");

        let remote_id = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 900001,
                    local_message_id: 0,
                    channel_id: 100,
                    channel_type: 1,
                    timestamp: 1_700_000_000_000,
                    from_uid: 200,
                    message_type: 1,
                    content: "hello".to_string(),
                    status: 2,
                    searchable_word: "hello".to_string(),
                    setting: 0,
                    pts: 1,
                    order_seq: 1,
                    extra: "{}".to_string(),
                },
            )
            .expect("upsert remote row")
            .message_id;
        assert_ne!(local_id, remote_id);

        store
            .mark_message_sent(uid, local_id, 900001)
            .expect("mark sent");

        let all = store
            .list_messages(uid, 100, 1, 10, 0)
            .expect("list messages after merge");
        assert_eq!(all.len(), 1, "duplicate server row should be merged");
        assert_eq!(all[0].message_id, local_id);
        assert_eq!(all[0].server_message_id, Some(900001));
    }

    #[test]
    fn upsert_remote_message_is_idempotent_by_server_message_id() {
        let store = test_store();
        let uid = "10004";

        let first = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 700001,
                    local_message_id: 0,
                    channel_id: 200,
                    channel_type: 1,
                    timestamp: 1_700_000_000_000,
                    from_uid: 12345,
                    message_type: 0,
                    content: "first".to_string(),
                    status: 2,
                    pts: 100,
                    setting: 0,
                    order_seq: 100,
                    searchable_word: "first".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("upsert first")
            .message_id;
        let loaded_first = store
            .get_message_by_id(uid, first)
            .expect("load first")
            .expect("first exists");
        assert_eq!(loaded_first.server_message_id, Some(700001));
        assert_eq!(loaded_first.content, "first");

        let second = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 700001,
                    local_message_id: 111,
                    channel_id: 200,
                    channel_type: 1,
                    timestamp: 1_700_000_000_100,
                    from_uid: 12345,
                    message_type: 0,
                    content: "updated".to_string(),
                    status: 3,
                    pts: 101,
                    setting: 1,
                    order_seq: 101,
                    searchable_word: "updated".to_string(),
                    extra: "{\"k\":1}".to_string(),
                },
            )
            .expect("upsert second")
            .message_id;
        assert_eq!(
            first, second,
            "same server_message_id should update same row"
        );

        let loaded_second = store
            .get_message_by_id(uid, second)
            .expect("load second")
            .expect("second exists");
        assert_eq!(loaded_second.content, "updated");
        assert_eq!(loaded_second.status, 3);
    }

    #[test]
    fn upsert_remote_message_prefers_self_local_message_id_for_merge() {
        let store = test_store();
        let uid = "100001087";
        let local_message_id = 545600001u64;
        let channel_id = 1112u64;

        let local_row_id = store
            .create_local_message(
                uid,
                &NewMessage {
                    channel_id,
                    channel_type: 1,
                    from_uid: 100001087,
                    message_type: 0,
                    content: "pending".to_string(),
                    searchable_word: "pending".to_string(),
                    setting: 0,
                    extra: "{}".to_string(),
                },
                local_message_id,
            )
            .expect("create local self message");

        let merged_row_id = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 9001001,
                    local_message_id,
                    channel_id,
                    channel_type: 1,
                    timestamp: 1_700_000_000_100,
                    from_uid: 100001087,
                    message_type: 0,
                    content: "pending".to_string(),
                    status: 2,
                    pts: 100,
                    setting: 0,
                    order_seq: 100,
                    searchable_word: "pending".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("merge self echo")
            .message_id;

        assert_eq!(
            merged_row_id, local_row_id,
            "must update existing local row"
        );

        let second_device_row_id = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 9001002,
                    local_message_id: 545600099,
                    channel_id,
                    channel_type: 1,
                    timestamp: 1_700_000_000_200,
                    from_uid: 100001087,
                    message_type: 0,
                    content: "from other device".to_string(),
                    status: 2,
                    pts: 101,
                    setting: 0,
                    order_seq: 101,
                    searchable_word: "other".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("insert self message from another device")
            .message_id;

        assert_ne!(
            second_device_row_id, local_row_id,
            "different local_message_id should insert another row"
        );

        let all = store
            .list_messages(uid, channel_id, 1, 50, 0)
            .expect("list messages");
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter()
                .filter(|m| m.server_message_id == Some(9001001))
                .count(),
            1
        );
        assert_eq!(
            all.iter()
                .filter(|m| m.server_message_id == Some(9001002))
                .count(),
            1
        );
    }

    #[test]
    fn upsert_remote_message_ignores_out_of_order_replay() {
        let store = test_store();
        let uid = "10006";

        let row_id = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 800001,
                    local_message_id: 0,
                    channel_id: 300,
                    channel_type: 1,
                    timestamp: 1_700_000_000_500,
                    from_uid: 22334,
                    message_type: 0,
                    content: "newer".to_string(),
                    status: 2,
                    pts: 300,
                    setting: 0,
                    order_seq: 300,
                    searchable_word: "newer".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("insert newer")
            .message_id;

        let same_row = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 800001,
                    local_message_id: 0,
                    channel_id: 300,
                    channel_type: 1,
                    timestamp: 1_700_000_000_100,
                    from_uid: 22334,
                    message_type: 0,
                    content: "older".to_string(),
                    status: 1,
                    pts: 200,
                    setting: 0,
                    order_seq: 200,
                    searchable_word: "older".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("replay older")
            .message_id;
        assert_eq!(row_id, same_row);

        let loaded = store
            .get_message_by_id(uid, row_id)
            .expect("load row")
            .expect("row exists");
        assert_eq!(loaded.content, "newer");
        assert_eq!(loaded.status, 2);
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
            .create_local_message(uid, &input, 0)
            .expect("create message 1");
        input.content = "hello-2".to_string();
        let m2 = store
            .create_local_message(uid, &input, 0)
            .expect("create message 2");
        input.channel_id = 101;
        input.content = "other-channel".to_string();
        let _m3 = store
            .create_local_message(uid, &input, 0)
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
        let uid_num = uid.parse::<u64>().expect("uid parse");
        let channel_id = 500_u64;
        let channel_type = 1_i32;
        store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 950001,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_000,
                    from_uid: 200,
                    message_type: 1,
                    content: "hello-unread".to_string(),
                    status: 2,
                    searchable_word: "hello-unread".to_string(),
                    setting: 0,
                    pts: 100,
                    order_seq: 100,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert message 1");
        store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 950002,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_001,
                    from_uid: uid_num,
                    message_type: 1,
                    content: "self-sent".to_string(),
                    status: 2,
                    searchable_word: "self-sent".to_string(),
                    setting: 0,
                    pts: 101,
                    order_seq: 101,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert self message");

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 0)
            .expect("project unread");
        let unread1 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread");
        assert_eq!(unread1, 0);

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 100)
            .expect("project read to 100");
        let unread2 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread2");
        assert_eq!(unread2, 0);

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 90)
            .expect("project stale 90");
        let unread3 = store
            .get_channel_unread_count(uid, channel_id, channel_type)
            .expect("get unread3");
        assert_eq!(unread3, 0);

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
                    version: 1,
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
                    version: 1,
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
    fn project_channel_read_cursor_is_precise_and_monotonic() {
        let store = test_store();
        let uid = "10007001";
        let uid_num = uid.parse::<u64>().expect("uid parse");
        let channel_id = 700_u64;
        let channel_type = 1_i32;

        let m1 = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 970001,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_001,
                    from_uid: 20001,
                    message_type: 1,
                    content: "m1".to_string(),
                    status: 2,
                    searchable_word: "m1".to_string(),
                    setting: 0,
                    pts: 10,
                    order_seq: 10,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert m1")
            .message_id;
        let m2 = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 970002,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_002,
                    from_uid: 20001,
                    message_type: 1,
                    content: "m2".to_string(),
                    status: 2,
                    searchable_word: "m2".to_string(),
                    setting: 0,
                    pts: 20,
                    order_seq: 20,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert m2")
            .message_id;
        let m3 = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 970003,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_003,
                    from_uid: 20001,
                    message_type: 1,
                    content: "m3".to_string(),
                    status: 2,
                    searchable_word: "m3".to_string(),
                    setting: 0,
                    pts: 30,
                    order_seq: 30,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert m3")
            .message_id;
        let m4_self = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 970004,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_004,
                    from_uid: uid_num,
                    message_type: 1,
                    content: "self".to_string(),
                    status: 2,
                    searchable_word: "self".to_string(),
                    setting: 0,
                    pts: 40,
                    order_seq: 40,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert self")
            .message_id;

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 20)
            .expect("project read pts=20");
        assert_eq!(
            store
                .get_channel_unread_count(uid, channel_id, channel_type)
                .expect("unread after pts=20"),
            1
        );

        let m1_extra = store
            .get_message_extra(uid, m1)
            .expect("extra m1")
            .expect("extra m1 exists");
        let m2_extra = store
            .get_message_extra(uid, m2)
            .expect("extra m2")
            .expect("extra m2 exists");
        let m3_extra = store.get_message_extra(uid, m3).expect("extra m3");
        let m4_extra = store.get_message_extra(uid, m4_self).expect("extra m4");
        assert_eq!(m1_extra.readed, 1);
        assert_eq!(m2_extra.readed, 1);
        assert!(m3_extra.is_none());
        assert!(m4_extra.is_none());

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 10)
            .expect("project stale read pts=10");
        let extra_after_stale = store
            .get_channel_extra(uid, channel_id, channel_type)
            .expect("get extra after stale")
            .expect("channel extra exists");
        assert_eq!(extra_after_stale.keep_pts, 20);
        assert_eq!(
            store
                .get_channel_unread_count(uid, channel_id, channel_type)
                .expect("unread after stale pts=10"),
            1
        );

        store
            .project_channel_read_cursor(uid, channel_id, channel_type, 30)
            .expect("project read pts=30");
        assert_eq!(
            store
                .get_channel_unread_count(uid, channel_id, channel_type)
                .expect("unread after pts=30"),
            0
        );
        let m3_after = store
            .get_message_extra(uid, m3)
            .expect("extra m3 after")
            .expect("extra m3 after exists");
        assert_eq!(m3_after.readed, 1);
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
                    version: 1001,
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
        assert_eq!(row.version, 1001);

        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 9001,
                    channel_type: 2,
                    channel_name: "stale-group".to_string(),
                    channel_remark: "stale-remark".to_string(),
                    avatar: "https://example/stale.png".to_string(),
                    unread_count: 99,
                    top: 0,
                    mute: 1,
                    last_msg_timestamp: 1,
                    last_local_message_id: 1,
                    last_msg_content: "stale".to_string(),
                    version: 1000,
                },
            )
            .expect("upsert stale channel");

        let row = store
            .get_channel_by_id(uid, 9001)
            .expect("get channel after stale upsert")
            .expect("channel still exists");
        assert_eq!(row.channel_name, "group-9001");
        assert_eq!(row.unread_count, 3);
        assert_eq!(row.top, 1);
        assert_eq!(row.mute, 0);
        assert_eq!(row.version, 1001);

        let page = store.list_channels(uid, 20, 0).expect("list channels");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].channel_id, 9001);
    }

    #[test]
    fn channel_reads_self_heal_stale_unread_when_materialized_projection_is_zero() {
        let store = test_store();
        let uid = "10011";
        let uid_num = uid.parse::<u64>().expect("uid parse");
        let channel_id = 9301;
        let channel_type = 1;

        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id,
                    channel_type,
                    channel_name: "dm-9301".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 1,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 1_700_000_000_020,
                    last_local_message_id: 0,
                    last_msg_content: "self-message".to_string(),
                    version: 3001,
                },
            )
            .expect("upsert stale unread channel");
        store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 89301,
                    local_message_id: 0,
                    channel_id,
                    channel_type,
                    timestamp: 1_700_000_000_020,
                    from_uid: uid_num,
                    message_type: 1,
                    content: "{\"content\":\"self-message\"}".to_string(),
                    status: 2,
                    pts: 20,
                    order_seq: 20,
                    searchable_word: "self-message".to_string(),
                    setting: 0,
                    extra: "{}".to_string(),
                },
            )
            .expect("insert self message");
        store
            .upsert_channel_extra(
                uid,
                &UpsertChannelExtraInput {
                    channel_id,
                    channel_type,
                    browse_to: 20,
                    keep_pts: 20,
                    keep_offset_y: 0,
                    draft: String::new(),
                    draft_updated_at: 0,
                },
            )
            .expect("seed keep pts");

        let row = store
            .get_channel_by_id(uid, channel_id)
            .expect("get channel after self-heal")
            .expect("channel exists");
        assert_eq!(row.unread_count, 0);

        let page = store.list_channels(uid, 20, 0).expect("list channels");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].unread_count, 0);

        let persisted = store
            .get_channel_by_id(uid, channel_id)
            .expect("get persisted channel")
            .expect("channel exists");
        assert_eq!(persisted.unread_count, 0);
    }

    #[test]
    fn channel_preview_falls_back_to_synced_fields_without_local_messages() {
        let store = test_store();
        let uid = "10009";
        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 9201,
                    channel_type: 2,
                    channel_name: "group-9201".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 0,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 555666777,
                    last_local_message_id: 0,
                    last_msg_content: "synced-preview".to_string(),
                    version: 2001,
                },
            )
            .expect("upsert synced preview channel");

        let row = store
            .get_channel_by_id(uid, 9201)
            .expect("get channel")
            .expect("channel exists");
        assert_eq!(row.last_msg_timestamp, 555666777);
        assert_eq!(row.last_msg_content, "synced-preview");
        assert_eq!(row.last_local_message_id, 0);

        let page = store.list_channels(uid, 20, 0).expect("list channels");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].last_msg_timestamp, 555666777);
        assert_eq!(page[0].last_msg_content, "synced-preview");
        assert_eq!(page[0].last_local_message_id, 0);
    }

    #[test]
    fn local_message_materialization_overrides_synced_channel_preview() {
        let store = test_store();
        let uid = "10010";
        store
            .upsert_channel(
                uid,
                &UpsertChannelInput {
                    channel_id: 9202,
                    channel_type: 2,
                    channel_name: "group-9202".to_string(),
                    channel_remark: String::new(),
                    avatar: String::new(),
                    unread_count: 0,
                    top: 0,
                    mute: 0,
                    last_msg_timestamp: 111,
                    last_local_message_id: 0,
                    last_msg_content: "stale-synced-preview".to_string(),
                    version: 2002,
                },
            )
            .expect("upsert initial channel");

        let inserted = store
            .upsert_remote_message_with_result(
                uid,
                &UpsertRemoteMessageInput {
                    server_message_id: 88001,
                    local_message_id: 0,
                    channel_id: 9202,
                    channel_type: 2,
                    timestamp: 999888777,
                    from_uid: 10086,
                    message_type: 1,
                    content: "{\"content\":\"fresh-local-message\"}".to_string(),
                    status: 2,
                    pts: 12,
                    setting: 0,
                    order_seq: 12,
                    searchable_word: "fresh local message".to_string(),
                    extra: "{}".to_string(),
                },
            )
            .expect("insert local materialized message");
        let latest = store
            .list_messages(uid, 9202, 2, 10, 0)
            .expect("list messages");
        assert_eq!(latest.len(), 1);

        let row = store
            .get_channel_by_id(uid, 9202)
            .expect("get channel")
            .expect("channel exists");
        assert_eq!(row.last_msg_timestamp, latest[0].created_at);
        assert_eq!(
            row.last_msg_content,
            "{\"content\":\"fresh-local-message\"}"
        );
        assert_eq!(row.last_local_message_id, inserted.message_id);

        let page = store.list_channels(uid, 20, 0).expect("list channels");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].last_msg_timestamp, latest[0].created_at);
        assert_eq!(
            page[0].last_msg_content,
            "{\"content\":\"fresh-local-message\"}"
        );
        assert_eq!(page[0].last_local_message_id, inserted.message_id);
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
                    version: 1000,
                    updated_at: 1000,
                },
            )
            .expect("upsert user");
        let user = store
            .get_user_by_id(uid, 20001)
            .expect("get user")
            .expect("user exists");
        assert_eq!(user.username.as_deref(), Some("alice"));
        assert_eq!(user.version, 1000);
        store
            .upsert_user(
                uid,
                &crate::UpsertUserInput {
                    user_id: 20001,
                    username: Some("stale-alice".to_string()),
                    nickname: Some("Stale Alice".to_string()),
                    alias: Some("stale".to_string()),
                    avatar: "avatar://stale".to_string(),
                    user_type: 9,
                    is_deleted: true,
                    channel_id: "stale-channel".to_string(),
                    version: 999,
                    updated_at: 999,
                },
            )
            .expect("upsert stale user");
        let user = store
            .get_user_by_id(uid, 20001)
            .expect("get user after stale upsert")
            .expect("user exists after stale upsert");
        assert_eq!(user.username.as_deref(), Some("alice"));
        assert_eq!(user.nickname.as_deref(), Some("Alice"));
        assert_eq!(user.alias.as_deref(), Some("A"));
        assert_eq!(user.version, 1000);

        store
            .upsert_friend(
                uid,
                &crate::UpsertFriendInput {
                    user_id: 20001,
                    tags: Some("work".to_string()),
                    is_pinned: true,
                    created_at: 1000,
                    version: 1001,
                    updated_at: 1001,
                },
            )
            .expect("upsert friend");
        let friends = store.list_friends(uid, 20, 0).expect("list friends");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].user_id, 20001);
        assert!(friends[0].is_pinned);
        assert_eq!(friends[0].version, 1001);
        store
            .upsert_friend(
                uid,
                &crate::UpsertFriendInput {
                    user_id: 20001,
                    tags: Some("stale".to_string()),
                    is_pinned: false,
                    created_at: 999,
                    version: 1000,
                    updated_at: 1000,
                },
            )
            .expect("upsert stale friend");
        let friends = store
            .list_friends(uid, 20, 0)
            .expect("list friends after stale upsert");
        assert_eq!(friends.len(), 1);
        assert_eq!(friends[0].tags.as_deref(), Some("work"));
        assert!(friends[0].is_pinned);
        assert_eq!(friends[0].version, 1001);
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
                    version: 1002,
                    updated_at: 1002,
                },
            )
            .expect("upsert group");
        let group = store
            .get_group_by_id(uid, 30001)
            .expect("get group")
            .expect("group exists");
        assert_eq!(group.name.as_deref(), Some("group-a"));
        assert_eq!(group.version, 1002);
        store
            .upsert_group(
                uid,
                &crate::UpsertGroupInput {
                    group_id: 30001,
                    name: Some("stale-group".to_string()),
                    avatar: "avatar://stale-group".to_string(),
                    owner_id: Some(99999),
                    is_dismissed: true,
                    created_at: 999,
                    version: 1001,
                    updated_at: 1001,
                },
            )
            .expect("upsert stale group");
        let group = store
            .get_group_by_id(uid, 30001)
            .expect("get group after stale upsert")
            .expect("group exists after stale upsert");
        assert_eq!(group.name.as_deref(), Some("group-a"));
        assert_eq!(group.owner_id, Some(20001));
        assert!(!group.is_dismissed);
        assert_eq!(group.version, 1002);

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
                    version: 1004,
                    updated_at: 1004,
                },
            )
            .expect("upsert group member");
        let members = store
            .list_group_members(uid, 30001, 20, 0)
            .expect("list group members");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].user_id, 20001);
        assert_eq!(members[0].version, 1004);
        store
            .upsert_group_member(
                uid,
                &crate::UpsertGroupMemberInput {
                    group_id: 30001,
                    user_id: 20001,
                    role: 2,
                    status: 9,
                    alias: Some("stale".to_string()),
                    is_muted: true,
                    joined_at: 1002,
                    version: 1003,
                    updated_at: 1003,
                },
            )
            .expect("upsert stale group member");
        let members = store
            .list_group_members(uid, 30001, 20, 0)
            .expect("list group members after stale upsert");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].role, 0);
        assert_eq!(members[0].status, 0);
        assert_eq!(members[0].alias.as_deref(), Some("owner"));
        assert!(!members[0].is_muted);
        assert_eq!(members[0].version, 1004);
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
            .create_local_message(uid, &input, 0)
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
            .create_local_message(uid, &input, 0)
            .expect("create message");

        store
            .set_message_revoke(uid, message_id, true, Some(30001))
            .expect("set revoke");
        let revoked_row = store
            .get_message_by_id(uid, message_id)
            .expect("get revoked message")
            .expect("message exists");
        assert_eq!(revoked_row.message_type, 0);
        assert_eq!(revoked_row.content, "消息已撤回");
        assert!(revoked_row.revoked);
        assert_eq!(revoked_row.revoked_by, Some(30001));
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
