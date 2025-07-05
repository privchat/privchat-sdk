# PrivChat SDK 开发任务清单

## 🎯 项目总览

基于前期架构讨论，构建现代化 IM SDK 的消息处理系统，重点实现：
- 📤 可靠的消息发送队列系统
- 📥 高效的批量消息接收处理
- 🔄 智能重试和崩溃恢复机制
- 📊 完整的消息状态管理

**设计哲学**: SQLite 作为消息状态源，sled 作为动作源，crossbeam-channel 提供高性能内存队列

---

## 🚀 Phase 1: MVP 核心发送队列系统

### ✅ 已完成项目
- [x] rusqlite 升级到 0.36
- [x] 智能 assets 缓存机制
- [x] 基础存储管理器架构
- [x] KV 存储集成

### 📋 待完成任务

#### 1.1 消息状态管理系统
- [ ] **实现完整的 MessageStatus 枚举**
  ```rust
  enum MessageStatus {
      Draft = 0,         // 草稿
      Sending = 1,       // 发送中
      Sent = 2,          // 已发送
      Delivered = 3,     // 已投递
      Read = 4,          // 已读
      Failed = 5,        // 发送失败
      Revoked = 6,       // 已撤回
      Burned = 7,        // 已焚烧
      Retrying = 8,      // 重试中
      Expired = 9,       // 过期
  }
  ```
  - 位置: `src/storage/entities.rs`
  - 依赖: 无
  - 预计时间: 2小时

- [ ] **实现状态流转逻辑**
  - 状态流转验证方法 `can_transition_to()`
  - 状态更新的原子操作
  - 状态变更事件通知
  - 位置: `src/storage/message_state.rs`
  - 依赖: MessageStatus 枚举
  - 预计时间: 4小时

#### 1.2 发送任务数据结构
- [ ] **定义 SendTask 结构体**
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct SendTask {
      pub client_msg_no: String,
      pub channel_id: String,
      pub message_data: MessageData,
      pub created_at: u64,
      pub retry_count: u32,
      pub max_retries: u32,
      pub next_retry_at: Option<u64>,
      pub priority: QueuePriority,
  }
  ```
  - 位置: `src/storage/queue/send_task.rs`
  - 依赖: MessageData 结构
  - 预计时间: 3小时

- [ ] **实现队列优先级系统**
  ```rust
  enum QueuePriority {
      Critical = 0,   // 撤回、删除
      High = 1,       // 文本消息、表情
      Normal = 2,     // 图片、语音
      Low = 3,        // 文件、视频
      Background = 4, // 已读回执、状态同步
  }
  ```
  - 位置: `src/storage/queue/priority.rs`
  - 依赖: 无
  - 预计时间: 2小时

#### 1.3 发送队列管理器
- [ ] **实现 SendQueueManager**
  - crossbeam-channel 内存队列
  - sled 持久化存储集成
  - 任务序列化/反序列化
  - 优先级队列支持
  - 位置: `src/storage/queue/send_manager.rs`
  - 依赖: SendTask, QueuePriority
  - 预计时间: 6小时

- [ ] **实现消息入队逻辑**
  ```rust
  impl StorageManager {
      pub async fn send_message(&self, message: NewMessage) -> Result<String> {
          // 1. 写入 SQLite (status = Sending)
          // 2. 写入 sled (动作源)
          // 3. 推入内存队列
      }
  }
  ```
  - 位置: `src/storage/mod.rs`
  - 依赖: SendQueueManager
  - 预计时间: 4小时

#### 1.4 发送消费器
- [ ] **实现 SendConsumer**
  - 多线程消费逻辑
  - 频道级串行发送保障
  - 网络发送接口抽象
  - 位置: `src/storage/queue/send_consumer.rs`
  - 依赖: SendQueueManager, NetworkSender trait
  - 预计时间: 8小时

- [ ] **实现网络发送抽象层**
  ```rust
  #[async_trait]
  pub trait NetworkSender: Send + Sync {
      async fn send_message(&self, message: &MessageData) -> Result<SendResponse>;
      async fn revoke_message(&self, message_id: &str) -> Result<()>;
  }
  ```
  - 位置: `src/network/sender.rs`
  - 依赖: 无
  - 预计时间: 3小时

#### 1.5 崩溃恢复机制
- [ ] **实现启动时队列恢复**
  ```rust
  impl StorageManager {
      pub async fn recover_send_queue(&self) -> Result<()> {
          // 1. 从 sled 恢复任务
          // 2. 检查 SQLite 中 Sending 状态消息
          // 3. 重建内存队列
      }
  }
  ```
  - 位置: `src/storage/mod.rs`
  - 依赖: SendQueueManager
  - 预计时间: 5小时

---

## 🔄 Phase 2: 重试机制与可靠性

#### 2.1 智能重试系统
- [ ] **实现重试策略**
  - 指数退避算法 (2^retry_count, 最大64秒)
  - 可重试错误分类
  - 最大重试次数控制
  - 位置: `src/storage/queue/retry_policy.rs`
  - 依赖: SendConsumer
  - 预计时间: 6小时

- [ ] **实现网络状态监听**
  - 网络可用性检测
  - 网络恢复时触发队列处理
  - 离线状态下的队列暂停
  - 位置: `src/network/monitor.rs`
  - 依赖: SendQueueManager
  - 预计时间: 4小时

#### 2.2 错误处理与分类
- [ ] **定义发送失败原因**
  ```rust
  enum SendFailureReason {
      NetworkTimeout,     // 网络超时 → 重试
      NetworkUnavailable, // 无网络 → 等待恢复
      ServerError,        // 服务端错误 → 根据错误码
      AuthFailure,        // 认证失败 → 重新登录
      RateLimited,        // 限流 → 延迟重试
      MessageTooLarge,    // 消息过大 → 不重试
      Forbidden,          // 权限不足 → 不重试
  }
  ```
  - 位置: `src/error.rs`
  - 依赖: 无
  - 预计时间: 3小时

#### 2.3 消息去重机制
- [ ] **实现消息去重策略**
  - client_msg_no 唯一性保证
  - 数据库唯一约束
  - 重复消息检测和处理
  - 位置: `src/storage/deduplication.rs`
  - 依赖: 消息数据结构
  - 预计时间: 4小时

---

## 📥 Phase 3: 接收消息批量处理

#### 3.1 接收队列系统
- [ ] **实现 ReceiveQueueManager**
  - 单条消息直接写入
  - 批量消息队列处理
  - 优先级分类 (实时 vs 历史)
  - 位置: `src/storage/queue/receive_manager.rs`
  - 依赖: 无
  - 预计时间: 5小时

- [ ] **实现批量写入优化**
  - 批量 INSERT 事务优化
  - 每50条开启一次事务
  - 写入失败的回滚处理
  - 位置: `src/storage/dao/message.rs`
  - 依赖: ReceiveQueueManager
  - 预计时间: 4小时

#### 3.2 离线消息同步
- [ ] **实现分阶段拉取策略**
  ```rust
  enum SyncPhase {
      Critical,    // 最新关键消息
      Recent,      // 最近聊天消息
      History,     // 历史消息分页
      Metadata,    // 扩展数据
  }
  ```
  - 位置: `src/sync/strategy.rs`
  - 依赖: ReceiveQueueManager
  - 预计时间: 6小时

---

## 📊 Phase 4: 性能优化与监控

#### 4.1 性能监控
- [ ] **实现队列指标收集**
  - 发送成功率统计
  - 平均发送延迟
  - 队列积压监控
  - 重试次数分布
  - 位置: `src/metrics/queue_metrics.rs`
  - 依赖: SendQueueManager, ReceiveQueueManager
  - 预计时间: 4小时

- [ ] **实现性能分析工具**
  - 慢查询检测
  - 内存使用监控
  - 磁盘IO分析
  - 位置: `src/metrics/performance.rs`
  - 依赖: StorageManager
  - 预计时间: 3小时

#### 4.2 缓存优化
- [ ] **实现消息缓存层**
  - 最近消息内存缓存
  - LRU 缓存策略
  - 缓存命中率统计
  - 位置: `src/storage/cache/message_cache.rs`
  - 依赖: 无
  - 预计时间: 5小时

#### 4.3 并发优化
- [ ] **实现频道级并发控制**
  - 每频道独立锁
  - 跨频道并行处理
  - 死锁检测和预防
  - 位置: `src/storage/queue/channel_lock.rs`
  - 依赖: SendConsumer
  - 预计时间: 4小时

---

## 🧪 Phase 5: 测试与文档

#### 5.1 单元测试
- [ ] **发送队列系统测试**
  - 消息发送流程测试
  - 重试机制测试
  - 崩溃恢复测试
  - 位置: `src/storage/queue/tests.rs`
  - 预计时间: 8小时

- [ ] **接收队列系统测试**
  - 批量处理测试
  - 并发安全测试
  - 性能基准测试
  - 位置: `src/storage/queue/receive_tests.rs`
  - 预计时间: 6小时

#### 5.2 集成测试
- [ ] **端到端流程测试**
  - 完整发送接收流程
  - 网络异常场景模拟
  - 大量数据压力测试
  - 位置: `tests/integration/`
  - 预计时间: 10小时

#### 5.3 文档完善
- [ ] **API 文档更新**
  - 队列系统使用指南
  - 配置参数说明
  - 最佳实践建议
  - 位置: `docs/`
  - 预计时间: 6小时

---

## 📅 时间规划

### 第1周 (Phase 1)
- **周一-周二**: 消息状态管理 + 发送任务结构 (9小时)
- **周三-周四**: 发送队列管理器 (6小时)
- **周五**: 消息入队逻辑 + 网络抽象 (7小时)

### 第2周 (Phase 1完成 + Phase 2开始)
- **周一-周三**: 发送消费器实现 (8小时)
- **周四-周五**: 崩溃恢复机制 + 重试策略 (11小时)

### 第3周 (Phase 2完成 + Phase 3)
- **周一-周二**: 错误处理 + 去重机制 (7小时)
- **周三-周五**: 接收队列系统 (9小时)

### 第4周 (Phase 3完成 + Phase 4)
- **周一-周二**: 离线同步策略 (6小时)
- **周三-周五**: 性能监控 + 缓存优化 (12小时)

### 第5周 (Phase 5)
- **整周**: 测试 + 文档 (30小时)

---

## 🔧 开发环境准备

### 依赖检查
- [x] Rust 工具链
- [x] rusqlite 0.36
- [x] sled 存储
- [x] crossbeam-channel
- [x] tokio 异步运行时

### 新增依赖
- [ ] `bincode` - 任务序列化
- [ ] `async-trait` - 网络接口抽象  
- [ ] `metrics` - 性能监控
- [ ] `tracing-futures` - 异步跟踪

---

## 🎯 关键里程碑

1. **MVP 发送队列可用** (第2周结束)
   - 基本发送功能正常
   - 简单重试机制工作
   - 崩溃恢复验证通过

2. **可靠性保障完成** (第3周结束)
   - 复杂错误场景处理
   - 去重机制验证
   - 网络异常恢复测试

3. **完整系统集成** (第4周结束)
   - 发送接收系统协作
   - 性能达到预期指标
   - 监控数据完整

4. **生产就绪** (第5周结束)
   - 所有测试通过
   - 文档完整
   - 性能基准确立

---

## 📋 待讨论事项

- [ ] 网络层的具体实现方案 (HTTP/WebSocket/自定义协议)
- [ ] 大文件上传的队列策略
- [ ] 多端同步的消息状态处理
- [ ] 数据库分表策略 (大量历史消息)
- [ ] 加密消息的队列处理

---

**下一步行动**: 开始 Phase 1.1 - 实现 MessageStatus 枚举和状态流转逻辑 