# Privchat SDK API Contract

> **本文档是 Swift 与 Kotlin SDK 的单一真实契约**  
> Swift / Kotlin 必须 100% 对齐此 Contract，仅在「允许的差异」范围内做平台最小化处理。

## 设计原则

**The Swift and Kotlin SDKs expose a unified, platform-agnostic API contract. All public APIs are semantically identical across platforms, differing only where required by language or runtime constraints.**

| 维度 | 约束 |
|------|------|
| 逻辑一致 | 相同输入 → 相同语义行为 |
| 命名一致 | 函数名、参数名、类型名 1:1 对应 |
| 调用顺序一致 | 典型流程（connect → bootstrap → send）无歧义 |
| 返回语义一致 | 成功/失败、可选值、集合含义一致 |
| 错误语义一致 | 统一 SdkError 模型，不暴露平台原生错误 |

### 允许的差异（仅此三类）

1. **语言语法差异**：`async/await` vs `suspend`
2. **平台类型差异**：`URL` vs `File` vs `String` path
3. **平台资源生命周期**：线程、调度器、主线程回调

---

## 一、Client（连接与生命周期）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `connect()` | 建立网络连接，初始化 SDK（若未初始化） | — | `void` | Disconnected, NotInitialized, Network, Timeout |
| `disconnect()` | 断开连接 | — | `void` | — |
| `isConnected()` | 是否已连接 | — | `bool` | — |
| `connectionState()` | 获取连接状态 | — | `ConnectionState` | — |
| `currentUserId()` | 当前认证用户 ID | — | `u64?` | — |
| `shutdown()` | 完全关闭 SDK，释放资源 | — | `void` | — |

**典型调用顺序**：`connect()` → `authenticate()` 或 `register()`/`login()` → 业务调用 → `disconnect()` → `shutdown()`

---

## 二、Auth（账号与认证）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `register(username, password, deviceId)` | 注册新账号 | String, String, String | `AuthResult` | Authentication, InvalidParameter, Network |
| `login(username, password, deviceId)` | 登录 | String, String, String | `AuthResult` | Authentication, InvalidParameter, Network |
| `authenticate(userId, token, deviceId)` | 使用 token 认证 | u64, String, String | `void` | Authentication, InvalidParameter |
| `logout()` | 登出 | — | `void` | — |

---

## 三、Messaging（消息）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `sendText(channelId, text)` | 发送文本消息 | u64, String | `localMessageId` (u64) | Disconnected, InvalidParameter, Network |
| `sendText(channelId, text, options?)` | 发送文本（支持回复、提及） | u64, String, SendMessageOptions? | `localMessageId` | 同上 |
| `sendMedia(channelId, media, options?)` | 发送媒体 | u64, MediaInput, SendMessageOptions? | `(localMessageId, AttachmentInfo)` | 同上 + UploadFailed |
| `retryMessage(messageId)` | 重试发送失败消息 | u64（message.id，本地消息主键） | `void` | InvalidParameter, Disconnected |
| `markAsRead(channelId, messageId)` | 标记消息已读 | u64, u64 | `void` | Disconnected |
| `markFullyReadAt(channelId, messageId)` | 标记到指定消息为已读 | u64, u64 | `void` | Disconnected |
| `revokeMessage(messageId)` | 撤回消息 | u64 | `void` | Disconnected, InvalidParameter |
| `editMessage(messageId, newContent)` | 编辑消息 | u64, String | `void` | 同上 |
| `addReaction(messageId, emoji)` | 添加反应 | u64, String | `void` | 同上 |
| `removeReaction(messageId, emoji)` | 移除反应 | u64, String | `void` | 同上 |

**MediaInput 语义**（平台可做类型适配）：
- `image(sourcePath, quality)`：图片，quality 如 P720, P1080
- `video(sourcePath, quality)`
- `audio(sourcePath)`
- `file(sourcePath, mimeType?)`

---

## 四、Storage（存储与查询）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `getMessages(channelId, limit, beforeSeq?)` | 获取消息列表 | u64, u32, u64? | `List<MessageEntry>` | Database, Disconnected |
| `getMessageById(messageId)` | 按 message_id 查询 | u64 | `MessageEntry?` | Database |
| `paginateBack(channelId, limit)` | 向后分页（更早消息） | u64, u32 | `List<MessageEntry>` | Database, Disconnected |
| `paginateForward(channelId, limit)` | 向前分页（更新消息） | u64, u32 | `List<MessageEntry>` | 同上 |
| `getChannels(limit, offset)` | 获取会话列表 | u32, u32 | `List<ChannelListEntry>` | Database |
| `getFriends(limit?, offset?)` | 获取好友列表 | u32?, u32? | `List<FriendEntry>` | Database |
| `getGroups(limit?, offset?)` | 获取群组列表 | u32?, u32? | `List<GroupEntry>` | Database |
| `getGroupMembers(groupId, limit?, offset?)` | 群成员列表 | u64, u32?, u32? | `List<GroupMemberEntry>` | Database |

---

## 五、Sync（同步）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `runBootstrapSync()` | 执行启动同步 | — | `void` | Disconnected, Network, Timeout |
| `runBootstrapSyncInBackground()` | 后台执行启动同步 | — | `void` | — |
| `isBootstrapCompleted()` | 启动同步是否完成 | — | `bool` | Database |
| `syncEntities(type, scope?)` | 同步指定实体 | String, String? | `u32` (count) | Disconnected, Network |
| `syncChannel(channelId, channelType)` | 同步单个频道 | u64, u8 | `SyncStateEntry` | Disconnected, Network |
| `syncAllChannels()` | 同步所有频道 | — | `List<SyncStateEntry>` | 同上 |
| `getChannelSyncState(channelId, channelType)` | 获取频道同步状态 | u64, u8 | `SyncStateEntry` | Database |
| `needsSync(channelId, channelType)` | 是否需要同步 | u64, u8 | `bool` | Database |
| `startSupervisedSync(observer)` | 启动受监督同步 | SyncObserver | `void` | Disconnected |
| `stopSupervisedSync()` | 停止受监督同步 | — | `void` | — |

---

## 六、Channels（会话操作）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `markChannelRead(channelId, channelType)` | 标记会话已读 | u64, u8 | `void` | Disconnected |
| `pinChannel(channelId, pin)` | 置顶/取消置顶 | u64, bool | `bool` | Database |
| `hideChannel(channelId)` | 隐藏会话（本地） | u64 | `bool` | Database |
| `muteChannel(channelId, muted)` | 静音/取消静音 | u64, bool | `bool` | Database |
| `channelUnreadStats(channelId)` | 未读统计 | u64 | `UnreadStats` | Database |
| `ownLastRead(channelId)` | 最后已读位置 | u64 | `LastReadPosition` | Database |
| `setChannelNotificationMode(channelId, mode)` | 通知模式 | u64, NotificationMode | `void` | Database |
| `getOrCreateDirectChannel(peerUserId)` | 获取或创建私聊会话 | u64 | `GetOrCreateDirectChannelResult` | Disconnected, Network |

---

## 七、Friends & Groups（好友与群组）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `searchUsers(query)` | 搜索用户 | String | `List<UserEntry>` | Network |
| `sendFriendRequest(toUserId, remark?, searchSessionId?)` | 发送好友请求 | u64, String?, String? | `u64` (request id) | Network, InvalidParameter |
| `acceptFriendRequest(fromUserId)` | 接受好友请求 | u64 | `u64` | Network |
| `rejectFriendRequest(fromUserId)` | 拒绝好友请求 | u64 | `bool` | Network |
| `deleteFriend(userId)` | 删除好友 | u64 | `bool` | Network |
| `listFriendPendingRequests()` | 待处理好友请求 | — | `List<FriendPendingEntry>` | Database |
| `createGroup(name, memberIds)` | 创建群组 | String, List<u64> | `GroupCreateResult` | Network |
| `inviteToGroup(groupId, userIds)` | 邀请进群 | u64, List<u64> | `bool` | Network |
| `removeGroupMember(groupId, userId)` | 移除群成员 | u64, u64 | `bool` | Network |
| `leaveGroup(groupId)` | 退出群组 | u64 | `bool` | Network |
| `joinGroupByQrcode(qrcode)` | 二维码加群 | String | `GroupQRCodeJoinResult` | Network |

---

## 八、Presence & Typing（在线状态与输入状态）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `subscribePresence(userIds)` | 订阅在线状态 | List<u64> | `List<PresenceEntry>` | Disconnected |
| `unsubscribePresence(userIds)` | 取消订阅 | List<u64> | `void` | — |
| `getPresence(userId)` | 查询缓存状态 | u64 | `PresenceEntry?` | — |
| `batchGetPresence(userIds)` | 批量查询缓存 | List<u64> | `List<PresenceEntry>` | — |
| `fetchPresence(userIds)` | 从服务器查询 | List<u64> | `List<PresenceEntry>` | Network |
| `sendTyping(channelId)` | 发送输入状态 | u64 | `void` | Disconnected |
| `stopTyping(channelId)` | 停止输入 | u64 | `void` | Disconnected |

---

## 九、File（文件上传/下载）

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `sendAttachmentFromPath(channelId, path, options?, progress?)` | 从路径上传 | u64, Path, SendMessageOptions?, ProgressObserver? | `(u64, AttachmentInfo)` | UploadFailed, InvalidParameter |
| `sendAttachmentBytes(channelId, filename, mimeType, data, options?, progress?)` | 从内存上传 | u64, String, String, Bytes, SendMessageOptions?, ProgressObserver? | `(u64, AttachmentInfo)` | 同上 |
| `downloadAttachmentToCache(fileId, fileUrl, progress?)` | 下载到缓存 | String, String, ProgressObserver? | `Path` | Network, PermissionDenied |
| `downloadAttachmentToPath(fileUrl, outputPath, progress?)` | 下载到指定路径 | String, Path, ProgressObserver? | `void` | 同上 |

---

## 九.1、VideoProcessHook（视频缩略图与压缩回调）

发送视频时，SDK 需平台提供**视频缩略图生成**和**视频压缩**能力。未设置时，视频缩略图使用 1x1 透明 PNG 占位上传。

| 方法 | 语义 | 参数 | 返回 | 错误 |
|------|------|------|------|------|
| `setVideoProcessHook(hook?)` | 设置视频处理回调 | VideoProcessHook? | `void` | — |
| `removeVideoProcessHook()` | 移除视频处理回调 | — | `void` | — |

**MediaProcessOp**（Rust 底层固定，Swift/Kotlin 上层必须遵循此命名）：
- `Thumbnail`：生成缩略图
- `Compress`：压缩视频

**VideoProcessHook 回调签名**：

```
(op: MediaProcessOp, sourcePath: Path, metaPath: Path, outputPath: Path) -> Result<Bool, String>
```

| 参数 | 语义 |
|------|------|
| `op` | 当前操作：`Thumbnail` 或 `Compress` |
| `sourcePath` | 源视频文件路径 |
| `metaPath` | meta.json 路径（可读取源信息） |
| `outputPath` | 输出路径：缩略图时写 thumbnail 文件（如 .jpg）；压缩时写压缩后视频 |

**返回值**：`true` 表示成功，`false` 表示跳过（如原视频已满足要求可不压缩）；`Err(String)` 表示失败。

**调用时机**：
- `Thumbnail`：在 prepare 阶段，发送视频前生成缩略图（会话列表/消息气泡预览）
- `Compress`：在上传前，如需压缩则调用（具体时机由 SDK 实现决定）

**平台职责**：实现钩子内完成解码、抽帧/编码、写入 `outputPath`；建议在后台线程执行，避免阻塞主线程。

---

## 十、Events & Observers（事件与观察者）

### 10.1 Delegate（统一回调接口）

| 回调 | 语义 | 参数 |
|------|------|------|
| `onMessageReceived(message)` | 收到新消息 | MessageEntry |
| `onConnectionStateChanged(state)` | 连接状态变化 | ConnectionState |
| `onNetworkStatusChanged(oldStatus, newStatus)` | 网络状态变化 | NetworkStatus, NetworkStatus |
| `onEvent(event)` | 通用事件 | SDKEvent |

### 10.2 观察者注册（返回 token，用于注销）

| 方法 | 语义 | 参数 | 返回 |
|------|------|------|------|
| `observeSends(observer)` | 观察发送状态 | SendObserver | `u64` (token) |
| `unobserveSends(token)` | 取消观察 | u64 | `void` |
| `observeTimeline(channelId, observer)` | 观察消息时间线 | u64, TimelineObserver | `u64` |
| `unobserveTimeline(token)` | 取消观察 | u64 | `void` |
| `observeChannelList(observer)` | 观察会话列表 | ChannelListObserver | `u64` |
| `unobserveChannelList(token)` | 取消观察 | u64 | `void` |
| `observeTyping(channelId, observer)` | 观察输入状态 | u64, TypingObserver | `u64` |
| `unobserveTyping(token)` | 取消观察 | u64 | `void` |
| `observeReceipts(channelId, observer)` | 观察已读回执 | u64, ReceiptsObserver | `u64` |
| `unobserveReceipts(token)` | 取消观察 | u64 | `void` |
| `setDelegate(delegate)` | 设置 Delegate | PrivchatDelegate | `void` |
| `removeDelegate()` | 移除 Delegate | — | `void` |
| `setVideoProcessHook(hook?)` | 设置视频缩略图/压缩回调 | VideoProcessHook? | `void` |
| `removeVideoProcessHook()` | 移除视频处理回调 | — | `void` |

### 10.3 事件语义

**SendUpdate**：`channelId`, `id` (message.id), `serverMessageId?`, `state` (Enqueued | Sending | Sent | Retrying | Failed)

**MessageEntry**：`id`, `serverMessageId?`, `channelId`, `channelType`, `fromUid`, `content`, `status`, `timestamp`

**TypingIndicatorEvent**：`channelId`, `userId`, `isTyping`

**ReadReceiptEvent**：`channelId`, `serverMessageId`, `readerUid`, `timestamp`

---

## 十一、错误模型（SdkError）

**Swift 与 Kotlin 必须统一暴露以下错误类型，不直接暴露平台原生错误（NSError / Exception）。**

```
SdkError
├── Generic { message }
├── Database { message }
├── Network { message, code }
├── Authentication { reason }
├── InvalidParameter { field, message }
├── Timeout { timeoutSecs }
├── Disconnected
├── NotInitialized
├── UploadFailed { message }     // 可选，可归入 Network
└── PermissionDenied { message } // 可选
```

| 变体 | 语义 | 典型场景 |
|------|------|----------|
| Generic | 通用错误 | 无法归类时 |
| Database | 数据库错误 | 存储层异常 |
| Network | 网络错误 | 连接失败、RPC 失败 |
| Authentication | 认证错误 | token 失效、登录失败 |
| InvalidParameter | 参数错误 | 非法 channelId、空内容 |
| Timeout | 超时 | 请求超时 |
| Disconnected | 未连接 | 在未 connect 时调用 |
| NotInitialized | 未初始化 | SDK 未正确初始化 |

---

## 十二、配置（PrivchatConfig）

| 字段 | 类型 | 必需 | 语义 |
|------|------|------|------|
| dataDir | String | ✅ | 数据目录 |
| assetsDir | String | ✅ | SQL 迁移文件目录 |
| serverConfig | ServerConfig | ✅ | 服务器端点列表 |
| connectionTimeout | u64 | ✅ | 连接超时（秒） |
| heartbeatInterval | u64 | ✅ | 心跳间隔（秒） |
| debugMode | bool | — | 调试模式 |
| fileApiBaseUrl | String? | — | 文件服务 API 基础 URL |
| httpClientConfig | HttpClientConfig? | — | HTTP 客户端配置 |

**ServerEndpoint**：`protocol`, `host`, `port`, `path?`, `useTls`

---

## 十三、UniFFI 与 Thin Wrapper 分层

```
Rust Core (privchat-sdk)
        ↓
UniFFI FFI 层 (privchat-ffi)  ← internal，命名可含 FFI 风格
        ↓
Swift/Kotlin Thin Wrapper     ← public API，必须符合本 Contract
        ↓
App
```

**原则**：业务层不直接使用 UniFFI 生成的 API。Thin Wrapper 负责：
- 将 FFI 命名转换为 Contract 命名
- 将 FFI 错误映射为 SdkError
- 提供平台 idiomatic 的 async/suspend 封装
- 处理平台类型（Path ↔ URL ↔ String）

---

## 十四、版本与变更

| 版本 | 说明 |
|------|------|
| v1.0 | 初始 Contract，与 privchat-ffi 当前 API 对齐 |
| v1.1 | 新增 VideoProcessHook：`setVideoProcessHook` / `removeVideoProcessHook`，支持视频缩略图与压缩回调；`MediaProcessOp` 固定为 `Thumbnail` / `Compress` |

**变更规则**：对 Contract 的任意增删改必须同步更新 Swift、Kotlin 实现，并在此文档记录版本变更。
