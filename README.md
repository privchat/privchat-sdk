# privchat-sdk

[简体中文](README.zh-Hans.md)

Core Rust SDK for the PrivChat messaging platform, providing unified low-level capabilities for iOS, Android, macOS, Linux, and Windows clients.

## Overview

privchat-sdk is the **single core engine** of the entire PrivChat client ecosystem. Platform SDKs such as Kotlin Multiplatform (privchat-sdk-kotlin), Swift (privchat-sdk-swift), and Android (privchat-sdk-android) all call into this project via UniFFI bindings and do not implement business logic directly.

## Responsibilities

- **Connection Management** - Establish, maintain, and close transport connections to PrivChat Server
- **Authentication** - Account registration, password login, and token-based authentication
- **Messaging** - Create, send, edit, revoke, and pin messages; manage the outbound queue
- **Channel Management** - CRUD operations on channels, unread counts, read markers, subscribe/unsubscribe
- **Entity Management** - CRUD for friends, groups, group members, and blocklist
- **Data Synchronization** - Bootstrap full sync, reconnect resume sync, incremental delta sync, per-entity-type and per-channel sync
- **Local Storage** - SQLite + SQLCipher encrypted database + Sled KV store with multi-account switching
- **Event-Driven** - Broadcast connection state changes, sync completion, timeline updates, read receipts, etc.
- **Extended Features** - Message reactions, @mentions, reminders, presence, typing indicators, generic RPC calls

## Architecture

```
+---------------------------------------------+
|        Platform SDK (Kotlin / Swift)         |
|       Calls via UniFFI-generated bindings    |
+---------------------------------------------+
|             privchat-sdk-ffi                 |
|      UniFFI Object + async export layer      |
|      PrivchatClient -> InnerSdk proxy        |
+---------------------------------------------+
|              privchat-sdk                    |
|                                              |
|  +----------+  +-------------+  +--------+  |
|  | Actor    |  | Local Store |  | Event  |  |
|  | CmdQueue |  | SQLite/Sled |  | Broadcast  |
|  +----+-----+  +------+------+  +---+----+  |
|       |               |             |        |
|  +----+---------------+-------------+----+   |
|  |          Receive Pipeline             |   |
|  |     Message receiving & processing    |   |
|  +---------------+-----------------------+   |
|                   |                          |
|  +----------------+---------------------+    |
|  |        msgtrans transport layer       |   |
|  |    QUIC / TCP / WebSocket + TLS       |   |
|  +---------------------------------------+   |
+---------------------------------------------+
|           privchat-protocol                  |
|      Protocol definitions (codec)            |
+---------------------------------------------+
```

### Core Design

- **Single-threaded Actor Model** - All commands (connect / login / authenticate, etc.) are serialized through an MPSC channel. No `RwLock` contention, no data races.
- **Pure async** - End-to-end async/await with no synchronous blocking compatibility layers.
- **Event Broadcast** - 256-capacity broadcast channel + 1024-event history buffer with sequence numbers and timestamps.
- **Multi-protocol Transport** - QUIC (low latency), TCP (high compatibility), WebSocket (mobile-friendly) with automatic fallback and TLS support.
- **Encrypted Local Storage** - SQLCipher-encrypted SQLite + Sled KV with multi-user UID switching and message cache budget policies.

## Project Structure

```
privchat-sdk/
+-- crates/
|   +-- privchat-sdk/             # Core SDK library
|   |   +-- src/
|   |   |   +-- lib.rs            # Main SDK logic (Actor model, state machine, API)
|   |   |   +-- local_store.rs    # SQLite local storage
|   |   |   +-- storage_actor.rs  # Async storage operations
|   |   |   +-- receive_pipeline.rs # Message receive pipeline
|   |   |   +-- task/             # Background task registry
|   |   |   +-- runtime/          # Tokio runtime abstraction
|   |   +-- examples/basic/       # SDK usage example
|   |
|   +-- privchat-sdk-ffi/         # FFI export layer
|       +-- src/lib.rs            # UniFFI exports (PrivchatClient)
|       +-- uniffi.toml           # UniFFI binding configuration
|       +-- bindings/             # Generated binding code (Kotlin / Swift)
|       +-- examples/             # FFI usage examples
|
+-- assets/                       # Runtime resources
+-- docs/                         # Architecture docs & API specs
+-- Cargo.toml                    # Workspace root configuration
```

## API Reference

### Connection & Authentication

| Method | Description |
|--------|-------------|
| `connect()` | Establish transport connection |
| `login(username, password, device_id)` | Login with username and password |
| `register(username, password, device_id)` | Register a new account |
| `authenticate(user_id, token, device_id)` | Token-based authentication |
| `disconnect()` / `shutdown()` | Disconnect / shut down the SDK |

### Messaging

| Method | Description |
|--------|-------------|
| `create_local_message()` | Create a local message (unsent) |
| `list_messages(channel_id, channel_type, limit, offset)` | Query message list |
| `enqueue_outbound_message()` | Enqueue message for sending |
| `edit_message()` | Edit a message |
| `set_message_revoke()` | Revoke a message |
| `set_message_pinned()` | Pin / unpin a message |

### Channels

| Method | Description |
|--------|-------------|
| `upsert_channel()` | Create or update a channel |
| `get_channel_by_id()` / `list_channels()` | Query channels |
| `mark_channel_read()` | Mark channel as read |
| `get_channel_unread_count()` | Get unread count |
| `subscribe_channel()` / `unsubscribe_channel()` | Subscribe / unsubscribe from push |

### Entity Management

| Method | Description |
|--------|-------------|
| `upsert_user()` / `get_user_by_id()` | User info |
| `upsert_friend()` / `delete_friend()` / `list_friends()` | Friends |
| `upsert_group()` / `get_group_by_id()` / `list_groups()` | Groups |
| `upsert_group_member()` / `list_group_members()` | Group members |
| `upsert_blacklist_entry()` / `list_blacklist_entries()` | Blocklist |

### Data Synchronization

| Method | Description |
|--------|-------------|
| `run_bootstrap_sync()` | Initial full sync |
| `resume sync` | Reconnect recovery by `since_version` + per-channel `pts` |
| `sync_entities(entity_type, scope)` | Sync by entity type |
| `sync_channel()` / `sync_all_channels()` | Channel sync |
| `get_difference()` | Incremental delta pull |

### Extended Features

| Method | Description |
|--------|-------------|
| `upsert_message_reaction()` | Message reactions |
| `record_mention()` / `get_unread_mention_count()` | @mentions |
| `subscribe_presence()` / `fetch_presence()` | Presence status |
| `send_typing()` | Typing indicators |
| `rpc_call(route, body_json)` | Generic RPC calls |

## Building

```bash
# Check compilation
cargo check

# Build FFI library (Release)
cargo build -p privchat-sdk-ffi --release

# Run core tests
cargo test -p privchat-sdk --lib
```

## Local Sync Regression

Use the local sync regression script to validate the current production sync contract end to end:

```bash
./scripts/run_local_sync_regression.sh
```

It runs:

- `privchat-server` full `cargo test`
- local DB integration test `entity_sync_version_db_test` when `DATABASE_URL` or
  `PRIVCHAT_TEST_DATABASE_URL` is set
- targeted `privchat-sdk` recovery and entity-version tests
- `accounts` example smoke flow

The current validated smoke result is:

- `accounts`: `32 / 32 passed`

## Generating Language Bindings

### Kotlin Bindings

```bash
cd crates/privchat-sdk-ffi
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language kotlin --out-dir bindings/kotlin --config uniffi.toml
```

### Swift Bindings

```bash
cd crates/privchat-sdk-ffi
cargo run --manifest-path uniffi-bindgen/Cargo.toml --release -- \
  generate ../../target/release/libprivchat_sdk_ffi.dylib \
  --language swift --out-dir bindings/swift --config uniffi.toml
```

## Cross-Compilation

```bash
# iOS Simulator (arm64)
cargo build -p privchat-sdk-ffi --release --target aarch64-apple-ios-sim

# iOS Device
cargo build -p privchat-sdk-ffi --release --target aarch64-apple-ios

# Android (requires cargo-ndk)
cargo ndk -t arm64-v8a build -p privchat-sdk-ffi --release
```

## State Machine

The SDK maintains a strict session state machine internally:

```
New -> Connected -> LoggedIn -> Authenticated -> Shutdown
                                     ^
                   register/login ---+
```

All API calls validate the current state and return explicit error codes when called in an invalid state.

## Error Handling

The SDK uses a unified `SdkError` enum for error types. The FFI layer wraps errors as `PrivchatFfiError { code, detail }`, with every error containing an error code and detailed description.

## Platform SDK Integration

This project is not intended for direct use by application developers. Instead, it is consumed through the following platform SDKs:

| Platform SDK | Description |
|--------------|-------------|
| [privchat-sdk-kotlin](https://github.com/privchat/privchat-sdk-kotlin) | Kotlin Multiplatform (Android + iOS + Desktop) |
| [privchat-sdk-swift](https://github.com/privchat/privchat-sdk-swift) | Native Swift (iOS / macOS) |
| [privchat-sdk-android](https://github.com/privchat/privchat-sdk-android) | Native Android |

## Documentation

- [Architecture Spec](docs/architecture-spec.md) - Actor model / local-first / FFI constraints
- [Public API v2](docs/public-api-v2.md) - SDK public API and call flows
- [Sync Resume Spec](../privchat-docs/spec/03-protocol-sdk/SDK_SYNC_RESUME_SPEC.md) - Reconnect recovery using entity versions and per-channel `pts`
