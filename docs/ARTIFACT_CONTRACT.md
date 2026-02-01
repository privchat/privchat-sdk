# Privchat SDK 产物契约（Artifact Contract）

> **单一真实规范**：各 SDK 的最终交付物、发布方式、接入方引用格式。  
> CI、发布、接入方必须严格遵循此契约。

---

## 架构原则

```
privchat-ffi (Rust)          ← 核心逻辑
        ↓
各语言薄封装 (Swift/Kotlin/KMP)
        ↓
各平台产物                   ← 平台可直接依赖的 binary 包
```

**各 SDK 职责**：

| 层 | 职责 |
|---|---|
| privchat-ffi | 真正逻辑 |
| swift/android/kotlin | 语言绑定 + API 包装 |
| **SDK 产物** | **平台可直接依赖的 binary 包** |

👉 **产物必须是平台原生标准格式，不要发源码给业务侧自己编。**

---

## 命名规范（io.privchat.*）

| 平台 | 命名规则 | 示例 |
|------|----------|------|
| Kotlin/KMP | `io.privchat.sdk` | `io.privchat.sdk` |
| Android | `io.privchat.sdk.android` | `io.privchat.sdk.android` |
| Swift | Module 名（PascalCase） | `PrivchatSDK` |
| Rust | crate 名 | `privchat-sdk` |
| FFI | crate 名 | `privchat-ffi` |
| Maven groupId | `io.privchat` | `io.privchat` |

**规则**：Java/Kotlin 用反向域名 `io.privchat.*`；Swift/Rust 用自然语言模块名。

---

## 产物矩阵

| 模块 | 技术栈 | 产物 | 发布方式 | 用途 |
|------|--------|------|----------|------|
| privchat-ffi | Rust | static/dynamic lib | crates/internal | 核心，供各封装层链接 |
| privchat-sdk-swift | Swift | **XCFramework** | SPM | iOS / macOS / watchOS |
| privchat-sdk-android | Kotlin | **AAR** | Maven | Android |
| privchat-sdk-kotlin | Kotlin Multiplatform | **klib** | Maven | KuiklyUI / Desktop / KMP iOS |

---

## 1. privchat-sdk-swift

### 产物

**XCFramework**：`PrivchatSDK.xcframework`

内部包含：

- `ios-arm64`
- `ios-simulator-arm64`
- `macos-arm64`
- `macos-x86_64`（可选）

### 发布方式（推荐）

**Swift Package Manager (SPM)**

```swift
// Package.swift
.target(
    name: "YourApp",
    dependencies: [
        .binaryTarget(
            name: "PrivchatSDK",
            url: "https://.../PrivchatSDK.xcframework.zip",
            checksum: "..."
        )
    ]
)
```

**优点**：现代 iOS 标准、Apple 官方推荐、接入一行代码。

### 构建

```bash
cd privchat-sdk-swift
bash scripts/build_xcframework.sh
```

---

## 2. privchat-sdk-android

### 产物

**AAR**：`privchat-sdk-android.aar`

内部结构：

```
classes.jar           # Kotlin API
jni/
   arm64-v8a/libprivchat_ffi.so
   armeabi-v7a/libprivchat_ffi.so
   x86_64/libprivchat_ffi.so
```

即：**Kotlin wrapper + Rust .so 一起打包**。

### ❌ 不要

- 只发 .so（业务侧还要写 JNI）
- AAR + 手动 JNI
- 发源码 module（每次编译 Rust，巨慢）

### ✅ 正确

- 发布 AAR，Telegram / Signal / Stripe / Realm 均为 AAR 标准。

### 发布方式

Maven Central / 私有 Maven

### 接入

```kotlin
implementation("io.privchat:sdk-android:1.0.0")
```

---

## 3. privchat-sdk-kotlin

### 产物

**KMP Library（.klib + metadata）**

发布物：

```
privchat-sdk-kotlin

Maven artifacts:
  - metadata
  - iosArm64.klib, iosSimulatorArm64.klib, iosX64.klib
  - macosArm64.klib, macosX64.klib
  - linuxX64.klib, mingwX64.klib
  - android-arm64-v8a, android-x86_64

**不含 jvmMain**：纯 Native Stack，macosMain / linuxMain / mingwMain 为 Kotlin/Native，不依赖 JVM。
```

### 本质

**Kotlin/Native bindings to privchat-ffi**，供：

- KuiklyUI
- Compose Multiplatform
- Kotlin Native Desktop
- KMP iOS

使用。

### ⚠️ 关键点

**KMP 不直接打包 Rust .so / .a，也不产出 XCFramework。**

而是：

```
cinterop → link privchat-ffi.a / .so
```

即：**每个平台构建时链接 FFI**。这是 KMP 的标准模式。

### 发布方式

Maven

### 接入

```kotlin
// build.gradle.kts
kotlin {
    sourceSets {
        commonMain.dependencies {
            implementation("io.privchat:sdk-kotlin:1.0.0")
        }
    }
}
```

---

## 4. KuiklyUI 架构建议

KuiklyUI = Kotlin Native，不依赖 JVM。

**KuiklyUI 只依赖**：

```
privchat-sdk-kotlin
```

**不要依赖** privchat-sdk-android / privchat-sdk-swift。

否则会导致：多余依赖、打包膨胀、平台耦合。

---

## 5. 一句话记忆

| 平台 | 产物 |
|------|------|
| Swift | XCFramework |
| Android | AAR |
| Kotlin | klib |

---

## 6. CI 与发布

三条独立 CI 流水线：

1. **privchat-sdk-swift**：构建 XCFramework → 发布 SPM / 托管 zip
2. **privchat-sdk-android**：构建 AAR → 发布 Maven
3. **privchat-sdk-kotlin**：构建 klib → 发布 Maven

互不依赖，可并行执行。
