# privchat-sdk -> privchat-rust 迁移与优化执行文档

## 1. 目标

把旧 `privchat-sdk` / `privchat-ffi` 的能力完整迁移到 `privchat-rust`（`privchat-sdk` + `privchat-sdk-ffi`），并在迁移过程中按新架构做实质优化，而不是“等价搬运”。

目标约束：

1. 只保留 `UniFFI Object + async` 主链路。
2. 不保留 by-handle / snapshot / sync helper 分叉路径。
3. SDK 核心采用 Actor 命令队列，避免业务路径直接争抢共享锁。
4. 迁移过程全程可回归：`basic` + iOS auto-repro 门禁。

---

## 2. 旧 SDK 功能盘点（按域）

### 2.1 连接、鉴权、生命周期

主要位置：

- `crates/privchat-sdk/src/sdk.rs`
- `crates/privchat-sdk/src/client.rs`
- `crates/privchat-sdk/src/lifecycle.rs`
- `crates/privchat-sdk/src/network/mod.rs`

关键能力：

1. 多协议端点解析与回退（`quic://` / `tcp://` / `ws(s)://`）
2. `connect/register/login/authenticate/disconnect/logout`
3. 前后台切换与生命周期 hook
4. 网络状态监听、断线重连控制

### 2.2 消息主链路

主要位置：

- `crates/privchat-sdk/src/sdk.rs`
- `crates/privchat-sdk/src/storage/queue/*`
- `crates/privchat-sdk/src/events.rs`

关键能力：

1. `send_message` 与带 options 的发送
2. 本地队列、重试、优先级、发送恢复
3. 收到消息后的本地落库与事件广播
4. 撤回、编辑、已读、reaction

### 2.3 同步能力

主要位置：

- `crates/privchat-sdk/src/sync/bootstrap.rs`
- `crates/privchat-sdk/src/sync/sync_engine.rs`
- `crates/privchat-sdk/src/sync/entity_sync/*`

关键能力：

1. `run_bootstrap_sync`
2. `sync_entities` / `sync_channel` / `sync_all_channels`
3. 增量 cursor / pts 管理
4. 监督式同步（start/stop supervised sync）

### 2.4 存储子系统

主要位置：

- `crates/privchat-sdk/src/storage/mod.rs`
- `crates/privchat-sdk/src/storage/dao/*`
- `crates/privchat-sdk/src/storage/sqlite.rs`
- `crates/privchat-sdk/src/storage/kv.rs`
- `crates/privchat-sdk/src/storage/media.rs`

关键能力：

1. 用户维度初始化与切换
2. DAO（消息、会话、频道、群组、好友、设置等）
3. kv 缓存/计数器/TTL
4. 媒体索引、附件落盘与清理

### 2.5 实时扩展能力

主要位置：

- `crates/privchat-sdk/src/presence.rs`
- `crates/privchat-sdk/src/typing.rs`
- `crates/privchat-sdk/src/rate_limiter.rs`

关键能力：

1. presence 订阅/查询/批量查询
2. typing 发送/停止
3. 消息/RPC/重连限流

### 2.6 FFI 与 Kotlin 使用面

主要位置：

- `crates/privchat-ffi/src/sdk.rs`
- `crates/privchat-ffi/src/api.udl`
- `privchat-sdk-kotlin/shared/src/iosMain/kotlin/io/privchat/sdk/*`

关键能力：

1. 大量 FFI 导出函数（connect/login/auth/sync/channel/friend/group）
2. 事件/观察者桥接
3. iOS 侧自动化复现链路

---

## 3. 迁移总策略（先可用，再完整，再优化）

### Phase A: 核心链路可用（P0）

范围：

1. `connect/login/authenticate/run_bootstrap_sync`
2. 基础事件流（连接状态 + 错误 + 关键业务事件）
3. FFI/Kotlin 只走 UniFFI async

完成标准：

1. iOS auto-repro 可稳定跑通登录到 bootstrap 完成
2. 无 crash/hang
3. 无同步桥接残留

### Phase B: 业务能力迁移（P1）

范围：

1. 消息发送/回执/撤回/编辑/reaction
2. 会话、频道、好友、群组
3. 搜索、分页、历史拉取

完成标准：

1. FFI 对外 API 覆盖旧 SDK 主要业务能力
2. 旧侧 demo 关键路径可在新 SDK 上跑通

### Phase C: 同步与扩展能力（P1/P2）

范围：

1. entity sync、supervised sync、pts/cursor
2. presence/typing
3. 生命周期 hooks、后台行为策略

完成标准：

1. 同步状态可观测且可恢复
2. 弱网下可持续工作

### Phase D: 深度优化与收敛（P2）

范围：

1. 性能、可观测、错误模型统一
2. 删除旧仓库耦合路径
3. 建立长期演进门禁

---

## 4. 每个迁移点“如何优化”

下面是执行时必须同步做的优化，不允许只做等价搬运。

### 4.0 Local-First 与加密标准化（P0，强制）

这部分是新 SDK 的基础约束，必须在迁移早期落地，不能后置。

**核心原则：严格遵循旧 `privchat-sdk` 的操作流程与语义，不允许在迁移中改变业务时序语义。**

必须遵循的旧流程（基线）：

1. 连接成功后再进行登录与鉴权，不允许“未连接先登录”语义漂移。
2. 发送路径必须保持“本地先入库/入队，再网络投递，再状态推进”语义。
3. 收到服务端消息后必须先完成去重与落库，再触发业务事件。
4. 启动后先恢复本地状态（队列、cursor、会话）再继续增量同步。
5. 用户切换必须按 `init/switch/cleanup` 既有顺序，不允许跳步。

标准化硬约束：

1. 单一写入入口  
所有本地写操作只能经 Actor 命令执行；禁止旁路直接写 DB。

2. 本地状态机统一  
消息生命周期统一为 `draft -> queued -> sent/acked -> failed -> synced`，禁止隐式状态跳转。

3. 事务一致性  
“消息落库 + 出站队列入队”必须同事务；崩溃后可恢复，不丢、不重、不错序。

4. 去重与幂等  
入站/出站都必须有稳定幂等键（`message_id/request_id`）；重复投递不产生重复副作用。

5. 加密标准统一  
统一加密算法/KDF 参数/nonce 规则，并将加密参数版本化写入元数据表（不可隐式升级）。

6. 密钥分层与轮换  
主密钥与数据密钥分层；支持 key version 轮换；禁止明文密钥与明文敏感数据落盘。

7. 迁移与回滚  
数据库迁移必须可重复执行、可校验（版本+checksum）、可回滚（至少提供失败恢复路径）。

8. 可观测与门禁  
每次改动必须通过：本地一致性测试、加密读写测试、迁移测试、断网重放测试、iOS auto-repro。

针对旧流程的优化要求（在“遵循流程”前提下）：

1. 不改变业务语义，仅优化并发模型  
把“多锁竞争”改为 Actor 串行写；不改变外部可见时序。

2. 不改变协议含义，仅优化恢复能力  
cursor/pts 与本地队列恢复流程保持原语义，提升崩溃恢复成功率与速度。

3. 不改变结果集语义，仅优化查询路径  
分页、搜索、会话统计结果与旧 SDK 对齐，允许内部索引与缓存优化。

### 4.1 连接与鉴权

迁移动作：

1. `connect/login/authenticate` 统一进 `SessionActor` / `TransportActor`。
2. 对外只暴露 async API，不做同步包装。

优化动作：

1. 端点策略支持顺序回退 + 可选并发探测（Happy Eyeballs）。
2. 统一连接超时预算：`per-endpoint timeout + total timeout`。
3. API 幂等策略固定：同类并发调用 `InProgress` 或 in-flight 复用（全局统一）。
4. 错误分层：`Transport/Auth/State/Timeout/Internal`，保留 `u32` 协议错误码透传。

### 4.2 消息发送链路

迁移动作：

1. 旧 queue 模块能力映射到 `MessageActor`（或 SessionActor 子命令）。
2. 发送状态回执统一由事件流返回。

优化动作：

1. 队列内存上限 + 持久化恢复策略显式化。
2. 重试策略与限流策略参数化（按 message type / route）。
3. “发送成功后落库”改为“先落库后投递，状态机推进”，避免丢消息。

### 4.3 同步（bootstrap/entity/channel）

迁移动作：

1. `run_bootstrap_sync` 先做 MVP（关键实体）。
2. 再补 `sync_entities/sync_channel/supervised_sync`。

优化动作：

1. cursor/pts 统一存储模型，避免多入口各自维护游标。
2. 同步任务可取消、可超时、可恢复（TaskRegistry 统一管理）。
3. 对每个同步阶段打点：stage、latency、commit 数、失败类型。

### 4.4 存储层

迁移动作：

1. 先迁移必需 DAO（auth/session/channel/message）。
2. 再迁移高级表（reaction/mention/reminder/device）。

优化动作：

1. 明确读写分层：Actor 内串行写，查询可并发读（通过只读快照或专用读接口）。
2. 用户切换流程标准化：init/switch/cleanup 的状态机化。
3. 建立迁移脚本版本门禁（schema drift 检查）。

### 4.5 事件流

迁移动作：

1. 旧 delegate/observer 统一映射到新 EventStream。

优化动作：

1. 事件必须有 `sequence_id`。
2. 定义 replay（至少最后连接状态）与背压（buffer+drop 策略）。
3. 连接状态作为单一事实源，事件和 `connection_state()` 一致。

### 4.6 FFI/Kotlin 层

迁移动作：

1. 删除手写 poll/阻塞桥接，全部走 UniFFI 生成 suspend。
2. FFI 层不暴露内部快照对象和 runtime helper。

优化动作：

1. FFI API 保持小而稳定：对象方法 + DTO，避免泄露内部结构。
2. Kotlin 侧请求路径统一协程取消语义（cancel -> rust future cancel）。
3. 统一崩溃抓取脚本，自动收集 runtime/app/crash/hang 样本。

---

## 5. 迁移执行流程（建议顺序）

### Step 1: 冻结旧接口面

1. 导出现有 FFI API 清单（函数、参数、返回、错误码）。
2. 标记 `Must Have / Should Have / Nice to Have`。

产物：

1. `docs/ffi-api-inventory.md`

### Step 2: 新 SDK API 草案

1. 定义 `PrivchatClient` 新公共 API（Rust 签名）。
2. 定义 `Command/Response` 与状态机迁移表。

产物：

1. `docs/public-api-v2.md`
2. `docs/state-machine-v2.md`

### Step 3: 核心链路替换

---

## 当前实现进度（持续更新）

### 已完成（本轮）

1. `privchat-ffi` 同步桥接清理完成：仅保留 UniFFI Object + async 导出（`connect/login/register/authenticate/run_bootstrap_sync/shutdown`）。
2. 新 `privchat-sdk` 已接入 Actor 主链（单入口命令队列）。
3. 新 `privchat-sdk` 增加本地 SQLite 持久化骨架（`auth_session`）并接入主流程：
   - `login/register/authenticate` 后写入本地会话
   - `run_bootstrap_sync` 完成后写入 `bootstrap_completed`
   - `session_snapshot` / `clear_local_state` 已可通过 API/FFI 调用
4. 端点 URL 解析兼容旧流程：
   - 支持 `quic://`, `tcp://`, `ws://`, `wss://`
   - 默认端口回退逻辑已迁移（缺省 `9001`）
5. 错误码统一策略已落地第一版：
   - SDK 错误映射到 `privchat_protocol::ErrorCode (u32)`
   - FFI 错误结构包含 `code: u32 + detail: String`
6. 自动门禁持续通过：
   - `cargo test -p privchat-sdk -p privchat-sdk-ffi`
   - `build-ios.sh`
   - `scripts/ios-auto-repro.sh`（auto-login 到 MainTab，无 crash/hang）
7. Local-first 与多队列策略已落地第一版：
   - `message.id` 作为本地唯一主键（本地 CRUD/状态推进统一按 `message.id`）
   - `local_message_id` 在入队前由 Snowflake 生成并回写，仅用于服务端幂等去重
   - 发送队列拆分为普通队列 + 文件多队列（`file-0/file-1/file-2`，按 route_key 分流）
8. 用户隔离目录已对齐旧流程：
   - `users/<uid>/messages.db`（SQLite/SQLCipher）
   - `users/<uid>/kv`（sled）
   - `users/<uid>/queues/{normal,file-*}`
   - `users/<uid>/media`
9. 新增会话控制 API（SDK+FFI）：
   - `disconnect / is_connected / ping`
   - `connect` 已修复“stale transport 自动重建”
   - `connection_state` 已导出（`New/Connected/LoggedIn/Authenticated/Shutdown`）
10. 消息已读链路（SDK+FFI）已落地：
   - `set_message_read(message.id, channel_id, channel_type, is_read)`
   - `mark_channel_read(channel_id, channel_type)`
   - `get_channel_unread_count(channel_id, channel_type)`
   - `basic.rs` 已覆盖 unread/read 回归路径
11. 频道本地能力（SDK+FFI）已落地第一版：
   - `upsert_channel / get_channel_by_id / list_channels`
   - `basic.rs` 已加入 channel 本地链路 smoke 验证
12. 会话扩展（`channel_extra`）第一版已落地：
   - `upsert_channel_extra / get_channel_extra`
   - 覆盖草稿、browse_to、keep_pts、keep_offset_y 关键字段
13. Entity Model V1（第一批）已迁移到新 SDK/FFI：
   - `user`: `upsert_user / get_user_by_id / list_users_by_ids`
   - `friend`: `upsert_friend / list_friends`
   - `group`: `upsert_group / get_group_by_id / list_groups`
   - `group_member`: `upsert_group_member / list_group_members`
14. Channel Member（旧 `channel_member` 表）已迁移到新 SDK/FFI：
   - `upsert_channel_member / list_channel_members / delete_channel_member`
   - 对齐旧表字段：`role/status/version/forbidden_expiration_time/member_avatar_cache_key`
   - `LocalStore` 回归测试已覆盖（upsert -> list -> delete）
15. Message Extra 核心语义（第一批）已迁移到新 SDK/FFI：
   - `set_message_revoke(message.id, revoked, revoker)`
   - `edit_message(message.id, content, edited_at)`
   - `set_message_pinned(message.id, is_pinned)`
   - `get_message_extra(message.id)`
   - `LocalStore` 回归测试已覆盖（revoke/edit/pin lifecycle）
16. 未读统计能力补齐（SDK+FFI）：
   - `get_total_unread_count(exclude_muted)`
   - 与 `get_channel_unread_count` 配套，支持“包含静音会话 / 排除静音会话”两种口径
   - `LocalStore` 回归测试已覆盖（`unread_count_lifecycle`）
17. 同步接口补齐（SDK+FFI）：
   - `sync_channel(channel_id, channel_type)`
   - `sync_all_channels()`
   - 统一走 Actor + async 主链，复用 `sync_entities` 网络实现，不引入同步桥接
18. Presence/Typing 基础链路补齐（SDK+FFI）：
   - `subscribe_presence / unsubscribe_presence / fetch_presence`
   - `send_typing(channel_id, channel_type, is_typing, action_type)`
   - 全部走 Actor + async 主链（RPC route: `presence/*`）
19. Supervised Sync 控制面补齐（SDK+FFI）：
   - `start_supervised_sync(interval_secs) / stop_supervised_sync()`
   - `is_supervised_sync_running()`
   - 任务由 `RuntimeProvider + TaskRegistry` 管理，保持 async 架构约束
20. Bootstrap 硬门禁已下沉到 SDK 核心状态检查：
   - 所有本地数据路径命令统一经 `current_uid_required()`
   - 若未执行 `run_bootstrap_sync()`，直接返回 `InvalidState("run_bootstrap_sync required before local-first operations")`
   - iOS auto-repro 回归通过（登录 -> authenticate -> runBootstrapSync -> MainTab）
21. 依赖与内部链路清理：
   - 保留并标准化 `snowflake_me` 生成链路（仅用于 `local_message_id`）
   - 恢复 StorageActor `UpdateLocalMessageId` 命令通路用于入队前回写
   - 保留 LocalStore `update_local_message_id` 与回归测试，确保服务端幂等字段可持续维护

### 进行中（下一批）

1. 数据库层深化迁移（向旧 SDK 对齐）：
   - 用户分库/多用户切换
   - 消息/会话/好友/群组核心表与 DAO
   - 迁移版本管理与一致性校验
   - SQLCipher 参数、Refinery 版本策略（`V年月日时分秒__*.sql`）与旧库完全对齐
2. Local-first 发送链路：
   - “先入库+入队，再网络发送，再状态推进”
   - 幂等键与崩溃恢复
3. 同步域迁移：
   - `sync_entities` 扩展到旧 SDK 实体域
   - cursor/pts 统一持久化与恢复
4. 事件流契约：
   - `sequence_id`、replay、backpressure
5. API 覆盖补齐：
   - 按旧 `privchat-sdk` 的公共接口清单逐项迁移

1. 打通 `connect/login/authenticate/bootstrap`。
2. iOS auto-repro 门禁改为强制执行。

产物：

1. 可运行二进制 + 日志 + crash 结果

### Step 4: 业务域分批迁移

批次建议：

1. 消息与会话
2. 好友与群组
3. 同步与 presence/typing
4. 高级能力（reaction/attachment/lifecycle）

每批都要做：

1. 功能测试
2. 并发测试
3. 取消测试
4. iOS auto-repro

### Step 5: 下线旧路径

1. 删除旧 FFI helper/by-handle/snapshot 入口
2. Kotlin 删除旧 bridge 代码
3. 文档与脚本切换到新 SDK

---

## 6. 门禁与验收标准

### 6.1 必过门禁

1. `cargo check`（`privchat-rust` workspace）
2. `cargo run -p privchat-sdk-ffi --example basic`（connect/login/auth/bootstrap）
3. `privchat-sdk-kotlin/scripts/ios-auto-repro.sh`（自动登录、日志与崩溃采集）

### 6.2 稳定性门禁

1. 并发调用：同一 API 多次并发行为可预测
2. 取消：中途 cancel 不崩溃、不泄露任务
3. 断网重连：状态机正确推进
4. shutdown 幂等：重复调用稳定返回
5. local-first 一致性：断电/崩溃后队列恢复与状态推进正确
6. 加密门禁：加密参数版本、密钥版本、读写解密全链路验证通过
7. 迁移门禁：旧库升级到新 schema 后数据可读且语义不变

---

## 7. 风险与应对

### 风险 1：迁移期间行为漂移

应对：

1. 旧/新双实现对照测试（关键场景 replay）
2. 每批次设“行为差异白名单”

### 风险 2：性能回退

应对：

1. 建立基线指标（登录耗时、发送耗时、同步耗时）
2. 每次大改后跑基准并记录

### 风险 3：iOS 特有崩溃复发

应对：

1. 强制执行 auto-repro 门禁
2. 崩溃样本自动归档（ips/top/runtime/app stdout）

---

## 8. 近期执行清单（按优先级）

P0：

1. 在新 `privchat-rust` 完成 `run_bootstrap_sync` MVP
2. 建立 `ffi-api-inventory.md` 与 `public-api-v2.md`
3. 把 iOS 门禁改成 “login + bootstrap 成功才通过”

P1：

1. 迁移消息发送/历史分页/已读/reaction
2. 迁移好友与群组核心接口
3. 加入并发与取消自动测试

P2：

1. 完整 supervised sync / presence / typing
2. 迁移 lifecycle hooks 与设备相关接口
3. 清理旧仓库冗余路径并冻结新 API
