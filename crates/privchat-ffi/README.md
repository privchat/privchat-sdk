# Privchat FFI - UniFFI 跨语言绑定层

[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org)
[![UniFFI](https://img.shields.io/badge/UniFFI-0.31%2Fmain-blue.svg)](https://mozilla.github.io/uniffi-rs/)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-green.svg)](LICENSE)

完整的即时通讯 SDK FFI 层，使用 **UniFFI** 框架生成 Kotlin、Swift、Python、Ruby 等多语言绑定。

## ✨ 特性

### 核心功能 ✅
- 🔗 **连接管理** - 连接、断开、重连、状态监控
- 💬 **消息收发** - 发送消息、接收消息、已读回执
- 🎯 **事件系统** - 轮询模式 + 回调模式
- ⚙️  **配置管理** - Builder 模式，灵活配置

### 高级功能 ✅（阶段4）
- 📚 **消息管理** - 历史查询、全文搜索
- 📋 **会话管理** - 会话列表、标记已读
- 👥 **在线状态** - 订阅、查询、缓存
- ⌨️  **输入状态** - 实时输入指示器
- 🎯 **高级操作** - 消息撤回、编辑、表情反应

### 跨语言支持 ✅
- 📱 **Android** - Kotlin bindings
- 🍎 **iOS** - Swift bindings
- 🐍 **Python** - Python bindings
- 💎 **Ruby** - Ruby bindings

## 🏗️ 架构

```
┌─────────────────────────────────────────────┐
│     UI Layer (Kotlin/Swift/Python...)      │
│   实现 PrivchatDelegate 接收回调通知        │
└─────────────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────┐
│          Privchat FFI (本项目)              │
│  ├─ 基础API: 连接、消息、事件               │
│  ├─ 高级API: 查询、在线状态、输入状态       │
│  ├─ 事件系统: 轮询 + 回调                   │
│  └─ 类型转换: SDK类型 ↔ FFI类型            │
└─────────────────────────────────────────────┘
                     ↓
┌─────────────────────────────────────────────┐
│         Privchat SDK (核心SDK)              │
│  ├─ 网络层: TCP/WebSocket                  │
│  ├─ 存储层: SQLCipher 加密数据库            │
│  ├─ 事件管理: 统一事件分发                  │
│  └─ RPC 客户端: 服务端通信                  │
└─────────────────────────────────────────────┘
```

## UniFFI 版本

当前使用 **UniFFI main**（`git = "https://github.com/mozilla/uniffi-rs", branch = "main"`），以获取最新修复（例如 [v0.31 中修复的 completing foreign futures 段错误 #2733](https://github.com/mozilla/uniffi-rs/pull/2733)）。  
若需锁定到发布版，可在 `Cargo.toml` 中改回：

```toml
uniffi = { version = "0.31", features = ["tokio"] }
```

升级或切换 UniFFI 后需**重新生成 C/Swift 绑定**并重新编译各目标静态库，否则 Kotlin cinterop 可能 ABI 不一致。参见 [privchat-sdk-kotlin/README.md](../../../privchat-sdk-kotlin/README.md) 中「重新生成 C 头文件」步骤。

## 🚀 快速开始

### Rust

```rust
use privchat_ffi::{PrivchatSDK, PrivchatConfigBuilder, ServerEndpoint, TransportProtocol};
use std::sync::Arc;

// 1. 配置
let endpoint = ServerEndpoint {
    protocol: TransportProtocol::Tcp,
    host: "127.0.0.1".to_string(),
    port: 9001,
    path: None,
    use_tls: false,
};

let config = Arc::new(PrivchatConfigBuilder::new())
    .data_dir("/tmp/privchat_data".to_string())
    .server_endpoint(endpoint)
    .build()?;

// 2. 初始化
let sdk = Arc::new(PrivchatSDK::new(config)?);

// 3. 连接
sdk.clone().connect("user123".to_string(), "token".to_string())?;

// 4. 发送消息
sdk.clone().send_message("Hello!".to_string(), 12345, 1)?;

// 5. 查询历史
let history = sdk.get_message_history(12345, 20, None)?;

// 6. 订阅在线状态
sdk.subscribe_presence(vec![100, 200, 300])?;

// 7. 断开
sdk.clone().disconnect()?;
sdk.shutdown()?;
```

### Kotlin (Android)

```kotlin
// 1. 初始化
val endpoint = ServerEndpoint(
    protocol = TransportProtocol.TCP,
    host = "127.0.0.1",
    port = 8080u,
    path = null,
    useTls = false
)

val config = PrivchatConfigBuilder()
    .dataDir("/data/privchat")
    .serverEndpoint(endpoint)
    .build()

val sdk = PrivchatSDK(config)

// 2. 设置回调
sdk.setDelegate(object : PrivchatDelegate {
    override fun onMessageReceived(messageJson: String) {
        val message = JSONObject(messageJson)
        Log.i("SDK", "New message: ${message.getString("content")}")
    }
    
    override fun onConnectionStateChanged(oldState: String, newState: String) {
        Log.i("SDK", "Connection: $oldState -> $newState")
    }
    
    override fun onEvent(eventJson: String) {
        // Handle generic events
    }
})

// 3. 连接
sdk.connect("user123", "token")

// 4. 发送消息
sdk.sendMessage("Hello from Kotlin!", 12345u, 1)

// 5. 查询历史
val historyJson = sdk.getMessageHistory(12345u, 20u, null)

// 6. 在线状态
val statusesJson = sdk.subscribePresence(listOf(100u, 200u, 300u))
```

### Swift (iOS)

```swift
// 1. 初始化
let endpoint = ServerEndpoint(
    protocol: .tcp,
    host: "127.0.0.1",
    port: 9001,
    path: nil,
    useTls: false
)

let config = try PrivchatConfigBuilder()
    .dataDir("/path/to/data")
    .serverEndpoint(endpoint: endpoint)
    .build()

let sdk = try PrivchatSDK(config: config)

// 2. 设置回调
class MyDelegate: PrivchatDelegate {
    func onMessageReceived(messageJson: String) {
        print("New message: \(messageJson)")
    }
    
    func onConnectionStateChanged(oldState: String, newState: String) {
        print("Connection: \(oldState) -> \(newState)")
    }
    
    func onEvent(eventJson: String) {
        // Handle events
    }
}

sdk.setDelegate(delegate: MyDelegate())

// 3. 连接和发消息
try sdk.connect(login: "user123", token: "token")
try sdk.sendMessage(content: "Hello from Swift!", channelId: 12345, channelType: 1)

// 4. 查询历史
let historyJson = try sdk.getMessageHistory(channelId: 12345, limit: 20, beforeSeq: nil)
```

### Python

```python
from privchat_ffi import PrivchatSDK, PrivchatConfigBuilder, ServerEndpoint, TransportProtocol
import json

# 1. 配置
endpoint = ServerEndpoint(
    protocol=TransportProtocol.TCP,
    host="127.0.0.1",
    port=8080,
    path=None,
    use_tls=False
)

config = PrivchatConfigBuilder() \
    .data_dir("/tmp/privchat") \
    .server_endpoint(endpoint) \
    .build()

# 2. 初始化
sdk = PrivchatSDK(config)

# 3. 设置回调
class MyDelegate:
    def on_message_received(self, message_json):
        message = json.loads(message_json)
        print(f"New message: {message}")
    
    def on_connection_state_changed(self, old_state, new_state):
        print(f"Connection: {old_state} -> {new_state}")
    
    def on_event(self, event_json):
        pass

sdk.set_delegate(MyDelegate())

# 4. 使用
sdk.connect("user123", "token")
sdk.send_message("Hello from Python!", 12345, 1)

# 5. 查询
history = sdk.get_message_history(12345, 20, None)
messages = json.loads(history)
```

## 📚 完整API列表

### 基础API

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `new(config)` | 初始化SDK | `PrivchatSDK` |
| `connect(login, token)` | 连接服务器 | `TaskHandle` |
| `disconnect()` | 断开连接 | `TaskHandle` |
| `send_message(content, channel_id, channel_type)` | 发送消息 | `TaskHandle` |
| `mark_as_read(channel_id, message_id)` | 标记已读 | `TaskHandle` |
| `connection_state()` | 获取连接状态 | `ConnectionState` |
| `current_user_id()` | 获取当前用户ID | `String?` |
| `shutdown()` | 关闭SDK | `void` |

### 账号管理（新增 ✨）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `register(username, password, device_id)` | 注册新账号 | `String (JSON)` |
| `login(username, password, device_id)` | 登录账号 | `String (JSON)` |
| `authenticate(user_id, token)` | 认证用户 | `void` |

### 好友管理（新增 ✨）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `get_friends()` | 获取好友列表 | `String (JSON)` |
| `search_users(query)` | 搜索用户 | `String (JSON)` |
| `send_friend_request(to_user_id, remark)` | 发送好友请求 | `String (JSON)` |
| `accept_friend_request(from_user_id)` | 接受好友请求 | `String (JSON)` |
| `reject_friend_request(from_user_id)` | 拒绝好友请求 | `String (JSON)` |

### 群组管理（新增 ✨）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `create_group(name, member_ids)` | 创建群组 | `String (JSON)` |
| `invite_to_group(group_id, user_ids)` | 邀请进群 | `String (JSON)` |

### 事件系统

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `set_delegate(delegate)` | 设置回调 | `void` |
| `remove_delegate()` | 移除回调 | `void` |
| `poll_events(max_events)` | 轮询事件 | `Vec<SDKEvent>` |
| `pending_events_count()` | 待处理事件数 | `u32` |
| `clear_events()` | 清空事件队列 | `void` |

### 消息管理（阶段4）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `get_message_history(channel_id, limit, before_seq)` | 查询历史消息 | `String (JSON)` |
| `search_messages(query, channel_id)` | 搜索消息 | `String (JSON)` |

### 会话管理（阶段4）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `get_channels()` | 获取会话列表 | `String (JSON)` |
| `mark_channel_read(channel_id, channel_type)` | 标记已读 | `void` |

### 在线状态（阶段4）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `subscribe_presence(user_ids)` | 订阅在线状态 | `String (JSON)` |
| `unsubscribe_presence(user_ids)` | 取消订阅 | `void` |
| `get_presence(user_id)` | 查询状态（缓存） | `String? (JSON)` |
| `batch_get_presence(user_ids)` | 批量查询（缓存） | `String (JSON)` |
| `fetch_presence(user_ids)` | 查询状态（服务器） | `String (JSON)` |

### 输入状态（阶段4）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `send_typing(channel_id)` | 发送输入状态 | `void` |
| `stop_typing(channel_id)` | 停止输入 | `void` |

### 高级操作（阶段4）

| 方法 | 说明 | 返回值 |
|------|------|--------|
| `revoke_message(message_id)` | 撤回消息 | `void` |
| `edit_message(message_id, new_content)` | 编辑消息 | `void` |
| `add_reaction(message_id, emoji)` | 添加表情反应 | `void` |

## 📖 示例

### 1. 完整工作流程 ⭐ 推荐
```bash
cargo run --example complete_workflow
```
展示从注册到使用的完整流程：注册、登录、获取会话、获取好友、发消息等

### 2. 基础使用
```bash
cargo run --example basic_usage
```
最简单的SDK使用示例

### 3. 完整工作流程（旧版）
```bash
cargo run --example full_workflow
```
展示连接、发消息、断开等基础流程

### 4. 回调接口
```bash
cargo run --example callback_demo
```
演示如何使用回调接收实时事件

### 5. 事件轮询
```bash
cargo run --example event_polling
```
演示如何使用轮询方式获取事件

### 6. 高级功能
```bash
cargo run --example advanced_features
```
展示消息历史、在线状态、输入状态等高级功能

## 🧪 测试

```bash
# 运行所有测试
cargo test

# 运行特定测试
cargo test --lib

# 运行集成测试
cargo test --test '*'
```

## 📦 生成绑定

### 生成 Kotlin 绑定
```bash
cargo run --bin uniffi-bindgen generate src/api.udl --language kotlin --out-dir bindings/kotlin
```

### 生成 Swift 绑定
```bash
cargo run --bin uniffi-bindgen generate src/api.udl --language swift --out-dir bindings/swift
```

### 生成 Python 绑定
```bash
cargo run --bin uniffi-bindgen generate src/api.udl --language python --out-dir bindings/python
```

## 🎯 开发进度

- [x] **阶段0**: 最小可编译版本（基础结构）
- [x] **阶段1**: 真实SDK集成（连接、消息）
- [x] **阶段2**: 事件轮询系统
- [x] **阶段3**: 回调接口（Delegate）
- [x] **阶段4**: 完整功能（查询、在线状态、高级操作）
- [ ] **阶段5**: 测试和文档（单元测试、集成测试、API文档）

查看详细进度：
- [ROADMAP.md](ROADMAP.md) - 6阶段路线图
- [ARCHITECTURE.md](ARCHITECTURE.md) - 架构设计文档
- [PHASE4_COMPLETE.md](PHASE4_COMPLETE.md) - 阶段4完成报告

## 🏆 特点

### 与 Matrix SDK FFI 对比

| 特性 | Privchat FFI | Matrix SDK FFI |
|------|--------------|----------------|
| UniFFI 版本 | 0.27 | 0.25 |
| 自定义宏 | ✅ | ❌ |
| 事件轮询 | ✅ | ❌ |
| 回调接口 | ✅ | ✅ |
| TaskHandle | ✅ | ✅ |
| Builder 模式 | ✅ | ✅ |
| 在线状态 | ✅ | ❌ |
| 输入状态 | ✅ | ✅ |
| 消息撤回 | ✅ | ✅ |

### 技术亮点

1. **双模式事件系统**
   - 轮询模式（简单场景）
   - 回调模式（生产环境）
   - 混合模式（最大灵活性）

2. **智能缓存**
   - 在线状态三层缓存
   - 自动清理机制
   - LRU 策略

3. **性能优化**
   - 输入状态防抖
   - 批量查询支持
   - 异步处理

4. **跨语言友好**
   - JSON 数据交换
   - 统一错误处理
   - 详细文档

## 📝 许可证

本项目采用双重许可：

- MIT License
- Apache License 2.0

## 🤝 贡献

欢迎贡献！请查看 [CONTRIBUTING.md](CONTRIBUTING.md) 了解详情。

## 📧 联系

- Issues: [GitHub Issues](https://github.com/yourorg/privchat/issues)
- Discussions: [GitHub Discussions](https://github.com/yourorg/privchat/discussions)

---

**Built with ❤️ using UniFFI and Rust** 🦀
