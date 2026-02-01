# Sample 统一流程

所有 sample（Android / Swift / Kotlin）遵循相同的 UI 与流程。详见各 SDK 的 `sample/README.md`。

## 界面要求

1. **服务器地址输入**：支持协议格式
   - `quic://127.0.0.1:8082`
   - `wss://127.0.0.1:8081/path`
   - `ws://127.0.0.1:8081`
   - `tcp://127.0.0.1:8080`
2. **账号**：文本输入
3. **密码**：文本输入
4. **【登录】** / **【注册】** 按钮

## 标准流程

1. 使用输入的服务器地址初始化 SDK
2. `connect`
3. 使用输入的账号密码【登录】或【注册】
4. 使用登录/注册返回的 token 调用 `authenticate`
5. `runBootstrapSync`
6. `getChannels` 列表会话
7. 选择一个对话：`sendText` / `sendMedia`（图片） / `retryMessage` / upload progress
8. 网络连接状态监听，断连时 UI 有感知
9. 断网后自动 `reconnect`
10. 好友列表 `getFriends`

## URL 解析

SDK 提供 `parseServerUrl(url)` / `parse_server_url(url)`，上层直接调用即可：

- **Kotlin (FFI)**：`parseServerUrl(serverUrl)` 返回 `ServerEndpoint`，解析失败抛异常
- **Swift**：`try parseServerUrl(url: serverUrl)` 返回 `ServerEndpoint`，解析失败抛异常
- **Builder**：`builder.serverUrl(serverUrl)` 链式调用，解析并添加 endpoint

| 协议 | TransportProtocol | useTls |
|------|-------------------|--------|
| quic:// | Quic | true |
| wss:// | WebSocket | true |
| ws:// | WebSocket | false |
| tcp:// | Tcp | false |

path：URL 中 `/` 后的路径部分（如 `/path`）
