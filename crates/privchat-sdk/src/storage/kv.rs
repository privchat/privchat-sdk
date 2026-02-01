//! KV 存储模块 - 基于 sled 的高性能键值存储
//! 
//! 本模块提供：
//! - 高性能的键值存储
//! - 用户隔离的命名空间
//! - 原子操作和批量操作
//! - 状态缓存和计数器

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use sled::{Db, Tree};
use serde::{Serialize, Deserialize};
use crate::error::{PrivchatSDKError, Result};
use crate::storage::KvStats;

/// KV 存储组件
#[derive(Debug)]
#[allow(dead_code)]
pub struct KvStore {
    base_path: PathBuf,
    /// 主数据库实例
    db: Arc<Db>,
    /// 用户专属的 Tree 实例
    user_trees: Arc<RwLock<HashMap<String, Tree>>>,
    /// 当前用户ID
    current_user: Arc<RwLock<Option<String>>>,
}

impl KvStore {
    /// 创建新的 KV 存储实例
    pub async fn new(base_path: &Path) -> Result<Self> {
        let base_path = base_path.to_path_buf();
        let kv_path = base_path.join("kv");
        
        // 创建 KV 存储目录
        tokio::fs::create_dir_all(&kv_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建 KV 存储目录失败: {}", e)))?;
        
        // 打开 sled 数据库（切换账号后旧实例可能刚释放锁，重试多次带退避）
        const MAX_OPEN_RETRIES: u32 = 8;
        const RETRY_DELAY_MS: u64 = 300;
        let mut db_opt: Option<sled::Db> = None;
        let mut last_err: Option<sled::Error> = None;
        for attempt in 0..MAX_OPEN_RETRIES {
            match sled::open(&kv_path) {
                Ok(d) => {
                    db_opt = Some(d);
                    break;
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    last_err = Some(e);
                    let is_lock = msg.contains("could not acquire lock")
                        || msg.contains("Resource temporarily unavailable")
                        || msg.contains("WouldBlock");
                    if is_lock && attempt + 1 < MAX_OPEN_RETRIES {
                        let delay_ms = RETRY_DELAY_MS * (1 << attempt);
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    } else {
                        break;
                    }
                }
            }
        }
        let db = db_opt.ok_or_else(|| {
            PrivchatSDKError::KvStore(
                last_err
                    .map(|e| format!("打开 sled 数据库失败: {}", e))
                    .unwrap_or_else(|| "打开 sled 数据库失败".to_string()),
            )
        })?;
        
        Ok(Self {
            base_path,
            db: Arc::new(db),
            user_trees: Arc::new(RwLock::new(HashMap::new())),
            current_user: Arc::new(RwLock::new(None)),
        })
    }
    
    /// 初始化用户 Tree
    pub async fn init_user_tree(&self, uid: &str) -> Result<()> {
        let tree_name = format!("user_{}", uid);
        let tree = self.db.open_tree(&tree_name)
            .map_err(|e| PrivchatSDKError::KvStore(format!("打开用户 Tree 失败: {}", e)))?;
        
        let mut user_trees = self.user_trees.write().await;
        user_trees.insert(uid.to_string(), tree);
        
        tracing::info!("用户 KV Tree 初始化完成: {}", uid);
        
        Ok(())
    }
    
    /// 切换用户
    pub async fn switch_user(&self, uid: &str) -> Result<()> {
        // 如果用户 Tree 不存在，先初始化
        let user_trees = self.user_trees.read().await;
        if !user_trees.contains_key(uid) {
            drop(user_trees);
            self.init_user_tree(uid).await?;
        }
        
        // 更新当前用户
        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());
        
        Ok(())
    }
    
    /// 清理用户数据
    pub async fn cleanup_user_data(&self, uid: &str) -> Result<()> {
        let mut user_trees = self.user_trees.write().await;
        user_trees.remove(uid);
        
        // 删除用户 Tree
        let tree_name = format!("user_{}", uid);
        self.db.drop_tree(&tree_name)
            .map_err(|e| PrivchatSDKError::KvStore(format!("删除用户 Tree 失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 获取当前用户的 Tree
    async fn get_current_tree(&self) -> Result<Tree> {
        let current_user = self.current_user.read().await;
        let uid = current_user.as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        
        let user_trees = self.user_trees.read().await;
        let tree = user_trees.get(uid)
            .ok_or_else(|| PrivchatSDKError::KvStore("用户 Tree 不存在".to_string()))?;
        
        Ok(tree.clone())
    }
    
    /// 设置键值对
    pub async fn set<K, V>(&self, key: K, value: &V) -> Result<()>
    where
        K: AsRef<[u8]>,
        V: Serialize,
    {
        let tree = self.get_current_tree().await?;
        let value_bytes = serde_json::to_vec(value)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化值失败: {}", e)))?;
        
        tree.insert(key, value_bytes)
            .map_err(|e| PrivchatSDKError::KvStore(format!("设置键值对失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 获取键值对
    pub async fn get<K, V>(&self, key: K) -> Result<Option<V>>
    where
        K: AsRef<[u8]>,
        V: for<'de> Deserialize<'de>,
    {
        let tree = self.get_current_tree().await?;
        
        let result = tree.get(key)
            .map_err(|e| PrivchatSDKError::KvStore(format!("获取键值对失败: {}", e)))?;
        
        match result {
            Some(value_bytes) => {
                let value = serde_json::from_slice(&value_bytes)
                    .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化值失败: {}", e)))?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
    
    /// 删除键值对
    pub async fn delete<K>(&self, key: K) -> Result<Option<Vec<u8>>>
    where
        K: AsRef<[u8]>,
    {
        let tree = self.get_current_tree().await?;
        
        let result = tree.remove(key)
            .map_err(|e| PrivchatSDKError::KvStore(format!("删除键值对失败: {}", e)))?;
        
        Ok(result.map(|v| v.to_vec()))
    }
    
    /// 检查键是否存在
    pub async fn exists<K>(&self, key: K) -> Result<bool>
    where
        K: AsRef<[u8]>,
    {
        let tree = self.get_current_tree().await?;
        
        let result = tree.contains_key(key)
            .map_err(|e| PrivchatSDKError::KvStore(format!("检查键存在失败: {}", e)))?;
        
        Ok(result)
    }
    
    /// 批量设置键值对
    pub async fn set_batch<K, V>(&self, pairs: Vec<(K, V)>) -> Result<()>
    where
        K: AsRef<[u8]>,
        V: Serialize,
    {
        let tree = self.get_current_tree().await?;
        let mut batch = sled::Batch::default();
        
        for (key, value) in pairs {
            let value_bytes = serde_json::to_vec(&value)
                .map_err(|e| PrivchatSDKError::Serialization(format!("序列化值失败: {}", e)))?;
            batch.insert(key.as_ref(), value_bytes);
        }
        
        tree.apply_batch(batch)
            .map_err(|e| PrivchatSDKError::KvStore(format!("批量设置失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 获取指定前缀的所有键值对
    pub async fn scan_prefix<V>(&self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, V)>>
    where
        V: for<'de> Deserialize<'de>,
    {
        let tree = self.get_current_tree().await?;
        let mut results = Vec::new();
        
        for result in tree.scan_prefix(prefix) {
            let (key, value_bytes) = result
                .map_err(|e| PrivchatSDKError::KvStore(format!("扫描前缀失败: {}", e)))?;
            
            let value = serde_json::from_slice(&value_bytes)
                .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化值失败: {}", e)))?;
            
            results.push((key.to_vec(), value));
        }
        
        Ok(results)
    }
    
    /// 原子性增加计数器
    pub async fn increment_counter(&self, key: &str, delta: i64) -> Result<i64> {
        let tree = self.get_current_tree().await?;
        
        loop {
            let (current_value, current_bytes) = match tree.get(key)
                .map_err(|e| PrivchatSDKError::KvStore(format!("获取计数器失败: {}", e)))? {
                Some(bytes) => {
                    let value_str = std::str::from_utf8(&bytes)
                        .map_err(|e| PrivchatSDKError::KvStore(format!("计数器值格式错误: {}", e)))?;
                    let value = value_str.parse::<i64>()
                        .map_err(|e| PrivchatSDKError::KvStore(format!("计数器值解析失败: {}", e)))?;
                    (value, Some(bytes))
                }
                None => (0, None),
            };
            
            let new_value = current_value + delta;
            let new_value_bytes = new_value.to_string().into_bytes();
            
            // 使用 compare_and_swap 实现原子性
            let result = tree.compare_and_swap(
                key,
                current_bytes,
                Some(new_value_bytes),
            ).map_err(|e| PrivchatSDKError::KvStore(format!("原子增加失败: {}", e)))?;
            
            match result {
                Ok(_) => return Ok(new_value),
                Err(_) => {
                    // 如果 CAS 失败，重试
                    tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
                    continue;
                }
            }
        }
    }
    
    /// 设置过期时间（通过存储时间戳实现）
    pub async fn set_with_ttl<K, V>(&self, key: K, value: &V, ttl_seconds: u64) -> Result<()>
    where
        K: AsRef<[u8]> + Clone,
        V: Serialize,
    {
        let tree = self.get_current_tree().await?;
        
        // 创建带过期时间的值
        let expired_value = ExpiredValue {
            value: serde_json::to_value(value)
                .map_err(|e| PrivchatSDKError::Serialization(format!("序列化值失败: {}", e)))?,
            expires_at: chrono::Utc::now().timestamp() + ttl_seconds as i64,
        };
        
        let value_bytes = serde_json::to_vec(&expired_value)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化过期值失败: {}", e)))?;
        
        tree.insert(key, value_bytes)
            .map_err(|e| PrivchatSDKError::KvStore(format!("设置带 TTL 的键值对失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 获取带过期时间的键值对
    pub async fn get_with_ttl<K, V>(&self, key: K) -> Result<Option<V>>
    where
        K: AsRef<[u8]> + Clone,
        V: for<'de> Deserialize<'de>,
    {
        let tree = self.get_current_tree().await?;
        
        let result = tree.get(key.clone())
            .map_err(|e| PrivchatSDKError::KvStore(format!("获取带 TTL 的键值对失败: {}", e)))?;
        
        match result {
            Some(value_bytes) => {
                let expired_value: ExpiredValue = serde_json::from_slice(&value_bytes)
                    .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化过期值失败: {}", e)))?;
                
                let now = chrono::Utc::now().timestamp();
                if now > expired_value.expires_at {
                    // 键已过期，删除并返回 None
                    tree.remove(key)
                        .map_err(|e| PrivchatSDKError::KvStore(format!("删除过期键失败: {}", e)))?;
                    return Ok(None);
                }
                
                let value = serde_json::from_value(expired_value.value)
                    .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化值失败: {}", e)))?;
                
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }
    
    /// 清理过期的键值对
    pub async fn cleanup_expired(&self) -> Result<u64> {
        let tree = self.get_current_tree().await?;
        let mut removed_count = 0u64;
        let now = chrono::Utc::now().timestamp();
        
        let mut keys_to_remove = Vec::new();
        
        for result in tree.iter() {
            let (key, value_bytes) = result
                .map_err(|e| PrivchatSDKError::KvStore(format!("遍历键值对失败: {}", e)))?;
            
            // 尝试解析为过期值
            if let Ok(expired_value) = serde_json::from_slice::<ExpiredValue>(&value_bytes) {
                if now > expired_value.expires_at {
                    keys_to_remove.push(key.to_vec());
                }
            }
        }
        
        // 删除过期的键
        for key in keys_to_remove {
            tree.remove(&key)
                .map_err(|e| PrivchatSDKError::KvStore(format!("删除过期键失败: {}", e)))?;
            removed_count += 1;
        }
        
        Ok(removed_count)
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Result<KvStats> {
        let tree = self.get_current_tree().await?;
        
        let key_count = tree.len() as u64;
        // 注意：sled Tree 没有 size_on_disk 方法，我们用一个估算值
        let tree_size = key_count * 256; // 假设每个键值对平均 256 字节
        
        Ok(KvStats {
            tree_size,
            key_count,
            total_keys: key_count,
            storage_size: tree_size,
        })
    }
}

/// 带过期时间的值结构
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExpiredValue {
    value: serde_json::Value,
    expires_at: i64,
}

/// 常用的键前缀常量
pub mod keys {
    /// 网络队列前缀
    pub const NETWORK_QUEUE: &str = "net_queue_";
    /// 最后在线时间前缀
    pub const LAST_ONLINE: &str = "last_online_";
    /// 令牌缓存前缀
    pub const TOKEN_CACHE: &str = "token_cache_";
    /// 计数器前缀
    pub const COUNTER: &str = "counter_";
    /// 用户状态前缀
    pub const USER_STATUS: &str = "user_status_";
    /// 会话状态前缀
    pub const SESSION_STATE: &str = "session_state_";
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use serde_json::json;
    
    #[tokio::test]
    async fn test_kv_store_basic_operations() {
        let temp_dir = TempDir::new().unwrap();
        let store = KvStore::new(temp_dir.path()).await.unwrap();
        
        // 初始化用户 Tree
        store.init_user_tree("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 设置和获取
        let test_data = json!({
            "name": "test",
            "value": 123
        });
        
        store.set("test_key", &test_data).await.unwrap();
        let retrieved: serde_json::Value = store.get("test_key").await.unwrap().unwrap();
        assert_eq!(retrieved, test_data);
        
        // 检查存在性
        assert!(store.exists("test_key").await.unwrap());
        assert!(!store.exists("non_existent_key").await.unwrap());
        
        // 删除
        store.delete("test_key").await.unwrap();
        let deleted: Option<serde_json::Value> = store.get("test_key").await.unwrap();
        assert!(deleted.is_none());
    }
    
    #[tokio::test]
    async fn test_kv_store_batch_operations() {
        let temp_dir = TempDir::new().unwrap();
        let store = KvStore::new(temp_dir.path()).await.unwrap();
        
        store.init_user_tree("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 批量设置
        let pairs = vec![
            ("key1", json!({"value": 1})),
            ("key2", json!({"value": 2})),
            ("key3", json!({"value": 3})),
        ];
        
        store.set_batch(pairs).await.unwrap();
        
        // 验证批量设置
        for i in 1..=3 {
            let key = format!("key{}", i);
            let value: serde_json::Value = store.get(&key).await.unwrap().unwrap();
            assert_eq!(value["value"], i);
        }
        
        // 前缀扫描
        let results: Vec<(Vec<u8>, serde_json::Value)> = store.scan_prefix(b"key").await.unwrap();
        assert_eq!(results.len(), 3);
    }
    
    #[tokio::test]
    async fn test_kv_store_counter() {
        let temp_dir = TempDir::new().unwrap();
        let store = KvStore::new(temp_dir.path()).await.unwrap();
        
        store.init_user_tree("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 测试计数器
        let counter_key = "test_counter";
        
        let result1 = store.increment_counter(counter_key, 5).await.unwrap();
        assert_eq!(result1, 5);
        
        let result2 = store.increment_counter(counter_key, 3).await.unwrap();
        assert_eq!(result2, 8);
        
        let result3 = store.increment_counter(counter_key, -2).await.unwrap();
        assert_eq!(result3, 6);
    }
    
    #[tokio::test]
    async fn test_kv_store_ttl() {
        let temp_dir = TempDir::new().unwrap();
        let store = KvStore::new(temp_dir.path()).await.unwrap();
        
        store.init_user_tree("test_user").await.unwrap();
        store.switch_user("test_user").await.unwrap();
        
        // 设置带 TTL 的值
        let test_data = json!({"message": "这是一个带过期时间的值"});
        store.set_with_ttl("ttl_key", &test_data, 1).await.unwrap();
        
        // 立即获取应该成功
        let retrieved: Option<serde_json::Value> = store.get_with_ttl("ttl_key").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), test_data);
        
        // 等待过期
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
        
        // 再次获取应该返回 None
        let expired: Option<serde_json::Value> = store.get_with_ttl("ttl_key").await.unwrap();
        assert!(expired.is_none());
    }
} 