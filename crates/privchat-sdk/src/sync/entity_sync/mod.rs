//! 实体状态同步 - ENTITY_SYNC_V1
//!
//! 与 PTS 消息流正交，统一 sync_entities 协议 + cursor + applier。
//! 设计见 privchat-docs/design/ENTITY_SYNC_V1.md

mod entity_type;
mod cursor_store;
mod applier;
mod engine;

pub use entity_type::EntityType;
pub use cursor_store::SyncCursorStore;
pub use engine::EntitySyncEngine;
