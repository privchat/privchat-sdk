use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;
use tokio;
use tracing;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    println!("=== PrivChat SDK 存储管理器示例 ===\n");
    
    // 示例1：使用嵌入式 SQL 初始化数据库
    println!("📁 示例1: 使用嵌入式 SQL 初始化数据库");
    let user_data_dir = PathBuf::from("./test_data");
    let storage_manager = StorageManager::new_simple(&user_data_dir).await?;
    
    // 初始化用户数据库（使用嵌入式 SQL）
    let uid = "user123";
    storage_manager.init_user(uid).await?;
    storage_manager.switch_user(uid).await?;
    
    println!("✅ 用户数据库初始化完成: {}", uid);
    println!("📍 用户数据目录: {}", storage_manager.user_dir(uid).display());
    println!("📍 基础数据目录: {}", storage_manager.base_path().display());
    println!();
    
    // 示例2：使用预设的外部 assets 目录初始化数据库（智能缓存）
    println!("📁 示例2: 使用预设的外部 assets 目录初始化数据库（智能缓存）");
    let user_data_dir = PathBuf::from("./test_data2");
    let assets_dir = PathBuf::from("./assets");
    
    // 先创建示例迁移文件
    create_sample_migration_files(&assets_dir).await?;
    
    let storage_manager2 = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await?;
    
    // 初始化用户数据库（使用预设的外部 assets 目录）
    let uid2 = "user456";
    
    // 第一次初始化 - 会扫描 assets 目录
    println!("🔍 第一次初始化 - 扫描 assets 目录");
    storage_manager2.init_user_with_assets(uid2).await?;
    storage_manager2.switch_user(uid2).await?;
    
    // 查看缓存信息
    if let Some(cache_info) = storage_manager2.get_assets_cache_info().await? {
        println!("📋 缓存信息:");
        println!("   SDK 版本: {}", cache_info.sdk_version);
        println!("   Assets 路径: {}", cache_info.assets_path);
        println!("   文件数量: {}", cache_info.file_timestamps.len());
        for (filename, timestamp) in &cache_info.file_timestamps {
            println!("   文件: {} (时间戳: {})", filename, timestamp);
        }
    }
    
    // 第二次初始化 - 应该使用缓存，跳过扫描
    println!("\n🚀 第二次初始化 - 使用缓存，跳过扫描");
    storage_manager2.init_user_with_assets(uid2).await?;
    
    println!("✅ 用户数据库初始化完成: {}", uid2);
    println!("📍 用户数据目录: {}", storage_manager2.user_dir(uid2).display());
    println!("📍 基础数据目录: {}", storage_manager2.base_path().display());
    println!("📍 Assets 目录: {}", storage_manager2.assets_path().unwrap().display());
    println!();
    
    // 示例3：演示缓存管理
    println!("📁 示例3: 演示缓存管理");
    
    // 清理缓存
    println!("🧹 清理 assets 缓存");
    storage_manager2.clear_assets_cache().await?;
    
    // 再次初始化应该重新扫描
    println!("🔍 清理缓存后再次初始化 - 重新扫描");
    storage_manager2.init_user_with_assets(uid2).await?;
    
    // 手动刷新缓存
    println!("🔄 手动刷新缓存");
    storage_manager2.refresh_assets_cache(&assets_dir).await?;
    
    println!("✅ 缓存管理演示完成");
    println!();
    
    // 示例4：使用自定义 assets 目录
    println!("📁 示例4: 使用自定义 assets 目录");
    let user_data_dir = PathBuf::from("./test_data3");
    let storage_manager3 = StorageManager::new_simple(&user_data_dir).await?;
    
    // 初始化用户数据库（使用自定义 assets 目录）
    let uid3 = "user789";
    let custom_assets_dir = PathBuf::from("./custom_assets");
    create_sample_migration_files(&custom_assets_dir).await?;
    
    storage_manager3.init_user_with_custom_assets(uid3, &custom_assets_dir).await?;
    storage_manager3.switch_user(uid3).await?;
    
    println!("✅ 用户数据库初始化完成: {}", uid3);
    println!("📍 用户数据目录: {}", storage_manager3.user_dir(uid3).display());
    println!("📍 基础数据目录: {}", storage_manager3.base_path().display());
    println!("📍 使用的 Assets 目录: {}", custom_assets_dir.display());
    println!();
    
    // 示例5：演示文件变化检测
    println!("📁 示例5: 演示文件变化检测");
    
    // 添加新的迁移文件
    let new_migration = r#"
-- 新增的迁移文件
CREATE TABLE IF NOT EXISTS test_table (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
"#;
    
    let new_file = assets_dir.join("20240401000001.sql");
    tokio::fs::write(&new_file, new_migration).await?;
    
    println!("📝 添加新的迁移文件: {}", new_file.display());
    
    // 再次初始化应该检测到变化
    println!("🔍 检测到文件变化，执行迁移");
    storage_manager2.init_user_with_assets(uid2).await?;
    
    println!("✅ 文件变化检测演示完成");
    println!();
    
    println!("🎉 所有示例执行完成！");
    println!("\n💡 性能优化说明:");
    println!("1. 首次运行时会扫描 assets 目录并缓存文件信息");
    println!("2. 后续运行时只有在以下情况才会重新扫描:");
    println!("   - SDK 版本变化");
    println!("   - assets 目录路径变化");
    println!("   - assets 目录中的文件发生变化");
    println!("3. 缓存存储在每个用户的 KV 存储中，与用户数据隔离");
    println!("4. 可以手动清理或刷新缓存");
    
    Ok(())
}

/// 创建示例 SQL 迁移文件
async fn create_sample_migration_files(assets_dir: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // 确保 assets 目录存在
    tokio::fs::create_dir_all(assets_dir).await?;
    
    // 创建基础表结构（较早的时间戳）
    let init_sql = r#"
-- 初始化基础表结构
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL UNIQUE,
    channel_id TEXT NOT NULL,
    from_uid TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS conversations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL UNIQUE,
    channel_type INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
"#;
    
    let init_file = assets_dir.join("20240101000001.sql");
    if !init_file.exists() {
        tokio::fs::write(&init_file, init_sql).await?;
    }
    
    // 创建扩展表结构（较晚的时间戳）
    let extension_sql = r#"
-- 添加消息扩展字段
ALTER TABLE messages ADD COLUMN message_type INTEGER DEFAULT 0;
ALTER TABLE messages ADD COLUMN status INTEGER DEFAULT 0;
ALTER TABLE messages ADD COLUMN extra TEXT DEFAULT '{}';

-- 添加用户表
CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    uid TEXT NOT NULL UNIQUE,
    username TEXT NOT NULL,
    avatar TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
"#;
    
    let extension_file = assets_dir.join("20240201000001.sql");
    if !extension_file.exists() {
        tokio::fs::write(&extension_file, extension_sql).await?;
    }
    
    // 创建索引优化（更晚的时间戳）
    let index_sql = r#"
-- 创建性能优化索引
CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id);
CREATE INDEX IF NOT EXISTS idx_messages_from_uid ON messages(from_uid);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_conversations_channel_id ON conversations(channel_id);
"#;
    
    let index_file = assets_dir.join("20240301000001.sql");
    if !index_file.exists() {
        tokio::fs::write(&index_file, index_sql).await?;
    }
    
    println!("📝 已创建示例迁移文件:");
    println!("   - {}", init_file.display());
    println!("   - {}", extension_file.display());
    println!("   - {}", index_file.display());
    
    Ok(())
}
