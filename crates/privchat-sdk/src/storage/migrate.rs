//! 数据库迁移与初始化 - 由 refinery 自动管理
//!
//! 设计原则：
//! - Migration 版本 = migrations 文件顺序，无需手写 BUILTIN_MIGRATIONS。
//! - 统一入口 `init_db`：pragmas → migrate → 版本校验，避免多处初始化或忘记 migrate。
//! - 新增迁移只需在 migrations/ 添加 V{n}__{name}.sql，编译期自动嵌入、自动执行。

mod embedded {
    use refinery::embed_migrations;

    embed_migrations!("./migrations");
}

use rusqlite::Connection;

use crate::error::{PrivchatSDKError, Result};
use crate::version::SDK_DB_VERSION;

/// refinery 使用的 migration 历史表名（与 refinery 默认一致，用于版本校验）
const REFINERY_TABLE: &str = "refinery_schema_history";

/// IM 场景推荐 PRAGMA：WAL、NORMAL 同步、外键、内存临时表、256MB mmap。
const IM_PRAGMAS: &str = "
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
PRAGMA temp_store=MEMORY;
PRAGMA mmap_size=268435456;
";

/// 开启 IM 必备 pragmas（写入性能、崩溃安全、少锁冲突）。
pub fn enable_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(IM_PRAGMAS.trim())
        .map_err(|e| PrivchatSDKError::Database(format!("设置 PRAGMA 失败: {}", e)))?;
    Ok(())
}

/// 执行内置 migrations（编译期嵌入，自动按版本顺序执行）。
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    embedded::migrations::runner()
        .run(conn)
        .map_err(|e| PrivchatSDKError::Database(format!("执行 migration 失败: {}", e)))?;
    Ok(())
}

/// 读取 refinery 表中当前数据库的 migration 版本；无表或空表返回 None。
fn get_db_migration_version(conn: &Connection) -> Result<Option<i64>> {
    let exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
        [REFINERY_TABLE],
        |row| row.get(0),
    ).map_err(|e| PrivchatSDKError::Database(format!("查询 {} 失败: {}", REFINERY_TABLE, e)))?;

    if !exists {
        return Ok(None);
    }

    let version: Option<i64> = conn.query_row(
        &format!("SELECT MAX(version) FROM {}", REFINERY_TABLE),
        [],
        |row| row.get::<_, Option<i64>>(0),
    ).map_err(|e| PrivchatSDKError::Database(format!("读取 migration 版本失败: {}", e)))?;

    Ok(version.filter(|&v| v > 0))
}

/// 强制版本校验：若 DB 版本 > 当前 SDK 支持的最高版本，拒绝使用（防 downgrade 后 schema 不兼容）。
fn check_db_version(conn: &Connection) -> Result<()> {
    let db_version = get_db_migration_version(conn)?;
    let Some(v) = db_version else { return Ok(()); };
    if v > SDK_DB_VERSION {
        return Err(PrivchatSDKError::Database(format!(
            "数据库版本 {} 高于当前 SDK 支持的最高版本 {}，请升级 SDK 后再打开",
            v, SDK_DB_VERSION
        )));
    }
    Ok(())
}

/// 统一初始化入口：先开 pragmas，再执行 migrations，最后做版本校验。
/// 调用方只需在打开连接并设置完加密等后调用一次，避免忘记 migrate 或多处初始化。
pub fn init_db(conn: &mut Connection) -> Result<()> {
    enable_pragmas(conn)?;
    run_migrations(conn)?;
    check_db_version(conn)?;
    Ok(())
}
