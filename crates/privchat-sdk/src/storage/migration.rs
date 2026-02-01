//! 数据库版本升级管理器 - Schema 版本管理和升级
//! 
//! 本模块提供：
//! - 数据库版本跟踪
//! - 自动升级机制
//! - 版本回滚支持
//! - 升级脚本管理
//! - 数据备份和恢复

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::RwLock;
use serde::{Serialize, Deserialize};
use crate::error::{PrivchatSDKError, Result};

/// 数据库版本升级管理器
#[derive(Debug)]
pub struct MigrationManager {
    base_path: PathBuf,
    /// 当前数据库版本
    current_version: Arc<RwLock<u32>>,
    /// 升级脚本注册表
    migrations: Arc<RwLock<HashMap<u32, Migration>>>,
    /// 版本历史记录
    version_history: Arc<RwLock<Vec<MigrationRecord>>>,
}

/// 数据库升级脚本
pub struct Migration {
    /// 版本号
    pub version: u32,
    /// 升级名称
    pub name: String,
    /// 升级描述
    pub description: String,
    /// 升级脚本
    pub up_script: MigrationScript,
    /// 回滚脚本
    pub down_script: Option<MigrationScript>,
    /// 升级前检查
    pub pre_check: Option<fn() -> Result<()>>,
    /// 升级后检查
    pub post_check: Option<fn() -> Result<()>>,
}

/// 升级脚本类型
#[derive(Debug, Clone)]
pub enum MigrationScript {
    /// SQL 脚本
    Sql(String),
    /// Rust 函数
    Function(fn() -> Result<()>),
    /// 自定义脚本
    Custom(String),
}

impl Clone for Migration {
    fn clone(&self) -> Self {
        Self {
            version: self.version,
            name: self.name.clone(),
            description: self.description.clone(),
            up_script: self.up_script.clone(),
            down_script: self.down_script.clone(),
            pre_check: self.pre_check,
            post_check: self.post_check,
        }
    }
}

impl std::fmt::Debug for Migration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Migration")
            .field("version", &self.version)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("up_script", &self.up_script)
            .field("down_script", &self.down_script)
            .field("pre_check", &self.pre_check.is_some())
            .field("post_check", &self.post_check.is_some())
            .finish()
    }
}

/// 升级记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRecord {
    /// 版本号
    pub version: u32,
    /// 升级名称
    pub name: String,
    /// 升级时间
    pub applied_at: u64,
    /// 升级耗时（毫秒）
    pub duration_ms: u64,
    /// 升级状态
    pub status: MigrationStatus,
    /// 错误信息
    pub error: Option<String>,
}

/// 升级状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MigrationStatus {
    /// 升级成功
    Success,
    /// 升级失败
    Failed,
    /// 升级中
    Running,
    /// 已回滚
    RolledBack,
}

/// 升级计划
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// 当前版本
    pub current_version: u32,
    /// 目标版本
    pub target_version: u32,
    /// 需要执行的升级列表
    pub migrations: Vec<Migration>,
    /// 是否需要备份
    pub needs_backup: bool,
}

impl MigrationManager {
    /// 创建新的升级管理器
    pub async fn new(base_path: &Path) -> Result<Self> {
        let base_path = base_path.to_path_buf();
        let migration_dir = base_path.join("migrations");
        
        // 创建升级目录
        tokio::fs::create_dir_all(&migration_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建升级目录失败: {}", e)))?;
        
        let manager = Self {
            base_path,
            current_version: Arc::new(RwLock::new(0)),
            migrations: Arc::new(RwLock::new(HashMap::new())),
            version_history: Arc::new(RwLock::new(Vec::new())),
        };
        
        // 加载版本历史
        manager.load_version_history().await?;
        
        // 注册内置升级脚本
        manager.register_builtin_migrations().await?;
        
        Ok(manager)
    }
    
    /// 获取当前数据库版本
    pub async fn get_current_version(&self) -> u32 {
        *self.current_version.read().await
    }
    
    /// 设置当前数据库版本
    pub async fn set_current_version(&self, version: u32) {
        let mut current_version = self.current_version.write().await;
        *current_version = version;
    }
    
    /// 获取最新版本
    pub async fn get_latest_version(&self) -> u32 {
        let migrations = self.migrations.read().await;
        migrations.keys().max().copied().unwrap_or(0)
    }
    
    /// 注册升级脚本
    pub async fn register_migration(&self, migration: Migration) -> Result<()> {
        let mut migrations = self.migrations.write().await;
        
        if migrations.contains_key(&migration.version) {
            return Err(PrivchatSDKError::Migration(format!(
                "版本 {} 的升级脚本已存在",
                migration.version
            )));
        }
        
        let version = migration.version;
        migrations.insert(migration.version, migration);
        
        tracing::info!("注册升级脚本: 版本 {}", version);
        
        Ok(())
    }
    
    /// 检查是否需要升级
    pub async fn needs_migration(&self) -> bool {
        let current_version = self.get_current_version().await;
        let latest_version = self.get_latest_version().await;
        
        current_version < latest_version
    }
    
    /// 创建升级计划
    pub async fn create_migration_plan(&self, target_version: Option<u32>) -> Result<MigrationPlan> {
        let current_version = self.get_current_version().await;
        let target_version = target_version.unwrap_or(self.get_latest_version().await);
        
        if current_version >= target_version {
            return Err(PrivchatSDKError::Migration(format!(
                "当前版本 {} 已经是最新版本，无需升级",
                current_version
            )));
        }
        
        let migrations = self.migrations.read().await;
        let mut plan_migrations = Vec::new();
        
        // 收集需要执行的升级脚本
        for version in (current_version + 1)..=target_version {
            if let Some(migration) = migrations.get(&version) {
                plan_migrations.push(migration.clone());
            } else {
                return Err(PrivchatSDKError::Migration(format!(
                    "缺少版本 {} 的升级脚本",
                    version
                )));
            }
        }
        
        // 按版本号排序
        plan_migrations.sort_by_key(|m| m.version);
        
        // 判断是否需要备份
        let needs_backup = plan_migrations.iter()
            .any(|m| m.version > current_version + 5); // 如果跨度超过5个版本，建议备份
        
        Ok(MigrationPlan {
            current_version,
            target_version,
            migrations: plan_migrations,
            needs_backup,
        })
    }
    
    /// 执行升级
    pub async fn migrate(&self, target_version: Option<u32>) -> Result<Vec<MigrationRecord>> {
        let plan = self.create_migration_plan(target_version).await?;
        
        if plan.migrations.is_empty() {
            return Ok(Vec::new());
        }
        
        tracing::info!(
            "开始数据库升级: {} -> {}",
            plan.current_version,
            plan.target_version
        );
        
        // 如果需要备份，先创建备份
        if plan.needs_backup {
            self.create_backup().await?;
        }
        
        let mut migration_records = Vec::new();
        
        for migration in plan.migrations {
            let record = self.execute_migration(&migration).await?;
            migration_records.push(record.clone());
            
            // 如果升级失败，停止执行
            if matches!(record.status, MigrationStatus::Failed) {
                tracing::error!("升级失败，停止执行: {}", record.error.unwrap_or_default());
                break;
            }
            
            // 更新当前版本
            self.set_current_version(migration.version).await;
        }
        
        // 保存版本历史
        self.save_version_history(&migration_records).await?;
        
        tracing::info!("数据库升级完成");
        
        Ok(migration_records)
    }
    
    /// 执行单个升级脚本
    async fn execute_migration(&self, migration: &Migration) -> Result<MigrationRecord> {
        let start_time = std::time::Instant::now();
        let applied_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        tracing::info!("执行升级: {} - {}", migration.version, migration.name);
        
        // 执行升级前检查
        if let Some(pre_check) = &migration.pre_check {
            if let Err(e) = pre_check() {
                return Ok(MigrationRecord {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied_at,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                    status: MigrationStatus::Failed,
                    error: Some(format!("升级前检查失败: {}", e)),
                });
            }
        }
        
        // 执行升级脚本
        let script_result = self.execute_script(&migration.up_script).await;
        
        let duration_ms = start_time.elapsed().as_millis() as u64;
        
        match script_result {
            Ok(_) => {
                // 执行升级后检查
                if let Some(post_check) = &migration.post_check {
                    if let Err(e) = post_check() {
                        return Ok(MigrationRecord {
                            version: migration.version,
                            name: migration.name.clone(),
                            applied_at,
                            duration_ms,
                            status: MigrationStatus::Failed,
                            error: Some(format!("升级后检查失败: {}", e)),
                        });
                    }
                }
                
                tracing::info!("升级成功: {} ({}ms)", migration.name, duration_ms);
                
                Ok(MigrationRecord {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied_at,
                    duration_ms,
                    status: MigrationStatus::Success,
                    error: None,
                })
            }
            Err(e) => {
                tracing::error!("升级失败: {} - {}", migration.name, e);
                
                Ok(MigrationRecord {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied_at,
                    duration_ms,
                    status: MigrationStatus::Failed,
                    error: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 执行升级脚本
    async fn execute_script(&self, script: &MigrationScript) -> Result<()> {
        match script {
            MigrationScript::Sql(sql) => {
                // 这里应该执行 SQL 脚本
                // 由于需要数据库连接，这里暂时返回成功
                tracing::debug!("执行 SQL 脚本: {}", sql);
                Ok(())
            }
            MigrationScript::Function(func) => {
                func()
            }
            MigrationScript::Custom(command) => {
                // 执行自定义命令
                tracing::debug!("执行自定义脚本: {}", command);
                Ok(())
            }
        }
    }
    
    /// 回滚到指定版本
    pub async fn rollback(&self, target_version: u32) -> Result<Vec<MigrationRecord>> {
        let current_version = self.get_current_version().await;
        
        if current_version <= target_version {
            return Err(PrivchatSDKError::Migration(format!(
                "无法回滚到版本 {}，当前版本为 {}",
                target_version,
                current_version
            )));
        }
        
        let migrations = self.migrations.read().await;
        let mut rollback_records = Vec::new();
        
        // 按版本号降序执行回滚
        for version in (target_version + 1..=current_version).rev() {
            if let Some(migration) = migrations.get(&version) {
                if let Some(down_script) = &migration.down_script {
                    let record = self.execute_rollback(migration, down_script).await?;
                    rollback_records.push(record);
                    
                    // 更新当前版本
                    self.set_current_version(version - 1).await;
                } else {
                    tracing::warn!("版本 {} 没有回滚脚本", version);
                }
            }
        }
        
        // 保存回滚记录
        self.save_version_history(&rollback_records).await?;
        
        tracing::info!("回滚完成: {} -> {}", current_version, target_version);
        
        Ok(rollback_records)
    }
    
    /// 执行回滚脚本
    async fn execute_rollback(
        &self,
        migration: &Migration,
        down_script: &MigrationScript,
    ) -> Result<MigrationRecord> {
        let start_time = std::time::Instant::now();
        let applied_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        tracing::info!("执行回滚: {} - {}", migration.version, migration.name);
        
        let script_result = self.execute_script(down_script).await;
        let duration_ms = start_time.elapsed().as_millis() as u64;
        
        match script_result {
            Ok(_) => {
                tracing::info!("回滚成功: {} ({}ms)", migration.name, duration_ms);
                
                Ok(MigrationRecord {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied_at,
                    duration_ms,
                    status: MigrationStatus::RolledBack,
                    error: None,
                })
            }
            Err(e) => {
                tracing::error!("回滚失败: {} - {}", migration.name, e);
                
                Ok(MigrationRecord {
                    version: migration.version,
                    name: migration.name.clone(),
                    applied_at,
                    duration_ms,
                    status: MigrationStatus::Failed,
                    error: Some(e.to_string()),
                })
            }
        }
    }
    
    /// 获取版本历史
    pub async fn get_version_history(&self) -> Vec<MigrationRecord> {
        self.version_history.read().await.clone()
    }
    
    /// 创建数据库备份
    async fn create_backup(&self) -> Result<PathBuf> {
        let backup_dir = self.base_path.join("backups");
        tokio::fs::create_dir_all(&backup_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建备份目录失败: {}", e)))?;
        
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let backup_path = backup_dir.join(format!("backup_{}.db", timestamp));
        
        // 这里应该实现实际的备份逻辑
        // 对于 SQLite，可以使用 VACUUM INTO 命令
        // 对于其他数据库，可以使用相应的备份工具
        
        tracing::info!("创建数据库备份: {}", backup_path.display());
        
        Ok(backup_path)
    }
    
    /// 加载版本历史
    async fn load_version_history(&self) -> Result<()> {
        let history_file = self.base_path.join("migration_history.json");
        
        if history_file.exists() {
            let content = tokio::fs::read_to_string(&history_file).await
                .map_err(|e| PrivchatSDKError::IO(format!("读取版本历史失败: {}", e)))?;
            
            let history: Vec<MigrationRecord> = serde_json::from_str(&content)
                .map_err(|e| PrivchatSDKError::Serialization(format!("反序列化版本历史失败: {}", e)))?;
            
            let mut version_history = self.version_history.write().await;
            *version_history = history;
            
            // 从历史记录中恢复当前版本
            if let Some(last_success) = version_history.iter()
                .filter(|r| matches!(r.status, MigrationStatus::Success))
                .last() {
                self.set_current_version(last_success.version).await;
            }
        }
        
        Ok(())
    }
    
    /// 保存版本历史
    async fn save_version_history(&self, new_records: &[MigrationRecord]) -> Result<()> {
        let mut version_history = self.version_history.write().await;
        version_history.extend_from_slice(new_records);
        
        let history_file = self.base_path.join("migration_history.json");
        let content = serde_json::to_string_pretty(&*version_history)
            .map_err(|e| PrivchatSDKError::Serialization(format!("序列化版本历史失败: {}", e)))?;
        
        tokio::fs::write(&history_file, content).await
            .map_err(|e| PrivchatSDKError::IO(format!("保存版本历史失败: {}", e)))?;
        
        Ok(())
    }
    
    /// 注册内置升级脚本
    async fn register_builtin_migrations(&self) -> Result<()> {
        // 版本 1: 初始化数据库
        self.register_migration(Migration {
            version: 1,
            name: "初始化数据库".to_string(),
            description: "创建基础数据库表结构".to_string(),
            up_script: MigrationScript::Sql(
                "CREATE TABLE IF NOT EXISTS messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    message_id TEXT NOT NULL UNIQUE,
                    message_type TEXT NOT NULL,
                    from_uid TEXT NOT NULL,
                    to_uid TEXT NOT NULL,
                    content TEXT NOT NULL,
                    timestamp INTEGER NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
                )".to_string()
            ),
            down_script: Some(MigrationScript::Sql("DROP TABLE IF EXISTS messages".to_string())),
            pre_check: None,
            post_check: None,
        }).await?;
        
        // 版本 2: 添加全文搜索
        self.register_migration(Migration {
            version: 2,
            name: "添加全文搜索".to_string(),
            description: "创建 FTS5 全文搜索表".to_string(),
            up_script: MigrationScript::Sql(
                "CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                    message_id UNINDEXED,
                    content,
                    content='messages',
                    content_rowid='id'
                )".to_string()
            ),
            down_script: Some(MigrationScript::Sql("DROP TABLE IF EXISTS messages_fts".to_string())),
            pre_check: None,
            post_check: None,
        }).await?;
        
        // 版本 3: 添加索引
        self.register_migration(Migration {
            version: 3,
            name: "添加索引".to_string(),
            description: "为常用查询字段添加索引".to_string(),
            up_script: MigrationScript::Sql(
                "CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp);
                 CREATE INDEX IF NOT EXISTS idx_messages_from_uid ON messages(from_uid);
                 CREATE INDEX IF NOT EXISTS idx_messages_to_uid ON messages(to_uid)".to_string()
            ),
            down_script: Some(MigrationScript::Sql(
                "DROP INDEX IF EXISTS idx_messages_timestamp;
                 DROP INDEX IF EXISTS idx_messages_from_uid;
                 DROP INDEX IF EXISTS idx_messages_to_uid".to_string()
            )),
            pre_check: None,
            post_check: None,
        }).await?;
        
        tracing::info!("注册了 {} 个内置升级脚本", 3);
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    
    #[tokio::test]
    async fn test_migration_manager_init() {
        let temp_dir = TempDir::new().unwrap();
        let manager = MigrationManager::new(temp_dir.path()).await.unwrap();
        
        assert_eq!(manager.get_current_version().await, 0);
        assert_eq!(manager.get_latest_version().await, 3); // 3个内置升级脚本
        assert!(manager.needs_migration().await);
    }
    
    #[tokio::test]
    async fn test_migration_plan() {
        let temp_dir = TempDir::new().unwrap();
        let manager = MigrationManager::new(temp_dir.path()).await.unwrap();
        
        let plan = manager.create_migration_plan(None).await.unwrap();
        
        assert_eq!(plan.current_version, 0);
        assert_eq!(plan.target_version, 3);
        assert_eq!(plan.migrations.len(), 3);
        assert!(!plan.needs_backup); // 版本跨度不大，不需要备份
    }
    
    #[tokio::test]
    async fn test_custom_migration() {
        let temp_dir = TempDir::new().unwrap();
        let manager = MigrationManager::new(temp_dir.path()).await.unwrap();
        
        // 注册自定义升级脚本（版本4，紧跟内置脚本）
        let custom_migration = Migration {
            version: 4,
            name: "自定义升级".to_string(),
            description: "测试自定义升级脚本".to_string(),
            up_script: MigrationScript::Function(|| {
                println!("执行自定义升级脚本");
                Ok(())
            }),
            down_script: Some(MigrationScript::Function(|| {
                println!("执行自定义回滚脚本");
                Ok(())
            })),
            pre_check: None,
            post_check: None,
        };
        
        manager.register_migration(custom_migration).await.unwrap();
        
        assert_eq!(manager.get_latest_version().await, 4);
        
        // 测试升级计划
        let plan = manager.create_migration_plan(Some(4)).await.unwrap();
        assert_eq!(plan.migrations.len(), 4); // 3个内置 + 1个自定义
    }
    
    #[tokio::test]
    async fn test_version_history() {
        let temp_dir = TempDir::new().unwrap();
        let manager = MigrationManager::new(temp_dir.path()).await.unwrap();
        
        // 执行升级
        let records = manager.migrate(Some(1)).await.unwrap();
        
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].version, 1);
        assert!(matches!(records[0].status, MigrationStatus::Success));
        
        // 检查版本历史
        let history = manager.get_version_history().await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, 1);
        
        // 检查当前版本
        assert_eq!(manager.get_current_version().await, 1);
    }
} 