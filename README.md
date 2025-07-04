# Privchat SDK

[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Security](https://img.shields.io/badge/security-SQLCipher-green.svg)](https://www.zetetic.net/sqlcipher/)

> 🔐 **高安全性私聊 SDK** - 基于 `msgtrans` 传输层的端到端加密通信解决方案

## 📖 项目概述

Privchat SDK 是一个专为私密通信设计的 Rust 库，提供完整的即时通讯功能，包括加密消息传输、安全的本地数据存储和多用户支持。基于 `msgtrans` 统一传输层框架，支持 TCP、WebSocket 和 QUIC 协议。

## ✨ 核心特性

### 🔒 安全性
- **SQLCipher 加密数据库**：使用 AES-256 加密保护本地数据
- **端到端加密**：基于 Curve25519 + AES-GCM 的消息加密
- **用户隔离**：每个用户使用独立的加密密钥
- **安全密钥派生**：基于用户 ID 自动生成加密密钥

### 🚀 架构设计
- **延迟初始化**：连接成功后才创建用户目录和数据库
- **异步优先**：完全异步的 API 设计，支持高并发
- **多协议支持**：TCP、WebSocket、QUIC 统一 API
- **FFI 友好**：提供 C 语言接口，支持跨语言调用

### 📱 消息类型
- **16 种消息类型**：涵盖连接、发送、接收、订阅、推送等完整场景
- **请求/响应模式**：支持 `send()` 单向和 `request()` 双向通信
- **批量消息**：支持历史消息批量推送
- **频道推送**：支持服务器向订阅者广播消息

## 🏗️ 架构设计

### 目录结构
```
privchat-sdk/
├── crates/
│   ├── privchat-sdk/       # 核心 SDK 实现
│   │   ├── src/
│   │   │   ├── client.rs   # 客户端主要逻辑
│   │   │   ├── error.rs    # 错误处理
│   │   │   └── lib.rs      # 公共 API
│   │   ├── examples/       # 使用示例
│   │   └── tests/          # 单元测试
│   └── privchat-ffi/       # C 语言 FFI 接口
└── README.md
```

### 用户数据目录
```
~/.privchat/
├── u_<UID_1>/              # 用户 1 的数据目录
│   ├── messages.db         # 加密的消息数据库 (SQLCipher)
│   ├── messages/           # 消息缓存
│   ├── media/              # 媒体文件
│   │   ├── images/         # 图片
│   │   ├── videos/         # 视频
│   │   └── audios/         # 音频
│   ├── files/              # 附件文件
│   └── cache/              # 临时缓存
└── u_<UID_2>/              # 用户 2 的数据目录
    └── ...
```

## 🔐 安全特性

### SQLCipher 加密数据库
```rust
// 自动设置加密密钥
conn.pragma_update(None, "key", &encryption_key)?;

// 创建加密表
conn.execute(
    "CREATE TABLE messages (
        id INTEGER PRIMARY KEY,
        content TEXT NOT NULL,
        ...
    )",
    [],
)?;
```

### 关键安全措施
- 🔑 **密钥派生**：基于用户 ID 使用哈希算法生成唯一密钥
- 🗄️ **数据库加密**：所有本地数据使用 SQLCipher AES-256 加密
- 🚫 **防止分析**：加密数据库无法使用标准 SQLite 工具查看
- 👤 **用户隔离**：每个用户拥有独立的数据目录和加密密钥

## 📋 消息类型

### 基础通信
```rust
// 连接/断开
ConnectRequest/ConnectResponse       // 1/2  - 客户端连接认证
DisconnectRequest/DisconnectResponse // 3/4  - 断开连接确认

// 消息发送
SendRequest/SendResponse             // 5/6  - 发送消息
RecvRequest/RecvResponse             // 7/8  - 接收消息确认

// 系统功能
PingRequest/PingResponse             // 9/10 - 心跳检测
SubscribeRequest/SubscribeResponse   // 11/12- 频道订阅
```

### 高级功能
```rust
// 批量消息
RecvBatchRequest/RecvBatchResponse   // 13/14- 批量历史消息

// 频道推送
PublishRequest/PublishResponse       // 15/16- 服务器推送消息
```

## 🚀 快速开始

### 1. 添加依赖
```toml
[dependencies]
privchat-sdk = { version = "0.1.0", path = "path/to/privchat-sdk" }
msgtrans = { version = "1.0.0", path = "path/to/msgtrans" }
tokio = { version = "1.0", features = ["full"] }
```

### 2. 基本使用
```rust
use privchat_sdk::PrivchatClient;
use msgtrans::Transport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 创建传输层
    let transport = Arc::new(Transport::new(/* 配置 */));
    
    // 2. 创建客户端
    let mut client = PrivchatClient::new(
        "/home/user/.privchat",  // 工作目录
        transport
    ).await?;
    
    // 3. 连接并认证
    client.connect("13800001111", "sms_code_123").await?;
    
    // 4. 发送消息
    let message = SendRequest {
        from_uid: client.user_id().unwrap().to_string(),
        channel_id: "chat_123".to_string(),
        payload: "Hello, World!".as_bytes().to_vec(),
        client_msg_no: uuid::Uuid::new_v4().to_string(),
    };
    
    client.send_chat_message(&message).await?;
    
    // 5. 订阅频道
    client.subscribe_channel("news_channel").await?;
    
    // 6. 断开连接
    client.disconnect("用户主动退出").await?;
    
    Ok(())
}
```

### 3. 高级用法
```rust
// 获取用户信息
let user_id = client.user_id().unwrap();
let user_dir = client.user_dir().unwrap();
let is_connected = client.is_connected().await;

// 获取加密数据库连接
if let Some(db) = client.database() {
    let conn = db.lock().unwrap();
    // 执行数据库操作
}

// 检查连接状态
if client.is_connected().await {
    println!("已连接到服务器");
}
```

## 🧪 运行测试

```bash
# 运行所有测试
cargo test

# 运行带输出的测试
cargo test -- --nocapture

# 运行特定测试
cargo test test_sqlcipher_database -- --nocapture
```

### 测试覆盖
- ✅ **密钥派生测试**：验证用户 ID 生成唯一密钥
- ✅ **SQLCipher 数据库测试**：验证加密数据库读写功能
- ✅ **数据库表创建测试**：验证自动创建完整表结构

## 📦 依赖关系

### 核心依赖
```toml
[dependencies]
msgtrans = { path = "../../../" }                    # 传输层框架
privchat-protocol = { path = "../../../privchat-protocol" } # 消息协议
rusqlite = { version = "0.36", features = ["bundled-sqlcipher-vendored-openssl"] } # 加密数据库
tokio = { version = "1.0", features = ["full"] }    # 异步运行时
serde = { version = "1.0", features = ["derive"] }  # 序列化
uuid = { version = "1.0", features = ["v4"] }       # UUID 生成
```

### 开发依赖
```toml
[dev-dependencies]
tempfile = "3.0"     # 临时文件（测试用）
tokio-test = "0.4"   # 异步测试工具
```

## 🔧 配置选项

### 传输层配置
```rust
use msgtrans::transport::TransportOptions;

let options = TransportOptions {
    connect_timeout: Duration::from_secs(10),
    read_timeout: Duration::from_secs(30),
    write_timeout: Duration::from_secs(30),
    max_packet_size: 1024 * 1024, // 1MB
    // ... 其他配置
};
```

### 数据库配置
```rust
// 自定义密钥派生（推荐生产环境使用）
impl PrivchatClient {
    fn derive_encryption_key_secure(user_id: &str, salt: &[u8]) -> String {
        // 使用 PBKDF2/Scrypt/Argon2 进行密钥派生
        // 示例：
        // let key = pbkdf2::pbkdf2_hmac::<sha2::Sha256>(
        //     user_id.as_bytes(),
        //     salt,
        //     10_000, // 迭代次数
        //     &mut key_bytes
        // );
        todo!("实现安全的密钥派生")
    }
}
```

## 🛠️ FFI 接口

### C 语言接口
```c
#include "privchat_ffi.h"

// 创建客户端
PrivchatClient* client = privchat_client_new("/path/to/data");

// 连接
bool connected = privchat_client_connect(client, "user", "token");

// 检查连接状态
bool is_connected = privchat_client_is_connected(client);

// 释放资源
privchat_client_free(client);
```

## 📈 性能特点

### 内存使用
- **低内存占用**：延迟初始化，按需创建资源
- **连接池复用**：复用传输层连接，减少开销
- **异步 I/O**：非阻塞 I/O，支持高并发

### 存储优化
- **增量同步**：仅同步变化的数据
- **压缩存储**：可选的消息内容压缩
- **清理策略**：自动清理过期缓存文件

## 🚨 安全建议

### 生产环境建议
1. **密钥管理**
   - 使用 PBKDF2/Scrypt/Argon2 进行密钥派生
   - 考虑使用硬件安全模块 (HSM)
   - 实现密钥轮换机制

2. **文件安全**
   - 数据库文件使用随机文件名
   - 设置适当的文件权限 (600)
   - 使用系统 API 设置隐藏属性

3. **网络安全**
   - 强制使用 TLS/SSL 加密传输
   - 实现证书固定 (Certificate Pinning)
   - 添加重放攻击防护

### 安全检查清单
- [ ] 密钥派生使用强随机数
- [ ] 数据库文件权限设置正确
- [ ] 传输层启用加密
- [ ] 实现消息去重机制
- [ ] 添加身份验证和授权
- [ ] 定期安全审计

## 🤝 贡献指南

### 开发环境
```bash
# 克隆项目
git clone <repository>
cd privchat-sdk

# 安装依赖
cargo build

# 运行测试
cargo test

# 格式化代码
cargo fmt

# 检查代码
cargo clippy
```

### 提交规范
- 使用清晰的提交消息
- 遵循 Rust 代码风格
- 添加必要的测试用例
- 更新相关文档

## 📄 许可证

本项目采用 MIT 许可证 - 详见 [LICENSE](LICENSE) 文件。

## 🔗 相关链接

- [msgtrans](../../../README.md) - 底层传输框架
- [privchat-protocol](../../../privchat-protocol/README.md) - 消息协议定义
- [SQLCipher](https://www.zetetic.net/sqlcipher/) - 加密数据库
- [Rust 异步编程](https://rust-lang.github.io/async-book/) - 异步编程指南

## 🆘 故障排除

### 常见问题

**Q: 数据库连接失败**
```
错误: SQLite错误: file is encrypted or is not a database
```
**A:** 检查加密密钥是否正确设置，确保使用相同的用户 ID 派生密钥。

**Q: 传输层连接超时**
```
错误: 传输错误: Connection timeout
```
**A:** 检查网络连接，确认服务器地址和端口正确。

**Q: FFI 调用失败**
```
错误: FFI功能需要完整的Transport实现
```
**A:** 当前 FFI 接口需要完整的 msgtrans Transport 实现才能正常工作。

### 调试技巧
1. 启用详细日志：`RUST_LOG=debug cargo run`
2. 检查文件权限：确保数据目录可写
3. 验证依赖版本：`cargo tree` 查看依赖树
4. 使用测试验证：`cargo test -- --nocapture`

---

**🔐 安全第一，隐私至上！**

> 如果您在使用过程中遇到任何问题或有改进建议，欢迎提 Issue 或 Pull Request。 