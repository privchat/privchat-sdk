# privchat-rust

全新最小化 Rust SDK 工程（不兼容旧 FFI 同步桥接）。

## 结构

- `crates/privchat-sdk`
  - 单线程 Actor 命令队列模型（`connect/login/authenticate` 串行执行）
  - 不对业务路径暴露 `RwLock` 竞争点
- `crates/privchat-sdk-ffi`
  - UniFFI Object + async 导出
  - 不含 `run_sync` / by-handle / snapshot 同步兼容层

## 构建

```bash
cd /Users/zoujiaqing/projects/privchat/privchat-rust
cargo check
cargo build -p privchat-sdk-ffi --release
```

## 生成 Kotlin 绑定

```bash
cd /Users/zoujiaqing/projects/privchat/privchat-rust/crates/privchat-sdk-ffi
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language kotlin --out-dir bindings/kotlin --config uniffi.toml
```

生成文件：

- `/Users/zoujiaqing/projects/privchat/privchat-rust/crates/privchat-sdk-ffi/bindings/kotlin/com/netonstream/privchat/sdk/privchat_sdk_ffi.kt`

## 迁移边界

当前这版是“新 SDK 主体”与“新 FFI async 导出”已独立可编译。
把它接入 `privchat-sdk-kotlin` sample 作为默认链路（并启用 iOS 自动复现门禁）是下一步。
