# Multi Account 测试迁移状态

## 当前状态

### ✅ 已完成（可运行）

**简化版测试** - `simple_test.rs`
- ✅ 多账号连接和认证（3个账号）
- ✅ 消息队列化发送
- ✅ 本地存储 → 队列 → 发送流程
- ✅ 服务器版本信息自动显示
- ✅ 连接状态管理
- ✅ 使用新的 `PrivchatSDK` API

运行命令：
```bash
cd privchat-sdk/crates/privchat-sdk/examples/multi_account
cargo run
```

### 🔄 待迁移（已保留代码）

**完整测试套件** - 20个测试阶段（需要API适配）

代码文件（总计5700+行）：
- `realistic_test_phases.rs` (3947 行) - 20个完整测试阶段实现
- `test_coordinator.rs` (801 行) - 测试协调器和报告生成
- `test_phases.rs` - 测试阶段辅助功能

这些文件保留了完整的测试逻辑，但需要从旧的 `PrivchatClient` API 迁移到新的 `PrivchatSDK` API。

## 完整测试套件内容（20个阶段）

### 核心功能测试

1. **Phase 1: 用户认证和初始化**
   - 并发认证所有账号
   - 验证连接状态

2. **Phase 2: 好友系统完整流程**
   - 用户搜索（通过用户名）
   - 好友请求（发送、接受、拒绝）
   - 好友列表查询
   - 删除好友

3. **Phase 3: 群组系统工作流**
   - 创建群组
   - 邀请成员
   - 设置群组权限
   - 退出群组

4. **Phase 4: 混合场景测试**
   - 私聊+群聊混合发送
   - 消息顺序验证

5. **Phase 5: 消息接收验证**
   - 推送消息接收
   - 事件触发验证

### 高级功能测试

6. **Phase 6: 表情包功能**
   - 表情包列表
   - 添加/删除表情包

7. **Phase 7: 会话列表和置顶**
   - 会话列表查询
   - 会话置顶/取消置顶

8. **Phase 8: 已读回执**
   - 标记消息已读
   - 已读状态同步

9. **Phase 9: 文件上传**
   - 文件上传流程
   - 上传令牌获取
   - 上传回调

10. **Phase 10: 其他消息类型**
    - 位置消息
    - 名片消息

### 消息管理测试

11. **Phase 11: 消息历史查询**
    - 历史消息获取
    - 分页查询

12. **Phase 12: 消息撤回**
    - 撤回自己的消息
    - 撤回事件通知

13. **Phase 13: 离线消息推送**
    - 断线重连
    - 离线消息获取
    - 历史消息同步

14. **Phase 14: PTS同步和队列限制**
    - PTS（Position Tag）同步
    - 离线消息队列限制

### 群组高级功能

15. **Phase 15: 高级群组功能**
    - 群组权限管理
    - 群成员管理
    - 群组设置

16. **Phase 16: 消息回复**
    - 引用消息
    - 回复消息

17. **Phase 17: 消息反应（Reaction）**
    - 添加表情反应
    - 移除表情反应

### 社交功能测试

18. **Phase 18: 黑名单**
    - 添加黑名单
    - 移除黑名单
    - 黑名单消息拦截

19. **Phase 19: @提及功能**
    - @特定用户
    - @全体成员
    - 提及通知

20. **Phase 20: 非好友消息**
    - 陌生人消息
    - 消息验证

## API迁移需求

### 旧API（PrivchatClient）→ 新API（PrivchatSDK）

需要适配的主要方法：

| 功能 | 旧API | 新API状态 |
|------|-------|----------|
| 发送消息 | `client.send_message(channel, content, type)` | ✅ `sdk.send_message(channel, content)` |
| 撤回消息 | `client.revoke_message(msg_id, channel)` | ⚠️ 需实现 `sdk.recall_message()` |
| 标记已读 | `client.mark_as_read(channel, msg_id)` | ✅ `sdk.mark_as_read()` |
| 消息反应 | `client.add_reaction(msg_id, emoji)` | ✅ `sdk.add_reaction()` |
| RPC调用 | `client.rpc_call(route, params)` | ✅ 保留在内部 |

### 需要完成的SDK方法

在 `privchat-sdk/src/sdk.rs` 中需要实现的高级方法：

- [ ] 好友管理
  - [ ] `search_users(query)` - 搜索用户
  - [ ] `send_friend_request(user_id)` - 发送好友请求
  - [ ] `accept_friend_request(request_id)` - 接受好友请求
  - [ ] `get_friend_list()` - 获取好友列表
  - [ ] `delete_friend(user_id)` - 删除好友

- [ ] 群组管理
  - [ ] `create_group(name, members)` - 创建群组
  - [ ] `invite_to_group(group_id, user_ids)` - 邀请入群
  - [ ] `leave_group(group_id)` - 退出群组
  - [ ] `get_group_members(group_id)` - 获取群成员

- [ ] 消息高级功能
  - [ ] `get_message_history(channel_id, limit)` - 获取历史消息
  - [ ] `search_messages(query)` - 搜索消息
  - [ ] `forward_message(msg_id, target_channel)` - 转发消息

- [ ] 黑名单
  - [ ] `add_to_blacklist(user_id)` - 添加黑名单
  - [ ] `remove_from_blacklist(user_id)` - 移除黑名单
  - [ ] `get_blacklist()` - 获取黑名单列表

## 迁移步骤建议

### 阶段1: 核心方法实现（优先）
1. 在 SDK 中实现好友管理相关方法
2. 在 SDK 中实现群组管理相关方法
3. 在 SDK 中实现消息历史查询方法

### 阶段2: 测试适配
1. 适配 Phase 1-5（核心功能）
2. 适配 Phase 6-10（高级功能）
3. 适配 Phase 11-15（消息和群组管理）
4. 适配 Phase 16-20（社交功能）

### 阶段3: 完整测试运行
1. 运行所有20个测试阶段
2. 生成详细测试报告
3. 验证所有功能

## 当前可用功能

虽然完整测试套件需要迁移，但以下核心功能已经可用：

✅ **基础功能**
- 多账号管理
- 连接和认证
- 消息发送（队列化）
- 消息接收
- 服务器版本信息

✅ **高级功能**（SDK已实现）
- 消息已读回执 (`mark_as_read`)
- 消息撤回 (`recall_message`)
- 消息编辑 (`edit_message`)
- 消息反应 (`add_reaction`, `remove_reaction`)
- 输入状态 (`start_typing`, `stop_typing`)

✅ **存储功能**
- SQLite本地存储
- 消息持久化
- 队列持久化（sled KV）

## 运行测试

### 简化版测试（当前可用）
```bash
cd privchat-sdk/crates/privchat-sdk/examples/multi_account
cargo run
```

### 完整测试（待迁移）
```bash
# 取消注释 main.rs 中的相关模块后运行
# 需要先完成API迁移
cargo run
```

## 文件说明

| 文件 | 状态 | 说明 |
|------|------|------|
| `main.rs` | ✅ 使用中 | 当前运行简化测试 |
| `simple_test.rs` | ✅ 使用中 | 3步骤简化测试 |
| `account_manager.rs` | ✅ 使用中 | 账号管理（已适配SDK） |
| `realistic_test_phases.rs` | 🔄 保留 | 20阶段完整测试（需迁移） |
| `test_coordinator.rs` | 🔄 保留 | 测试协调器（需迁移） |
| `test_phases.rs` | 🔄 保留 | 测试辅助（需迁移） |
| `event_system.rs` | ✅ 使用中 | 事件系统 |
| `types.rs` | ✅ 使用中 | 类型定义 |
| `mock_jwt.rs` | ✅ 使用中 | JWT生成 |

## 总结

- ✅ **核心功能可用** - 简化版测试已验证基本功能
- 📦 **完整代码保留** - 5700+行测试代码完整保存
- 🔄 **逐步迁移** - 根据需要逐步适配新API
- 🎯 **目标明确** - 最终恢复完整的20阶段测试

**建议**：先使用简化版测试验证核心功能，后续根据业务需求逐步实现和迁移完整测试。
