//! 数据库迁移管理 - 处理数据库版本升级和 schema 变更
//! 
//! 功能包括：
//! - 数据库版本管理
//! - 自动迁移执行
//! - 迁移回滚
//! - 版本兼容性检查

use rusqlite::{Connection, params};
use crate::error::{Result, PrivchatSDKError};
use std::path::Path;
use std::fs;
use std::collections::BTreeMap;

/// 数据库迁移管理器
pub struct MigrationDao<'a> {
    conn: &'a Connection,
}

impl<'a> MigrationDao<'a> {
    /// 创建新的 MigrationDao 实例
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
    
    /// 从外部 assets/ 目录初始化数据库（运行时读取）
    pub fn initialize_database_from_assets(&self, assets_path: &Path) -> Result<()> {
        // 读取 assets/ 目录下的所有 SQL 文件
        let migration_files = self.discover_migration_files(assets_path)?;
        
        if migration_files.is_empty() {
            return Err(PrivchatSDKError::Database("未找到任何可执行的 SQL 迁移文件".to_string()));
        }
        
        tracing::info!("开始初始化数据库，共发现 {} 个 SQL 文件", migration_files.len());
        
        // 首先确保版本表存在
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version TEXT PRIMARY KEY,
                applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // 按时间顺序执行 SQL 文件
        for (version, file_path) in migration_files {
            tracing::info!("执行数据库初始化脚本: {} ({})", version, file_path.display());
            
            let sql_content = fs::read_to_string(&file_path)
                .map_err(|e| PrivchatSDKError::IO(format!("读取 SQL 文件失败 {}: {}", file_path.display(), e)))?;
            
            // 直接执行 SQL 而不使用事务（保持简单）
            self.conn.execute_batch(&sql_content)
                .map_err(|e| PrivchatSDKError::Database(format!("执行 SQL 文件失败 {}: {}", file_path.display(), e)))?;
            
            // 记录迁移版本
            self.record_migration_version(&version)?;
            
            tracing::info!("成功执行初始化脚本: {}", version);
        }
        
        tracing::info!("数据库初始化完成");
        Ok(())
    }
    
    /// 发现迁移文件，按时间顺序排序
    fn discover_migration_files(&self, assets_path: &Path) -> Result<BTreeMap<String, std::path::PathBuf>> {
        let mut migration_files = BTreeMap::new();
        
        if !assets_path.exists() {
            return Err(PrivchatSDKError::IO(format!("Assets 目录不存在: {}", assets_path.display())));
        }
        
        let entries = fs::read_dir(assets_path)
            .map_err(|e| PrivchatSDKError::IO(format!("读取 assets 目录失败: {}", e)))?;
        
        for entry in entries {
            let entry = entry.map_err(|e| PrivchatSDKError::IO(format!("遍历 assets 目录失败: {}", e)))?;
            let path = entry.path();
            
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    // 检查是否是 SQL 文件，并且文件名符合时间格式
                    if file_name.ends_with(".sql") && self.is_valid_migration_filename(file_name) {
                        let version = file_name.trim_end_matches(".sql").to_string();
                        migration_files.insert(version, path);
                    }
                }
            }
        }
        
        if migration_files.is_empty() {
            return Err(PrivchatSDKError::Database("未找到有效的迁移文件".to_string()));
        }
        
        Ok(migration_files)
    }
    
    /// 验证迁移文件名是否符合时间格式 (YYYYMMDDHHMMSS)
    fn is_valid_migration_filename(&self, filename: &str) -> bool {
        let name_without_ext = filename.trim_end_matches(".sql");
        
        // 检查长度 (YYYYMMDDHHMMSS = 14位) 或 (YYYYMMDD = 8位)
        if name_without_ext.len() != 14 && name_without_ext.len() != 8 {
            return false;
        }
        
        // 检查是否全为数字
        name_without_ext.chars().all(|c| c.is_ascii_digit())
    }
    
    /// 记录迁移版本
    fn record_migration_version(&self, version: &str) -> Result<()> {
        // 确保版本表存在
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version TEXT PRIMARY KEY,
                applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )",
            [],
        )?;
        
        // 插入版本记录
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_version (version) VALUES (?1)",
            params![version],
        )?;
        
        Ok(())
    }
    
    /// 获取当前数据库版本
    pub fn get_current_version(&self) -> Result<Option<String>> {
        // 检查版本表是否存在
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_version')",
            [],
            |row| row.get(0)
        )?;
        
        if !table_exists {
            return Ok(None);
        }
        
        // 获取最新版本
        let version: Result<String> = self.conn.query_row(
            "SELECT version FROM schema_version ORDER BY applied_at DESC LIMIT 1",
            [],
            |row| row.get(0)
        ).map_err(|e| PrivchatSDKError::Database(format!("获取版本失败: {}", e)));
        
        match version {
            Ok(v) => Ok(Some(v)),
            Err(_) => Ok(None),
        }
    }
    
    /// 检查是否需要迁移
    pub fn needs_migration(&self) -> Result<bool> {
        let current_version = self.get_current_version()?;
        let target_version = "20240101000001";
        
        match current_version {
            Some(version) => Ok(version != target_version),
            None => Ok(true), // 没有版本信息，需要初始化
        }
    }
    
    /// 执行迁移
    pub fn migrate(&self) -> Result<()> {
        if !self.needs_migration()? {
            return Ok(());
        }
        
        let current_version = self.get_current_version()?;
        
        match current_version {
            Some(_) => {
                // 已有数据库，需要执行增量迁移
                self.migrate_incremental()?;
            }
            None => {
                // 全新数据库：请使用 DbActor 的 Migration 系统或 migrate_from_assets
                return Err(PrivchatSDKError::Database(
                    "全新数据库请使用 DbActor 的 Migration 系统或 MigrationDao::migrate_from_assets".to_string()
                ));
            }
        }
        
        Ok(())
    }
    
    /// 使用外部 assets/ 目录执行迁移
    pub fn migrate_from_assets(&self, assets_path: &Path) -> Result<()> {
        let current_version = self.get_current_version()?;
        
        match current_version {
            Some(version) => {
                // 已有数据库，检查是否需要增量迁移
                tracing::info!("当前数据库版本: {}", version);
                self.migrate_incremental_from_assets(assets_path, &version)?;
            }
            None => {
                // 全新数据库，使用外部文件初始化
                tracing::info!("初始化全新数据库");
                self.initialize_database_from_assets(assets_path)?;
            }
        }
        
        Ok(())
    }
    
    /// 从外部 assets/ 目录执行增量迁移
    fn migrate_incremental_from_assets(&self, assets_path: &Path, current_version: &str) -> Result<()> {
        let all_migration_files = self.discover_migration_files(assets_path)?;
        
        // 找到比当前版本新的文件（时间戳字符串比较）
        let mut new_migrations = Vec::new();
        for (version, file_path) in all_migration_files {
            // 字符串比较可以直接用于时间戳格式 YYYYMMDDHHMMSS
            if version.as_str() > current_version {
                new_migrations.push((version, file_path));
            }
        }
        
        if new_migrations.is_empty() {
            tracing::info!("没有需要执行的新迁移文件（当前版本: {}）", current_version);
            return Ok(());
        }
        
        tracing::info!("发现 {} 个新的迁移文件需要执行", new_migrations.len());
        
        // 执行新的迁移（BTreeMap 已经保证了按时间顺序排序）
        for (version, file_path) in new_migrations {
            tracing::info!("执行增量迁移: {} -> {}", current_version, version);
            
            let sql_content = fs::read_to_string(&file_path)
                .map_err(|e| PrivchatSDKError::IO(format!("读取 SQL 文件失败 {}: {}", file_path.display(), e)))?;
            
            // 直接执行 SQL 而不使用事务（保持简单）
            self.conn.execute_batch(&sql_content)
                .map_err(|e| PrivchatSDKError::Database(format!("执行 SQL 文件失败 {}: {}", file_path.display(), e)))?;
            
            // 记录迁移版本
            self.record_migration_version(&version)?;
            
            tracing::info!("成功执行迁移: {} (文件: {})", version, file_path.display());
        }
        
        Ok(())
    }
    
    /// 执行增量迁移
    fn migrate_incremental(&self) -> Result<()> {
        // 当前没有增量迁移需要执行
        // 如果将来有新版本，在这里添加增量迁移逻辑
        // 例如：ALTER TABLE, CREATE INDEX 等
        
        // 目前只有一个版本 (20240101000001)，所以不需要执行任何操作
        // 数据库结构已经存在且正确
        
        tracing::debug!("数据库已是最新版本，无需增量迁移");
        Ok(())
    }
    
    /// 验证数据库结构完整性
    pub fn validate_schema(&self) -> Result<bool> {
        // 检查关键表是否存在
        let required_tables = vec![
            "message",
            "channel",
            "channel_member",
            "message_extra",
            "message_reaction",
            "reminder",
            "robot",
            "robot_menu",
            "channel_extra",
            "user",
            "group",
            "group_member",
            "friend",
            "schema_version"
        ];
        
        for table in required_tables {
            let exists: bool = self.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                params![table],
                |row| row.get(0)
            )?;
            
            if !exists {
                return Ok(false);
            }
        }
        
        Ok(true)
    }
    
    /// 获取数据库统计信息
    pub fn get_database_stats(&self) -> Result<DatabaseStats> {
        let message_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM message WHERE is_deleted = 0",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        let channel_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM channel WHERE is_deleted = 0",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        let total_size: i64 = self.conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |row| row.get(0)
        ).unwrap_or(0);
        
        Ok(DatabaseStats {
            message_count,
            channel_count,
            total_size,
        })
    }
}

/// 数据库统计信息
#[derive(Debug, Clone)]
pub struct DatabaseStats {
    pub message_count: i64,
    pub channel_count: i64,
    pub total_size: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    
    #[test]
    fn test_migration_stats() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (version TEXT PRIMARY KEY, applied_at DATETIME DEFAULT CURRENT_TIMESTAMP)",
            [],
        ).unwrap();
        conn.execute("CREATE TABLE IF NOT EXISTS message (id INTEGER PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE IF NOT EXISTS channel (id INTEGER PRIMARY KEY)", []).unwrap();
        let migration = MigrationDao::new(&conn);
        
        let stats = migration.get_database_stats().unwrap();
        assert_eq!(stats.message_count, 0);
        assert_eq!(stats.channel_count, 0);
        assert!(stats.total_size > 0);
    }
}
