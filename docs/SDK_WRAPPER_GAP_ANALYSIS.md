# SDK 封装层功能缺口分析

> 对照 [SDK_API_CONTRACT](SDK_API_CONTRACT.md) 及 FFI 能力，分析 privchat-sdk-kotlin、privchat-sdk-swift、privchat-sdk-android 三个封装层欠缺的功能。

---

## 一、privchat-sdk-android

### 1.1 已封装（v1.1+）

| 方法 | 说明 |
|------|------|
| `setVideoProcessHook(hook?)` | 视频缩略图/压缩回调（v1.1） |
| `removeVideoProcessHook()` | 移除视频处理回调 |
| `reactions(channelId, messageId)` | 获取消息反应列表 |
| `reactionsBatch(channelId, messageIds)` | 批量获取消息反应 |
| `isEventReadBy(channelId, messageId, userId)` | 某用户是否已读某消息 |
| `seenByForEvent(channelId, messageId, limit?)` | 已读某消息的用户列表 |
| `searchMessages(query, channelId?)` | 搜索消息 |

### 1.2 当前实现状态

- 核心 API 与高级 API（VideoProcessHook、reactions、searchMessages 等）已全部封装。

---

## 二、privchat-sdk-swift

### 2.1 已封装（v1.1+）

- `setVideoProcessHook`, `removeVideoProcessHook`
- `reactions`, `reactionsBatch`, `isEventReadBy`, `seenByForEvent`, `searchMessages`

### 2.2 当前实现状态

- 核心 API 与高级 API 已全部封装。

---

## 三、privchat-sdk-kotlin

### 3.1 严重缺口：Native（iOS / macOS / Linux）实现未完成

- **iosMain / macosMain / linuxMain** 为占位实现，所有方法返回 `NotImplementedError`。
- 完整实现需：**构建时 cinterop 链接 privchat-ffi.a/.so**（Rust static lib，各 target 对应）。**KMP 不产出 XCFramework**（详见 [ARTIFACT_CONTRACT](ARTIFACT_CONTRACT.md)）。
- 可参考 [uniffi-kotlin-multiplatform-bindings](https://github.com/UbiqueInnovation/uniffi-kotlin-multiplatform-bindings)（需 uniffi 版本兼容）。

### 3.2 Android 实际实现

- androidMain 已完整实现，与 privchat-sdk-android 对齐。

### 3.3 commonMain 接口

- expect 定义完整，与 Contract 对齐。
- `reactions`、`reactionsBatch`、`isEventReadBy`、`seenByForEvent`、`searchMessages`、`setVideoProcessHook`、`removeVideoProcessHook` 已暴露。

---

## 四、汇总

| 项目 | 主要缺口 | 工作量 |
|------|----------|--------|
| **privchat-sdk-android** | 无（已完善） | — |
| **privchat-sdk-swift** | 无（已完善） | — |
| **privchat-sdk-kotlin** | **iosMain / macosMain / linuxMain / mingwMain 为占位**（需 cinterop 链接 FFI） | 大 |

---

## 五、Sample 目录（统一命名）

各 SDK 均使用 `sample/` 目录，无 `sample-android` 等冗余命名：

| 项目 | sample 技术 | 平台 |
|------|-------------|------|
| privchat-sdk-android | Compose | Android |
| privchat-sdk-swift | SwiftUI | iOS, macOS |
| privchat-sdk-kotlin | Compose Multiplatform | Android, iOS |

**功能最小集**：connect • bootstrap sync • list sessions • send text • send image • upload progress • retry • reconnect

## 六、建议优先级

1. **P0**：privchat-sdk-kotlin Native 实现（ios/macos/linux/mingw cinterop 链接 privchat-ffi.a/.so/.dll）
