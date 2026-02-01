# PrivChat 推送系统测试

> **目标**: 测试推送系统的各种场景，验证推送状态和服务端推送日志

---

## 📋 测试场景

### 用户配置
- **用户A**: 1个模拟设备（发送方）
- **用户B**: 2个模拟设备（接收方）

### 测试用例

1. **场景 1**: 用户B全部设备在线 → 不推送
2. **场景 2**: 用户B全部设备离线 → 推送
3. **场景 3**: 用户B部分设备在线 → 只给离线设备推送
4. **场景 4**: 用户B设备 apns_armed=true → 推送
5. **场景 5**: 用户B设备 apns_armed=false → 不推送
6. **场景 6**: 消息发送成功 → 取消 Push Intent
7. **场景 7**: 消息撤销 → 撤销 Push Intent
8. **场景 8**: 用户B设备上线 → 取消 Push Intent

---

## 🚀 运行测试

### 前置条件

1. **启动服务器**
```bash
cd privchat-server
cargo run
```

2. **配置环境变量（可选）**
```bash
export PRIVCHAT_SERVER_URL="ws://127.0.0.1:8081"
```

### 运行测试

```bash
cd privchat-sdk/crates/privchat-sdk/examples/push_test
cargo run --bin push_test
```

---

## 📊 观察日志

### 服务端日志（privchat-server）

观察以下关键日志：

1. **在线状态检查**
   - `[PUSH PLANNER] User {} is online, skip push`
   - `[PUSH PLANNER] User {} is offline, generating push intent`

2. **Intent 生成**
   - `[PUSH PLANNER] Intent sent to worker: intent_id={}`

3. **Intent 处理**
   - `[PUSH WORKER] Processing intent: intent_id={}`
   - `[PUSH WORKER] Intent {} is revoked, skipping`
   - `[PUSH WORKER] Intent {} is cancelled, skipping`

4. **Provider 调用**
   - `[PUSH Provider] send: task_id={}`

5. **事件发布**
   - `MessageDelivered.*published`
   - `MessageRevoked.*published`
   - `DeviceOnline.*published`

---

## 🔍 验证点

### 场景 1: 全部设备在线
- ✅ 不应该看到 "generating push intent"
- ✅ 应该看到 "User {} is online, skip push"

### 场景 2: 全部设备离线
- ✅ 应该看到 "generating push intent"
- ✅ 应该看到 "PUSH WORKER.*Processing intent"
- ✅ 应该看到 "Provider.*send"

### 场景 3: 部分设备在线
- ✅ 应该看到消息通过长连接发送到在线设备
- ✅ 应该看到为离线设备生成 Push Intent

### 场景 4: apns_armed=true
- ✅ 应该看到生成 Push Intent

### 场景 5: apns_armed=false
- ✅ 不应该看到生成 Push Intent

### 场景 6: 消息发送成功
- ✅ 应该看到 "MessageDelivered.*published"
- ✅ 应该看到 "Intent.*marked as cancelled"
- ✅ 应该看到 "Intent.*is cancelled.*skipping"

### 场景 7: 消息撤销
- ✅ 应该看到 "MessageRevoked.*published"
- ✅ 应该看到 "Intent.*marked as revoked"
- ✅ 应该看到 "Intent.*is revoked.*skipping"

### 场景 8: 设备上线
- ✅ 应该看到 "DeviceOnline.*published"
- ✅ 应该看到 "Intent.*marked as cancelled"
- ✅ 应该看到 "Intent.*is cancelled.*skipping"

---

## 📝 注意事项

1. **数据库准备**: 确保数据库已运行迁移（包含 `privchat_user_devices` 表）
2. **Redis 准备**: 确保 Redis 已启动（用于 Presence 检查）
3. **日志级别**: 建议设置 `RUST_LOG=push_test=debug,privchat_sdk=info,privchat_server=info`

---

## 🐛 调试

如果测试失败，检查：

1. **服务器是否运行**
2. **数据库连接是否正常**
3. **Redis 连接是否正常**
4. **用户是否成功注册/登录**
5. **设备是否成功连接**
6. **RPC 调用是否成功**

---

**测试项目版本**: v1.0  
**创建时间**: 2026-01-27
