# PrivChat SDK 基础使用示例（含实体同步）

本示例演示：**SDK 初始化 → 连接 → 认证 → 实体同步（好友 + 群组）→ 本地校验 → 断开**，并说明首次登录全量同步与「同步成功」的标注方式。

## 功能演示

1. ✅ 使用 **PrivchatSDK** 初始化（数据目录、assets、服务器端点）
2. ✅ 建立网络连接（QUIC）
3. ✅ 用户认证（示例使用 Mock JWT；实际应从认证服务获取）
4. ✅ **实体同步**：统一接口 `sync_entities(EntityType::Friend, None)` / `sync_entities(EntityType::Group, None)`（ENTITY_SYNC_V1）
5. ✅ **同步成功标注**：以 `sync_entities` 返回 `Ok(count)` 及本地 `get_friends` / `get_groups` 数量校验
6. ✅ 断开连接

## 首次登录与全量 / 增量

- **首次登录**：本地 KV 中 `sync_cursor:friend`、`sync_cursor:group` 等尚无记录，`EntitySyncEngine` 会以 `since_version = 0` 向服务端请求，相当于**全量拉取**好友、群组并写入本地 DB，同时写入 cursor。
- **再次登录**：cursor 已有值，引擎按 `since_version` 做**增量拉取**；若服务端未实现 version，则仍可能按分页全量拉取（取决于服务端实现）。

本示例在**每次认证后**执行一次好友与群组同步，无需在业务层区分「是否首次」；全量/增量由引擎根据 cursor 自动处理。

## 同步成功的标注方式

1. **接口层**：`sync_entities(entity_type, scope)` 返回 `Ok(count)` 表示本轮同步成功，`count` 为本轮拉取并落库的条数。
2. **存储层**：同步成功后，cursor 会更新（KV 中 `sync_cursor:friend`、`sync_cursor:group` 等），下次同步将基于该 cursor。
3. **校验**：示例中通过 `get_friends(100, 0)`、`get_groups(100, 0)` 读取本地数量，与本轮同步条数对比，用于验证「同步已落库」。

若服务端未实现 `entity/sync_entities`，`sync_entities` 会返回错误；示例中对失败做了容错并打日志，不影响后续步骤。

## 运行示例

确保服务器已启动（如 QUIC 127.0.0.1:8082），然后：

```bash
cd privchat-sdk/crates/privchat-sdk/examples/basic
cargo run
```

## 配置说明

- **数据目录**：`/tmp/privchat_basic_demo`
- **assets**：使用 SDK 根目录下的 `assets`（SQL 迁移），路径为 `CARGO_MANIFEST_DIR/../../../../assets`
- **服务器**：QUIC `127.0.0.1:8082`（可在代码中修改 `ServerEndpoint`）
- **JWT**：示例内生成 Mock Token，仅用于演示；生产环境必须从认证服务获取

## 扩展阅读

- 实体同步设计：`privchat-docs/design/ENTITY_SYNC_V1.md`
- 更多场景示例：`multi_account` 示例
