# PrivChat SDK

现代化的即时通讯SDK，采用Rust编写，支持多用户、多协议、数据库迁移等功能。

## 存储管理器 (StorageManager)

### 概述

StorageManager是SDK的核心存储管理组件，提供了统一的数据访问接口。它采用分层架构设计，支持多用户数据隔离、自动数据库迁移和版本管理。

### 主要特性

- 🔐 **数据隔离**：每个用户拥有独立的数据库文件
- 📦 **迁移管理**：支持嵌入式SQL和外部SQL文件的数据库迁移
- 🔄 **增量更新**：智能检测和执行增量迁移
- 🏗️ **分层架构**：DAO层、实体层、存储层清晰分离
- 🔄 **用户切换**：支持多用户间的无缝切换
- 🚀 **智能缓存**：缓存assets文件信息，避免重复扫描
- 📊 **性能优化**：基于版本号和文件时间戳的智能迁移检查

### 性能优化机制

PrivChat SDK 实现了智能的缓存机制来优化数据库迁移性能：

#### 🚀 智能缓存策略

1. **首次运行**：扫描assets目录，缓存文件信息
2. **后续运行**：只有以下情况才重新扫描：
   - SDK版本变化
   - assets目录路径变化  
   - assets目录中的文件发生变化
3. **缓存存储**：每个用户的KV存储中独立缓存
4. **自动失效**：版本升级时自动清理过期缓存

#### 📊 缓存内容

```rust
pub struct AssetsCache {
    pub sdk_version: String,                    // SDK版本号
    pub assets_path: String,                    // Assets目录路径
    pub file_timestamps: BTreeMap<String, u64>, // 文件时间戳映射
    pub cached_at: u64,                         // 缓存创建时间
    pub last_db_version: String,                // 最后数据库版本
}
```

#### 🔍 检查逻辑

```rust
// 检查是否需要重新扫描
async fn check_need_migration(&self, assets_path: &Path) -> Result<bool> {
    // 1. 检查缓存是否存在
    // 2. 检查SDK版本是否变化
    // 3. 检查assets目录路径是否变化
    // 4. 检查文件时间戳是否变化
    // 5. 返回是否需要重新扫描
}
```

### 初始化方式

#### 1. 简单初始化（仅用户数据目录）

```rust
use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;

// 创建存储管理器
let user_data_dir = PathBuf::from("./app_data");
let storage_manager = StorageManager::new_simple(&user_data_dir).await?;

// 初始化用户数据库（使用嵌入式SQL）
let uid = "user123";
storage_manager.init_user(uid).await?;
storage_manager.switch_user(uid).await?;
```

#### 2. 使用预设Assets目录

```rust
use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;

// 创建存储管理器，同时设置用户数据目录和assets目录
let user_data_dir = PathBuf::from("./app_data");
let assets_dir = PathBuf::from("./assets");
let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await?;

// 初始化用户数据库（使用预设的assets目录）
let uid = "user456";
storage_manager.init_user_with_assets(uid).await?;
storage_manager.switch_user(uid).await?;
```

#### 3. 使用自定义Assets目录

```rust
use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;

// 创建简单的存储管理器
let user_data_dir = PathBuf::from("./app_data");
let storage_manager = StorageManager::new_simple(&user_data_dir).await?;

// 为特定用户使用自定义assets目录
let uid = "user789";
let custom_assets_dir = PathBuf::from("./custom_migrations");
storage_manager.init_user_with_custom_assets(uid, &custom_assets_dir).await?;
storage_manager.switch_user(uid).await?;
```

### API 参考

#### 创建方法

| 方法 | 描述 | 用途 |
|------|------|------|
| `new_simple(base_path)` | 仅设置用户数据目录 | 使用嵌入式SQL的简单场景 |
| `new_with_assets(base_path, assets_path)` | 设置用户数据目录和assets目录 | 使用外部SQL文件的场景 |
| `new(base_path, assets_path)` | 底层通用方法 | 高级用法，支持可选的assets目录 |

#### 初始化方法

| 方法 | 描述 | 适用场景 |
|------|------|----------|
| `init_user(uid)` | 使用嵌入式SQL初始化 | 标准场景，SQL内置在代码中 |
| `init_user_with_assets(uid)` | 使用预设的assets目录 | 使用初始化时设置的assets目录 |
| `init_user_with_custom_assets(uid, assets_path)` | 使用自定义assets目录 | 为特定用户使用特定的迁移文件 |

#### 配置方法

| 方法 | 描述 | 返回值 |
|------|------|--------|
| `assets_path()` | 获取assets目录路径 | `Option<&Path>` |
| `base_path()` | 获取基础数据目录路径 | `&Path` |
| `user_dir(uid)` | 获取特定用户的数据目录 | `PathBuf` |

#### 用户管理

| 方法 | 描述 | 用途 |
|------|------|------|
| `switch_user(uid)` | 切换当前活跃用户 | 多用户场景下的用户切换 |

#### 缓存管理

| 方法 | 描述 | 用途 |
|------|------|------|
| `clear_assets_cache()` | 清理assets缓存 | 强制重新扫描assets目录 |
| `refresh_assets_cache(assets_path)` | 刷新assets缓存 | 重新扫描并更新缓存 |
| `get_assets_cache_info()` | 获取缓存信息 | 查看当前缓存状态 |
| `kv_store()` | 获取KV存储引用 | 访问底层KV存储 |

### 数据库迁移

#### SQL文件命名规则

SQL迁移文件必须按以下格式命名：

- `YYYYMMDDHHMMSS.sql` - 完整时间戳（推荐）
- `YYYYMMDD.sql` - 日期格式

例如：
- `20240101000001.sql` - 2024年1月1日00:00:01
- `20240201120000.sql` - 2024年2月1日12:00:00
- `20240301.sql` - 2024年3月1日

#### 迁移文件示例

**基础表结构 (20240101000001.sql)**
```sql
-- 初始化基础表结构
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL UNIQUE,
    channel_id TEXT NOT NULL,
    from_uid TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL UNIQUE,
    channel_type INTEGER NOT NULL DEFAULT 0,
    title TEXT NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
```

**扩展字段 (20240201000001.sql)**
```sql
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
```

**索引优化 (20240301000001.sql)**
```sql
-- 创建性能优化索引
CREATE INDEX IF NOT EXISTS idx_messages_channel_id ON messages(channel_id);
CREATE INDEX IF NOT EXISTS idx_messages_from_uid ON messages(from_uid);
CREATE INDEX IF NOT EXISTS idx_messages_created_at ON messages(created_at);
CREATE INDEX IF NOT EXISTS idx_channels_channel_id ON channels(channel_id);
```

### 目录结构

```
your_app/
├── app_data/                 # 用户数据目录
│   └── users/               # 用户数据
│       ├── user123/         # 用户123的数据
│       │   └── messages.db  # 用户数据库文件
│       └── user456/         # 用户456的数据
│           └── messages.db  # 用户数据库文件
└── assets/                  # SQL迁移文件目录
    ├── 20240101000001.sql   # 基础表结构
    ├── 20240201000001.sql   # 扩展字段
    └── 20240301000001.sql   # 索引优化
```

### 完整示例

```rust
use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt::init();
    
    // 设置数据目录
    let user_data_dir = PathBuf::from("./app_data");
    let assets_dir = PathBuf::from("./assets");
    
    // 创建存储管理器
    let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await?;
    
    // 初始化用户数据库
    let uid = "user123";
    storage_manager.init_user_with_assets(uid).await?;
    storage_manager.switch_user(uid).await?;
    
    println!("✅ 用户数据库初始化完成");
    println!("📍 用户数据目录: {}", storage_manager.user_dir(uid).display());
    println!("📍 Assets目录: {}", storage_manager.assets_path().unwrap().display());
    
    Ok(())
}
```

### 缓存管理示例

```rust
use privchat_sdk::storage::StorageManager;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let user_data_dir = PathBuf::from("./app_data");
    let assets_dir = PathBuf::from("./assets");
    let storage_manager = StorageManager::new_with_assets(&user_data_dir, &assets_dir).await?;
    
    let uid = "user123";
    
    // 首次初始化 - 扫描assets目录
    println!("🔍 首次初始化 - 扫描assets目录");
    storage_manager.init_user_with_assets(uid).await?;
    
    // 查看缓存信息
    if let Some(cache_info) = storage_manager.get_assets_cache_info().await? {
        println!("📋 缓存信息:");
        println!("   SDK版本: {}", cache_info.sdk_version);
        println!("   Assets路径: {}", cache_info.assets_path);
        println!("   文件数量: {}", cache_info.file_timestamps.len());
    }
    
    // 再次初始化 - 使用缓存
    println!("🚀 再次初始化 - 使用缓存");
    storage_manager.init_user_with_assets(uid).await?;
    
    // 清理缓存
    println!("🧹 清理缓存");
    storage_manager.clear_assets_cache().await?;
    
    // 清理后再次初始化 - 重新扫描
    println!("🔍 清理后再次初始化 - 重新扫描");
    storage_manager.init_user_with_assets(uid).await?;
    
    // 手动刷新缓存
    println!("🔄 手动刷新缓存");
    storage_manager.refresh_assets_cache(&assets_dir).await?;
    
    Ok(())
}
```

### 错误处理

```rust
use privchat_sdk::error::PrivchatSDKError;

match storage_manager.init_user_with_assets("user123").await {
    Ok(_) => println!("初始化成功"),
    Err(PrivchatSDKError::Database(msg)) => println!("数据库错误: {}", msg),
    Err(PrivchatSDKError::IO(msg)) => println!("IO错误: {}", msg),
    Err(e) => println!("其他错误: {}", e),
}
```

### 最佳实践

1. **目录规划**：建议将用户数据目录和assets目录分开管理
2. **版本控制**：将SQL迁移文件纳入版本控制
3. **备份策略**：定期备份用户数据目录
4. **测试**：在生产环境前充分测试迁移脚本
5. **监控**：记录迁移过程的日志用于故障排查
6. **缓存管理**：合理使用缓存机制提升性能
   - 初始化时设置固定的assets目录路径
   - 避免频繁更改assets目录位置
   - 在开发阶段可以手动清理缓存
   - 生产环境依赖自动缓存失效机制
7. **性能优化**：
   - 使用预设的assets目录而非每次传入
   - 利用智能缓存避免重复扫描
   - 监控缓存命中率和迁移执行时间

### 依赖

- `rusqlite` ^0.36 - SQLite数据库驱动
- `tokio` ^1.0 - 异步运行时
- `tracing` ^0.1 - 日志记录
- `chrono` ^0.4 - 时间处理

### 许可证

本项目采用MIT许可证。详见LICENSE文件。
