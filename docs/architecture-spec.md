# privchat-rust 新 SDK 架构规范（Spec）

## 1. 文档目标

本文档定义 `privchat-rust` 作为未来主线 SDK 的目标架构与迭代约束，确保后续开发在同一技术路线下推进，避免再次出现调用模型分裂、线程模型不一致、FFI 桥接不稳定等问题。

适用范围：

- Rust 核心库：`crates/privchat-sdk`
- Rust FFI：`crates/privchat-sdk-ffi`
- Kotlin/Apple 绑定调用侧（通过 UniFFI 生成）

---

## 2. 架构原则

1. 单一调用模型  
只允许 `UniFFI Object + async` 主链路，不保留同步 helper、by-handle、snapshot 等并行入口。

2. 单一并发入口  
业务请求通过 Actor 命令队列进入核心，禁止业务路径直接抢共享 `RwLock` 资源。

3. 状态机优先  
连接、鉴权、同步必须由明确状态机驱动，禁止隐式状态跳转。

4. 错误结构化  
对外错误必须可分类（可重试/不可重试、网络/协议/鉴权/内部），禁止只返回无结构字符串。

5. 可观测与可复现  
每条关键链路必须具备统一日志字段和自动化复现脚本门禁。

6. 结构化关闭与可取消  
所有异步任务必须可取消、可追踪、可有界关闭，禁止“悬挂任务”与不确定 Drop 行为。

---

## 3. 目标目录结构

```text
privchat-rust/
  crates/
    privchat-sdk/
      src/
        lib.rs
        runtime/
          mod.rs
          runtime_provider.rs
        task/
          mod.rs
          task_registry.rs
          task_handle.rs
        actor/
          mod.rs
          transport_actor.rs
          session_actor.rs
          sync_actor.rs
        state/
          mod.rs
          connection_state.rs
          invariants.rs
        transport/
          mod.rs
          endpoint_policy.rs
        error/
          mod.rs
          sdk_error.rs
        api/
          mod.rs
          client.rs
          types.rs
          connect.rs
          auth.rs
          sync.rs
    privchat-sdk-ffi/
      src/
        lib.rs
        ffi_error.rs
      uniffi.toml
      examples/
        basic.rs
  docs/
    architecture-spec.md
```

---

## 4. Runtime 模型

### 4.1 约束

- SDK 内部不得在业务对象 `Drop` 阶段触发不受控 runtime 关闭。
- SDK 不允许每个 API 临时创建 runtime。
- FFI 方法必须是 async（由 UniFFI future 承载），不得包装同步 `block_on` 主路径。
- 默认模式下 SDK 不创建 runtime，只接收外部 `tokio::runtime::Handle`（测试/CLI 可显式 opt-in 自建）。

### 4.2 目标实现

- `RuntimeProvider` 统一提供 `tokio::runtime::Handle`。
- `PrivchatSdk` 仅持有 `Handle` 与 actor 通道，不拥有“不可控自建 runtime 生命周期”。

---

## 5. 任务模型与关闭语义（P0）

### 5.1 TaskRegistry 约束

- 所有 `spawn` 必须经 `TaskRegistry` 注册，返回 `TaskHandle`。
- 禁止“裸 spawn”后丢弃句柄。
- `TaskHandle` 必须支持：`cancel()`、`is_finished()`、`join(timeout)`。

### 5.2 shutdown() 语义

- `shutdown()` 必须幂等。
- 执行流程：停止接收新命令 -> 广播取消 -> 有界等待任务退出 -> 清理资源。
- `shutdown()` 后任何业务 API 返回 `StateError(Shutdown)`，不得挂起。

### 5.3 Drop 语义

- Drop 不做阻塞等待。
- Drop 只触发 best-effort 取消信号，真正资源回收由 `shutdown()` 保证。

---

## 6. Actor 分层

### 6.1 Actor 划分

1. `TransportActor`  
负责连接建立、端点回退、心跳、断连检测。

2. `SessionActor`  
负责 login/authenticate、token 生命周期、会话上下文。

3. `SyncActor`  
负责 bootstrap/增量同步任务队列、同步状态推进。

### 6.2 通信规则

- API 层只向 Actor 发送命令，不直接访问底层 client。
- Actor 之间通过消息传递协作，不共享可变状态对象。
- 禁止持锁跨 `await`。
- 跨 actor request-response 必须带 `request_id` 与 timeout。

---

## 7. 连接与鉴权状态机

状态定义：

- `Disconnected`
- `Connecting`
- `Connected`
- `Authenticating`
- `Authenticated`
- `Syncing`
- `Ready`
- `Error`

状态迁移规则：

- `connect()` 只允许 `Disconnected -> Connecting -> Connected`
- `login()` 只允许 `Connected -> Authenticating -> Authenticated`
- `authenticate()` 只允许 `Connected/Authenticated`（幂等）
- `runBootstrapSync()` 只允许 `Authenticated -> Syncing -> Ready`
- 任一失败迁移到 `Error`，并带结构化错误与可重试标记

### 7.1 并发与幂等策略（P0）

- 同类命令并发调用时，必须定义统一策略（二选一并全局一致）：
  - `InFlightDedup`：相同参数复用同一 in-flight 结果
  - `FailFast`：返回 `StateError(InProgress)`
- `authenticate` 视为幂等操作：
  - 相同参数允许复用 in-flight
  - 冲突参数返回 `StateError(Conflict)`
- 非法重入（如 `login` 于 `Connecting`）必须立即返回结构化状态错误。

---

## 8. 端点策略（Endpoint Policy）

### 8.1 默认策略

- 优先级：`quic -> tcp -> websocket`
- 每端点独立超时（硬超时）
- 总超时按端点数量动态计算
- 支持策略扩展：顺序回退（默认）与 Happy-Eyeballs（可选）

### 8.2 强制要求

- 不得因首端点阻塞导致后续端点不执行。
- 连接失败日志必须包含：协议、host、port、耗时、失败类型。
- 并发探测模式下，第一个成功端点赢得连接，其余探测必须安全取消。

---

## 9. FFI 规范（UniFFI）

### 9.1 允许导出

- `PrivchatClient::new(config)`
- `connect() async`
- `login(...) async`
- `authenticate(...) async`
- `run_bootstrap_sync() async`
- `connection_state()`
- `shutdown() async`

### 9.2 禁止项

- 禁止 `run_sync`/`block_on` 导出主链路。
- 禁止 by-handle 全局注册表 API。
- 禁止 snapshot 风格“旁路读状态”接口绕过状态机。
- 禁止导出内部 actor 细节类型（仅允许稳定 DTO）。

### 9.1 线程语义

- FFI 对象线程语义必须明确（`Send/Sync` 或“内部单线程 actor 封装”）。
- 不允许“未声明线程语义”的跨线程可变访问。

---

## 10. Kotlin/Apple 调用侧规范

1. 仅使用 UniFFI 生成的 `suspend` 接口。
2. 禁止手写 `rust_future_poll_*` 轮询桥接逻辑。
3. 禁止额外同步包装（`runBlocking + 同步 FFI`）作为主流程。
4. 业务层只处理结构化 SDK 结果与状态事件。
5. 所有取消必须透传到 Rust future（协程 cancel -> FFI cancel）。

---

## 11. 错误模型规范

统一错误类型建议：

- `TransportError`
- `AuthError`
- `ProtocolError`
- `StateError`
- `TimeoutError`
- `InternalError`

每个错误应包含：

- `code`（稳定错误码）
- `message`
- `retryable: bool`
- `context`（endpoint、stage、request_id）

### 11.1 错误码来源与映射

- 业务错误码唯一来源：`privchat_protocol::ErrorCode`（数值码，稳定不可复用）。
- 传输层应优先透传协议码：
  - `RpcResponse.code`（`privchat-protocol/src/protocol.rs`）
  - 认证失败使用 `AuthorizationResponse.error_code`（需要与 `ErrorCode` 对齐为可解析码值）
- SDK 本地错误（如 ActorClosed、Timeout、State 冲突）不得伪造成业务协议码。
  - 对外以“结构化错误类型 + 可选 protocol_code”返回。
  - 若无协议码，则 `protocol_code = None`，并填充 `category`/`retryable`/`context`。

---

## 12. 可观测性规范

每条链路必须输出统一字段：

- `request_id`
- `stage`（connect/login/auth/sync）
- `endpoint`（proto://host:port）
- `elapsed_ms`
- `result`（ok/fail）
- `error_code`（失败时）
- `sequence_id`（事件流序号）

日志层级要求：

- 正常路径：`info`
- 端点回退：`warn`
- 状态机非法迁移/内部不变量破坏：`error`

---

## 13. 事件流契约（P0）

### 13.1 统一事件接口

- 提供统一 `EventStream`（连接状态、消息、同步、错误事件）。
- `connection_state()` 与事件流状态必须一致，状态机为唯一真源。

### 13.2 回放与背压

- 至少支持 replay 最近连接状态事件。
- 必须定义背压策略（固定 buffer + drop 策略）并文档化。
- 每个事件必须带 `sequence_id`，便于追踪与重放。

---

## 14. 测试与门禁

### 14.1 必须保留

- `crates/privchat-sdk-ffi/examples/basic.rs`
- 自动复现脚本：`privchat-sdk-kotlin/scripts/ios-auto-repro.sh`

### 14.2 门禁要求

每次核心改动必须通过：

1. Rust 例子链路：`connect -> login -> authenticate`
2. iOS 自动化：build + install + auto-login + log capture
3. 无 crash 文件（`.ips`）或可解释失败原因
4. 并发门禁：并发 `connect/login/authenticate` 行为符合幂等策略
5. 取消门禁：发起后取消不得泄露任务、不得导致状态机异常
6. 重连门禁：断链恢复后状态与事件序列满足预期

---

## 15. 迭代里程碑

### M1：模型收口（当前优先）

- 完成 runtime provider 化
- 移除剩余旧桥接代码路径
- FFI 只保留 object async 导出

验收标准：

- iOS 日志中不再出现旧桥接符号调用链
- `basic.rs` 稳定通过

### M2：状态机与错误模型

- 完成连接/鉴权/同步状态机
- 错误码稳定化与 retryable 语义
- 完成并发幂等策略与取消语义落地

验收标准：

- 非法调用返回 `StateError` 而非挂起
- 端点失败可观测字段齐全
- 并发和取消门禁用例通过

### M3：事件流与生产化

- 增加消息/连接状态事件流
- 完成同步与重连策略
- 完成 replay/backpressure/sequence_id

验收标准：

- 业务侧可只靠事件驱动刷新状态
- 断线重连行为可回归测试
- 事件流契约测试通过

---

## 16. 非目标（当前阶段不做）

- 兼容旧 by-handle API
- 兼容旧同步 helper 调用模型
- 为历史不一致接口做长期双轨支持

---

## 17. 版本与兼容策略

- 主版本在本次架构重构后可升级为不兼容版本（例如 `0.x` 内部破坏性调整允许）。
- 对外仅承诺 `UniFFI object async` 接口集合的稳定性。
