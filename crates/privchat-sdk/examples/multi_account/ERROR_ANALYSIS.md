# multi_account 错误分析（重新过一遍）

## 一、RPC 响应格式（根本原因）

服务端 RPC 统一返回 **`RPCMessageResponse { code, message, data }`**：
- `router.handle()` 里：`Ok(data) => RPCMessageResponse::success(data)`，即 handler 的返回值放进 **`data`**。
- 客户端 `call_rpc()` 只取 **`rpc_response.data`** 再给 `call_rpc_typed` 反序列化。

因此：
- Handler 返回的 **就是** 业务响应体（不要自己再包一层 code/message）。
- 客户端反序列化的类型必须和 **data 字段内容** 一致。

## 二、本次运行仍报错的原因

当前日志里仍出现：
- Phase 17：`invalid type: map, expected a boolean`
- Phase 21/24：`missing field 'code'`、`invalid type: boolean 'true', expected struct UnsubscribePresenceResponse`

说明 **当时连上的服务端是旧版本**（未包含我们之前的修改）：
- 旧版 reaction 返回 `{ "success": true, "message": "..." }`，客户端按 `bool` 反序列化 → 得到 map，报错。
- 旧版 subscribe 只返回 `{ "initial_statuses": ... }`，没有 `code`；unsubscribe 返回裸 `true`，客户端按 `UnsubscribePresenceResponse` 反序列化 → 报错。

当前代码已改为：
- reaction add/remove：handler 返回 `Ok(json!(true))` → `data = true` → 客户端反序列化 `bool` 正常。
- presence subscribe：返回 `SubscribePresenceResponse { code, message, initial_statuses }` → `data` 含 code；unsubscribe 返回 `{ code, message }` → `data` 为结构体。

**结论**：确认已 **重新编译并重启 privchat-server** 后再跑 multi_account，Phase 17 / 21 / 24 应通过。

## 三、已做修复汇总

| 问题 | 原因 | 修复 |
|------|------|------|
| Phase 3 群组消息 FK 违反 | `send_message("alice", gid, ...)` 把群组 channel_id 当用户 ID，走了 `get_or_create_direct_channel(gid)` | 新增 `send_message_to_channel(from, channel_id, content)`，Phase 3 改为调用该接口 |
| Phase 16 回复消息「不能与自己创建私聊会话」 | `send_message("bob", bob_id, ...)` 即 Bob 发给自己 | 改为 `send_message("bob", alice_id, ...)`，Bob 回复给 Alice |
| Phase 17 反应反序列化失败 | 服务端曾返回 `{ success, message }`，客户端期望 `bool` | 服务端改为 `Ok(json!(true))`，客户端反序列化 `data` 为 bool |
| Phase 21/24 在线状态反序列化失败 | 服务端曾返回无 code 的 subscribe、裸 true 的 unsubscribe | 协议与 handler 改为带 `code`/`message` 的 Subscribe/UnsubscribePresenceResponse，服务端返回对应结构 |

## 四、未修项（非阻断）

- **下载消息缩略图失败：解析 content JSON 失败**  
  本地把 **纯文本消息的 content** 当 JSON 解析导致。属 SDK 展示/缩略图逻辑，需在解析前判断 content 是否为 JSON 或区分文本/富文本，可单独改。
- **Failed to broadcast event: channel closed**  
  测试收尾时 channel 已关闭，属正常生命周期，可忽略。

## 五、建议自测步骤

1. 停掉所有旧 privchat-server 进程。
2. 在项目根目录执行：`cargo build -p privchat-server -p privchat-protocol`，再启动 server。
3. 运行 multi_account：  
   `cd privchat-sdk/crates/privchat-sdk/examples/multi_account && cargo run`
4. 确认 Phase 3、16、17、21、24 无上述错误；若仍有，再看当时 server 进程是否确为刚编译的二进制。
