# privchat-sdk

PrivChat 即时通讯平台的核心 Rust SDK，为 iOS、Android、macOS、Linux、Windows 等多端客户端提供统一的底层能力。

## 定位

privchat-sdk 是整个 PrivChat 客户端体系的**唯一核心引擎**。上层的 Kotlin Multiplatform（privchat-sdk-kotlin）、Swift（privchat-sdk-swift）、Android（privchat-sdk-android）等平台 SDK 均通过 UniFFI 绑定调用本项目，不直接实现业务逻辑。

## 职责

- **连接管理**：建立 / 维持 / 断开与 PrivChat Server 的传输层连接
- **认证鉴权**：账号注册、密码登录、Token 鉴权
- **消息收发**：创建、发送、编辑、撤回、置顶消息，管理发送队列
- **会话管理**：频道增删改查、未读计数、已读标记、订阅 / 退订
- **实体管理**：好友、群组、群成员、黑名单的 CRUD
- **数据同步**：Bootstrap 全量同步、增量差分同步、按实体类型 / 频道粒度同步
- **本地存储**：SQLite + SQLCipher 加密数据库 + Sled KV 存储，支持多账号切换
- **事件驱动**：广播连接状态变更、同步完成、时间线更新、已读回执等事件
- **扩展能力**：消息表情回应、@提醒、定时提醒、在线状态、输入状态指示、通用 RPC 调用

## 架构

```
┌─────────────────────────────────────────────┐
│           平台 SDK (Kotlin / Swift)          │
│         通过 UniFFI 生成的绑定调用            │
├─────────────────────────────────────────────┤
│              privchat-sdk-ffi               │
│       UniFFI Object + async 导出层           │
│       PrivchatClient → InnerSdk 代理         │
├─────────────────────────────────────────────┤
│               privchat-sdk                  │
│                                             │
│  ┌──────────┐  ┌─────────────┐  ┌────────┐ │
│  │ Actor    │  │ Local Store │  │ Event  │ │
│  │ 命令队列  │  │ SQLite/Sled │  │ 广播   │ │
│  └────┬─────┘  └──────┬──────┘  └───┬────┘ │
│       │               │             │       │
│  ┌────┴───────────────┴─────────────┴────┐  │
│  │          Receive Pipeline             │  │
│  │        消息接收与处理管线               │  │
│  └───────────────┬───────────────────────┘  │
│                  │                           │
│  ┌───────────────┴───────────────────────┐  │
│  │       msgtrans 传输层                  │  │
│  │   QUIC / TCP / WebSocket + TLS        │  │
│  └───────────────────────────────────────┘  │
├─────────────────────────────────────────────┤
│            privchat-protocol                │
│         协议定义（消息编解码）                │
└─────────────────────────────────────────────┘
```

### 核心设计

- **单线程 Actor 模型**：所有命令（connect / login / authenticate 等）通过 MPSC 通道串行执行，无 `RwLock` 竞争，无数据竞争
- **纯 async**：全链路 async/await，不含任何同步阻塞兼容层
- **事件广播**：256 容量的 broadcast channel + 1024 条事件历史缓冲，带序号和时间戳
- **多协议传输**：QUIC（低延迟）、TCP（高兼容）、WebSocket（移动友好），支持自动降级和 TLS
- **加密本地存储**：SQLCipher 加密的 SQLite + Sled KV，支持多用户 UID 切换，带消息缓存预算策略

## 项目结构

```
privchat-sdk/
├── crates/
│   ├── privchat-sdk/           # 核心 SDK 库
│   │   ├── src/
│   │   │   ├── lib.rs          # SDK 主逻辑（Actor 模型、状态机、API）
│   │   │   ├── local_store.rs  # SQLite 本地存储
│   │   │   ├── storage_actor.rs # 异步存储操作
│   │   │   ├── receive_pipeline.rs # 消息接收管线
│   │   │   ├── task/           # 后台任务注册
│   │   │   └── runtime/        # Tokio 运行时抽象
│   │   └── examples/basic/     # SDK 使用示例
│   │
│   └── privchat-sdk-ffi/       # FFI 导出层
│       ├── src/lib.rs          # UniFFI 导出（PrivchatClient）
│       ├── uniffi.toml         # UniFFI 绑定配置
│       ├── bindings/           # 生成的绑定代码（Kotlin / Swift）
│       └── examples/           # FFI 使用示例
│
├── assets/                     # 运行时资源
├── docs/                       # 架构文档与 API 规范
└── Cargo.toml                  # Workspace 根配置
```

## 主要 API

### 连接与认证

| 方法 | 说明 |
|------|------|
| `connect()` | 建立传输层连接 |
| `login(username, password, device_id)` | 用户名密码登录 |
| `register(username, password, device_id)` | 注册新账号 |
| `authenticate(user_id, token, device_id)` | Token 鉴权 |
| `disconnect()` / `shutdown()` | 断开连接 / 关闭 SDK |

### 消息

| 方法 | 说明 |
|------|------|
| `create_local_message()` | 创建本地消息（未发送） |
| `list_messages(channel_id, channel_type, limit, offset)` | 查询消息列表 |
| `enqueue_outbound_message()` | 入发送队列 |
| `edit_message()` | 编辑消息 |
| `set_message_revoke()` | 撤回消息 |
| `set_message_pinned()` | 置顶 / 取消置顶 |

### 会话与频道

| 方法 | 说明 |
|------|------|
| `upsert_channel()` | 创建或更新频道 |
| `get_channel_by_id()` / `list_channels()` | 查询频道 |
| `mark_channel_read()` | 标记已读 |
| `get_channel_unread_count()` | 获取未读数 |
| `subscribe_channel()` / `unsubscribe_channel()` | 订阅 / 退订频道推送 |

### 实体管理

| 方法 | 说明 |
|------|------|
| `upsert_user()` / `get_user_by_id()` | 用户信息 |
| `upsert_friend()` / `delete_friend()` / `list_friends()` | 好友 |
| `upsert_group()` / `get_group_by_id()` / `list_groups()` | 群组 |
| `upsert_group_member()` / `list_group_members()` | 群成员 |
| `upsert_blacklist_entry()` / `list_blacklist_entries()` | 黑名单 |

### 数据同步

| 方法 | 说明 |
|------|------|
| `run_bootstrap_sync()` | 首次全量同步 |
| `sync_entities(entity_type, scope)` | 按实体类型同步 |
| `sync_channel()` / `sync_all_channels()` | 频道同步 |
| `get_difference()` | 增量差分拉取 |

### 扩展功能

| 方法 | 说明 |
|------|------|
| `upsert_message_reaction()` | 消息表情回应 |
| `record_mention()` / `get_unread_mention_count()` | @提醒 |
| `subscribe_presence()` / `fetch_presence()` | 在线状态 |
| `send_typing()` | 输入状态指示 |
| `kv_put()` / `kv_get()` / `kv_scan_prefix()` | KV 存储 |
| `rpc_call(route, body_json)` | 通用 RPC 调用 |

## 构建

```bash
# 检查编译
cargo check

# 构建 FFI 库（Release）
cargo build -p privchat-sdk-ffi --release

# 运行核心测试
cargo test -p privchat-sdk --lib
```

## 生成跨语言绑定

### Kotlin 绑定

```bash
cd crates/privchat-sdk-ffi
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language kotlin --out-dir bindings/kotlin --config uniffi.toml
```

### Swift 绑定

```bash
cd crates/privchat-sdk-ffi
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language swift --out-dir bindings/swift --config uniffi.toml
```

## 交叉编译

```bash
# iOS Simulator (arm64)
cargo build -p privchat-sdk-ffi --release --target aarch64-apple-ios-sim

# iOS Device
cargo build -p privchat-sdk-ffi --release --target aarch64-apple-ios

# Android (需要 cargo-ndk)
cargo ndk -t arm64-v8a build -p privchat-sdk-ffi --release
```

## 状态机

SDK 内部维护一个严格的会话状态机：

```
New → Connected → LoggedIn → Authenticated → Shutdown
                                    ↑
                  register/login ───┘
```

所有 API 调用都会校验当前状态，在错误状态下调用会返回明确的错误码。

## 错误处理

SDK 使用 `SdkError` 枚举统一错误类型，FFI 层包装为 `PrivchatFfiError { code, detail }`，所有错误都包含错误码和详细描述信息。

## 平台 SDK 集成

本项目不直接面向应用开发者，而是通过以下平台 SDK 间接使用：

| 平台 SDK | 说明 |
|----------|------|
| [privchat-sdk-kotlin](https://github.com/privchat/privchat-sdk-kotlin) | Kotlin Multiplatform（Android + iOS + Desktop） |
| [privchat-sdk-swift](https://github.com/privchat/privchat-sdk-swift) | Swift 原生（iOS / macOS） |
| [privchat-sdk-android](https://github.com/privchat/privchat-sdk-android) | Android 原生 |

## 相关文档

- [架构规范](docs/architecture-spec.md) — Actor 模型 / local-first / FFI 约束
- [公开 API v2](docs/public-api-v2.md) — SDK 公开 API 与调用流程
