//! SDK 版本与运行时元信息
//!
//! 设计原则（架构文档）：
//! - **SDK Version** → Cargo.toml（唯一权威源）
//! - **Migration Version** → migrations 文件（文件即版本，由 refinery 自动管理）
//! - **Runtime Metadata** → 本模块

/// SDK semver，来自 Cargo.toml
///
/// 禁止手写版本号，必须用 `env!("CARGO_PKG_VERSION")` 与 Cargo.toml 保持同步。
pub const SDK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// git commit（由 vergen 在 build.rs 中生成）
pub const GIT_SHA: &str = env!("VERGEN_GIT_SHA");

/// build time（由 vergen 在 build.rs 中生成）
pub const BUILD_TIME: &str = env!("VERGEN_BUILD_TIMESTAMP");

/// 当前 SDK 支持的最高数据库 migration 版本（refinery 表 refinery_schema_history 的 version）。
/// 由 build.rs 在编译期扫描 migrations/ 下 V{version}__*.sql 取最大值自动生成，无需手写。
/// 用于启动时校验：若 DB 版本 > 此值则拒绝打开（防 downgrade 导致 schema 不兼容）。
pub const SDK_DB_VERSION: i64 = parse_db_version(env!("SDK_DB_VERSION"));

/// 编译期解析版本号字符串为 i64（build.rs 只会输出纯数字）
const fn parse_db_version(s: &str) -> i64 {
    let b = s.as_bytes();
    let mut v = 0i64;
    let mut i = 0usize;
    while i < b.len() {
        if b[i] >= b'0' && b[i] <= b'9' {
            v = v * 10 + (b[i] - b'0') as i64;
        }
        i += 1;
    }
    v
}
