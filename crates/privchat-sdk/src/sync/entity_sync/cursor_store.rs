//! 同步游标存储 - ENTITY_SYNC_V1 Cursor Key 规范
//!
//! 格式：sync_cursor:{entity_type} 或 sync_cursor:{entity_type}:{scope}

use crate::error::Result;
use crate::storage::kv::KvStore;
use std::sync::Arc;
use super::EntityType;

const PREFIX: &str = "sync_cursor";

/// 存储 (entity_type, scope?) 的 last_version
pub struct SyncCursorStore {
    kv: Arc<KvStore>,
}

impl SyncCursorStore {
    pub fn new(kv: Arc<KvStore>) -> Self {
        Self { kv }
    }

    fn key(entity_type: EntityType, scope: Option<&str>) -> String {
        match scope {
            None => format!("{}:{}", PREFIX, entity_type.as_str()),
            Some(s) => format!("{}:{}:{}", PREFIX, entity_type.as_str(), s),
        }
    }

    pub async fn get(&self, entity_type: EntityType, scope: Option<&str>) -> Result<Option<u64>> {
        let key = Self::key(entity_type, scope);
        self.kv.get::<&str, u64>(key.as_str()).await
    }

    pub async fn set(&self, entity_type: EntityType, scope: Option<&str>, version: u64) -> Result<()> {
        let key = Self::key(entity_type, scope);
        self.kv.set(key.as_str(), &version).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_key_format() {
        // ENTITY_SYNC_V1 规范：sync_cursor:{entity_type} 或 sync_cursor:{entity_type}:{scope}
        assert_eq!(
            SyncCursorStore::key(EntityType::Friend, None),
            "sync_cursor:friend"
        );
        assert_eq!(
            SyncCursorStore::key(EntityType::Group, None),
            "sync_cursor:group"
        );
        assert_eq!(
            SyncCursorStore::key(EntityType::GroupMember, Some("group_123")),
            "sync_cursor:group_member:group_123"
        );
    }
}
