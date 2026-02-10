# Public API V2 (privchat-rust)

本文档定义新 SDK 对外稳定接口（Rust 语义层），作为后续版本演进与兼容基线。

## 1. Core Object

- `PrivchatSdk::new(config)`
- `PrivchatSdk::with_runtime(config, runtime_provider)`

## 2. Lifecycle Flow (strict)

1. `connect()`
2. `login()` / `register()`
3. `authenticate()`
4. `run_bootstrap_sync()`
5. 进入 local-first 业务 API

约束：
- 未执行 `run_bootstrap_sync()` 时，任何本地数据路径 API 返回 `InvalidState`。

## 3. State / Session

- `connection_state()`
- `is_connected()`
- `session_snapshot()`
- `is_bootstrap_completed()`
- `clear_local_state()`
- `shutdown()`

## 4. Data Domains

- Message / Queue
- Channel / ChannelExtra
- User / Friend / Group / Member
- Reaction / Mention / Reminder
- Presence / Typing
- Sync (`sync_entities/sync_channel/sync_all_channels`)

## 5. Data Identity Rules

- 本地唯一主键：`message.id`（message 表 `id`）
- 服务端消息标识：`server_message_id`
- 队列仅使用 `message.id`；不再依赖 `local_message_id`。

## 6. Storage Rules

- 数据目录：`users/<uid>/...`
- 子目录：
  - `messages.db`（rusqlite + SQLCipher）
  - `kv`（sled）
  - `queues/{normal,file-0,file-1,file-2}`
  - `media`
- 迁移：`refinery`，脚本命名 `VYYYYMMDDHHMMSS__*.sql`

## 7. Concurrency Rules

- API -> Actor 命令队列（单入口）
- 禁止业务路径直接共享锁并发抢占
- 禁止同步桥接主路径（`block_on/run_sync`）

## 8. Error Rules

- 错误码统一 `u32`（协议错误码来源 `privchat-protocol`）
- 结构化错误：`code + detail + category + retryable`

## 9. FFI/Kotlin Rules

- 仅使用 UniFFI 生成的 async/suspend 调用
- 禁止手写 poll/阻塞桥接
- 每次改动必须通过 auto-repro 门禁（build + run + auto-login + log capture）
