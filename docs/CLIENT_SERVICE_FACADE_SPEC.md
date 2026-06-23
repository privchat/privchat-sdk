# Client Service Facade Spec (Phase C)

Status: **SPEC only — no code change, no daemon, no rpc implementation, no CLI behavior change.**

Input documents:
- `docs/SDK_FFI_CLIENT_SERVICE_REFACTOR_AUDIT.md` (Phase 1A audit; current ffi inventory, CLI usage subset, RpcBackend coverage policy).

This document is the contract that `privchat-sdk-ffi` (LocalBackend), `privchatd`, and `privchat-sdk-rpc` will all anchor to. `SDK_CONTROL_RPC_SPEC.md` (Phase D) is written **after** this one and derives wire methods, payload types, and error mapping from it.

---

## 1. Purpose

`privchat-sdk::client_service` is the **single high-level facade** for the PrivChat SDK. It encapsulates everything a frontend needs in order to:

- bring a client process up to a usable, authenticated state;
- read and mutate the local cached IM model (channels, messages, read cursors);
- send outbound messages and typing indicators;
- consume the SDK event stream.

It exists so that:

- `privchat-sdk-ffi` becomes a thin UniFFI binding layer over this facade.
- `privchatd` can implement an RPC server on top of this facade **without depending on `privchat-sdk-ffi`** (no UniFFI runtime, no language bridge).
- `privchat-sdk-rpc` derives its method set, payload types, error codes, and event semantics from this facade rather than from FFI internals or `privchat-protocol` wire types.

## 2. Non-goals

- **Not a 1:1 mirror of the 200-method FFI surface.** v1 covers only the CLI/TUI-used subset plus minimal session/read helpers.
- **Not a multi-account router.** One `ClientService` instance equals one active local account context. Multi-account selection is the caller's problem (see §9).
- **Not the IM wire protocol.** Facade types are derived from the operations callers want to perform, not from `privchat-protocol::rpc` request/response shapes. The IM wire stays an internal concern of the SDK.
- **Not a DTO mapping layer.** v1 does not introduce a separate control-DTO crate. Facade operation types live in `privchat-sdk::client_service::types` and are used directly as RPC payloads in Phase D.
- **Not a callback-interface system.** The facade exposes a polling/stream event model; cross-process callback bridging is out of scope.

## 3. Dependency position

```
privchat-sdk-ffi               privchatd                privchat-sdk-rpc (server side)
        \                          |                              /
         \                         |                             /
          v                        v                            v
          +--- privchat-sdk::client_service (this facade) ------+
                              |
                              v
                       privchat-sdk core
                  (InnerSdk, storage, transport,
                   protocol, sync, queues)
```

**Banned configurations:**
- `privchatd -> privchat-sdk-ffi -> privchat-sdk` (daemon must not drag in UniFFI runtime or language bridges).
- `privchat-sdk-rpc -> privchat-sdk-ffi` (RPC layer must not depend on FFI either).
- `privchat-sdk-ffi -> privchatd` (FFI must be runnable without a daemon present).

`privchat-sdk-ffi`, `privchatd`, and `privchat-sdk-rpc` are **siblings**, all depending on `privchat-sdk::client_service`.

## 4. Facade ownership rules

- The facade **owns the only stable contract** between SDK core and any outside consumer.
- Internal SDK helpers (`InnerSdk` methods, raw storage queries, raw RPC routes) **must not** be called directly from `privchat-sdk-ffi` or `privchatd` once the consumer's operation is covered by the facade.
- Facade methods that compose multiple SDK calls (`mark_read_by_local_message_id`, future `send_text_with_options`, etc.) are the canonical location for that composition. **The same composition must not live in two places.**
- Methods outside v1 that frontends use today via direct FFI passthrough may continue as passthrough until promoted to the facade. Promotion happens when a second consumer (e.g., `privchatd`) needs the same operation.

## 5. Type policy

| Type kind | Where it lives | Rule |
|---|---|---|
| Operation request / response types | `privchat-sdk::client_service::types` | New. Serde-serializable. Owned by the facade. Reused as RPC payloads in Phase D. |
| Stable SDK domain / storage entities | `privchat-sdk` (already exist) | Reusable directly: `StoredChannel`, `StoredMessage`, `StoredUser`, `SdkEvent`, `ConnectionState`, `SessionSnapshot`, `LoginResult`, `NewMessage`, `TypingActionType`. |
| FFI UniFFI records (`*View`, `*Input`, `*Result`) | `privchat-sdk-ffi` | **Binding-only.** Must not become facade contract types. FFI converts to/from facade types at its boundary. |
| IM wire types (`privchat-protocol::rpc::*`) | `privchat-protocol` | **Never** used as facade types. They are an SDK-internal implementation detail of how the facade talks to the server. |

If a SDK domain type and a facade operation type would have the same fields (e.g., `LoginResult`), prefer reusing the SDK type and document the reuse in §11.

## 6. Error policy

The facade returns `SdkError` (or `Result<T, SdkError>`-compatible). The facade does **not** define:

- `MethodNotImplemented`
- `UnsupportedInRpcBackend`
- `VersionMismatch`
- `ProtocolError`
- `TransportClosed`

Those are RPC-layer concerns and belong to `SDK_CONTROL_RPC_SPEC` (Phase D). FFI's `RpcBackend` will map them to a separate FFI variant (`PrivchatFfiError::Unsupported` or equivalent) after receiving them on the wire.

If the existing `SdkError` is missing variants the facade needs (e.g., `AuthRequired`, `NotFound`, `PermissionDenied`, `InvalidArgument`), it gets new variants there — not in a separate `ClientServiceError`. Goal: one error enum for the SDK side.

## 7. Event model

The facade exposes events as a **stream / polling primitive**, not as a callback interface.

```rust
pub fn subscribe_events(&self) -> EventStream;      // tokio::sync::broadcast::Receiver<SdkEvent> wrapper
pub fn last_event_sequence_id(&self) -> u64;
pub fn events_since(&self, cursor: u64, limit: u64) -> Vec<SequencedSdkEvent>;
```

**Cursor ownership rule (binds Phase D):**

- The facade **does not** own a global "current cursor" for callers. `last_event_sequence_id()` reflects the latest sequence the SDK has produced; it is not a per-caller read marker.
- **Cursor state belongs to the frontend process** (LocalBackend FFI, RpcBackend FFI, or `privchatd`'s per-client session). Each frontend tracks its own cursor and uses `events_since` / `subscribe_events` to consume.
- `privchatd` must **not** maintain a single global cursor that all RPC clients share. Each client's session keeps its own cursor; on reconnect the client tells the daemon what cursor to resume from.
- FFI's existing `next_event(timeout_ms)` / `next_event_envelope(timeout_ms)` API is preserved as binding-only convenience; internally they map to `subscribe_events` + the FFI-side cursor field.

**Event payload:** `SdkEvent` (existing SDK enum) is reused as-is. New variants are minor-compatible per §12.

## 8. Read / PTS model

PrivChat read marking is **pts-based**, not message-id-based. Definitions (frozen in this spec):

| Term | Meaning |
|---|---|
| `local_message_id` | Local DB primary key. Stable within a single client install. **Not** monotonic across clients. |
| `server_message_id` | Server-assigned message identifier. Stable across clients. **Not** the pts. |
| `pts` (also `message_pts`, `event_pts`) | Per-channel monotonic sequence used to advance the read cursor. The only value `mark_read_to_pts` accepts. |

**Facade exposes two read methods:**

```rust
pub async fn mark_read_to_pts(&self, req: MarkReadToPtsRequest) -> Result<u64, SdkError>;
pub async fn mark_read_by_local_message_id(&self, req: MarkReadByLocalMessageIdRequest) -> Result<u64, SdkError>;
```

- `mark_read_to_pts` is the **canonical primitive**. Wire-equivalent of the existing SDK `mark_read_to_pts`.
- `mark_read_by_local_message_id` is the **compatibility helper** for UI/TUI flows whose UX is "I scrolled past message X, mark it read." Internally it: (1) loads the `StoredMessage` by local id, (2) reads the message's pts field, (3) calls `mark_read_to_pts`.

**Hard constraints:**

- `server_message_id` MUST NOT be treated as `pts`.
- `local_message_id` MUST NOT be treated as `pts`.
- If the current SDK `StoredMessage` does not yet expose a pts field, **Phase 1B first task** is to expose it (either as a field on `StoredMessage` or via a SDK-internal `pts_by_local_id` helper). The CLI fix must not happen before that exposure exists.

## 9. Account model

- `ClientService::new(config)` constructs one instance bound to one local account context.
- v1 does not provide built-in multi-account routing inside the facade.
- Multi-account is delegated to upper layers: a `ClientServiceManager` (future, optional) or `privchatd`'s account registry.
- RPC envelope in Phase D **reserves** an `account` field but v1 daemon only accepts `"default"`; other values return `ACCOUNT_NOT_FOUND` / `UNSUPPORTED_ACCOUNT`.

`ClientServiceConfig` v1 fields:

```rust
pub struct ClientServiceConfig {
    pub endpoints: Vec<ServerEndpoint>,
    pub connection_timeout_secs: u32,
    pub data_dir: PathBuf,
    pub device_id: String,
    pub account_hint: Option<String>, // reserved; v1 ignores unless "default"
}
```

## 10. v1 API surface

Total: **18 public methods + 4 internal helpers**. Derived from the audit's 14-method CLI subset, plus session/read/lifecycle pairs required to make the facade usable on its own.

### Lifecycle (5)
- `new(config) -> Result<Self, SdkError>`
- `connect() -> Result<(), SdkError>`
- `disconnect() -> Result<(), SdkError>`
- `connection_state() -> Result<ConnectionState, SdkError>`
- `shutdown() -> Result<(), SdkError>`

### Auth & session (5)
- `login(req: LoginRequest) -> Result<LoginOutcome, SdkError>`
- `register(req: RegisterRequest) -> Result<LoginOutcome, SdkError>`
- `authenticate(req: AuthenticateRequest) -> Result<(), SdkError>`
- `logout() -> Result<(), SdkError>`
- `session_snapshot() -> Result<Option<SessionSnapshot>, SdkError>`

### Channels & messages (read) (3)
- `list_channels(req: ListChannelsRequest) -> Result<Vec<StoredChannel>, SdkError>`
- `list_messages(req: ListMessagesRequest) -> Result<Vec<StoredMessage>, SdkError>`
- `get_channel(channel_id: u64) -> Result<Option<StoredChannel>, SdkError>`

### Outbound (3)
- `send_text(req: SendTextRequest) -> Result<u64, SdkError>` — returns `local_message_id`.
- `mark_read_to_pts(req: MarkReadToPtsRequest) -> Result<u64, SdkError>`
- `mark_read_by_local_message_id(req: MarkReadByLocalMessageIdRequest) -> Result<u64, SdkError>`
- `send_typing(req: SendTypingRequest) -> Result<(), SdkError>`

### Events (3)
- `subscribe_events() -> EventStream`
- `last_event_sequence_id() -> u64`
- `events_since(cursor: u64, limit: u64) -> Vec<SequencedSdkEvent>`

### Internal helpers (not pub)
- `resolve_channel_type(channel_id: u64) -> Result<i32, SdkError>` (moved from ffi bucket G; used by the read/send helpers).
- `current_user_id() -> Result<u64, SdkError>` (used by `send_text` to fill `from_uid`).
- `pts_by_local_message_id(local_id: u64) -> Result<u64, SdkError>` (used by `mark_read_by_local_message_id`; may also be exposed pub later if Bot/MCP needs it).
- `resolve_pts_field(stored: &StoredMessage) -> Option<u64>` (centralised so the rule "server_message_id is not pts" is enforced in exactly one place).

## 11. Method details

Conventions for every entry: **Input** type (in `client_service::types` unless noted as reused), **Output** type, **Side effects**, **Errors that callers should expect**.

### 11.1 `new(config: ClientServiceConfig) -> Result<Self, SdkError>`

- Input: `ClientServiceConfig` (§9).
- Output: `ClientService` instance. **Does not connect.**
- Side effects: opens local data dir, initialises storage / queues / inner SDK.
- Errors: `InvalidArgument` (bad endpoints, bad data_dir), `Storage` (DB init failure).

### 11.2 `connect()` / `disconnect()` / `shutdown()`

- Input: none.
- Output: `()`.
- Side effects: transport state transitions. `shutdown` is terminal — after it returns, the only valid operation on the instance is `Drop`.
- Errors: `Network`, `Timeout`. `disconnect` and `shutdown` are idempotent.

### 11.3 `connection_state() -> ConnectionState`

- Output: existing SDK `ConnectionState` enum (`New | Connected | LoggedIn | Authenticated | Terminated | Shutdown`).
- Errors: none expected (returns the current cached state).

### 11.4 `login(req: LoginRequest)`

```rust
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    pub device_id: String,
}
pub struct LoginOutcome {
    pub user_id: u64,
    pub token: String,
}
```
- Side effects: opens session against server; does **not** call `authenticate` for the caller. Callers are expected to invoke `authenticate` next if they want to bind the session to this token (matches current FFI behaviour where `login` returns the credential and caller decides when to attach it).
- Errors: `AuthRequired` (bad creds), `Network`, `Timeout`, `InvalidArgument`.

### 11.5 `register(req: RegisterRequest)` — same shape as login.

### 11.6 `authenticate(req: AuthenticateRequest)`

```rust
pub struct AuthenticateRequest {
    pub user_id: u64,
    pub token: String,
    pub device_id: String,
}
```
- Side effects: binds the current connection to this identity; subsequent operations are performed as this user.
- Errors: `AuthRequired`, `Network`.

### 11.7 `logout()` / `session_snapshot()`

- `logout`: clears authenticated state on the wire and locally.
- `session_snapshot`: returns the existing SDK `SessionSnapshot` (reused as-is).

### 11.8 `list_channels(req)`

```rust
pub struct ListChannelsRequest {
    pub limit: u64,
    pub offset: u64,
}
```
- Output: `Vec<StoredChannel>` (reused SDK type).
- Side effects: local SQL read only. No network.

### 11.9 `list_messages(req)`

```rust
pub struct ListMessagesRequest {
    pub channel_id: u64,
    pub channel_type: Option<i32>,   // None = facade resolves via resolve_channel_type
    pub limit: u64,
    pub offset: u64,
    pub before_local_id: Option<u64>, // optional UI filter
}
```
- Output: `Vec<StoredMessage>` (reused).
- Side effects: local SQL read; may trigger `resolve_channel_type` (cached).

### 11.10 `get_channel(channel_id)`

- Output: `Option<StoredChannel>`.
- Errors: `Storage` only.

### 11.11 `send_text(req)`

```rust
pub struct SendTextRequest {
    pub channel_id: u64,
    pub channel_type: Option<i32>,   // None => facade resolves
    pub content: String,
}
```
- Output: `local_message_id: u64`.
- Side effects: writes a `NewMessage` to local store, enqueues it on the outbound queue, returns immediately (the queue handles wire send + ack). Matches existing FFI `send_message` semantics.
- Errors: `AuthRequired`, `InvalidArgument`, `Storage`.
- Note: facade fills `from_uid` via internal `current_user_id()`. Callers do **not** pass user_id (this is a deliberate narrowing from FFI's current signature).

### 11.12 `mark_read_to_pts(req)`

```rust
pub struct MarkReadToPtsRequest {
    pub channel_id: u64,
    pub read_pts: u64,
}
```
- Output: `last_read_pts: u64` (server-confirmed cursor).
- Side effects: wire RPC + local cursor advance.
- Errors: `Network`, `InvalidArgument` (pts <= current).

### 11.13 `mark_read_by_local_message_id(req)`

```rust
pub struct MarkReadByLocalMessageIdRequest {
    pub channel_id: u64,
    pub local_message_id: u64,
}
```
- Output: `last_read_pts: u64`.
- Side effects: internal `pts_by_local_message_id` lookup, then delegates to `mark_read_to_pts`.
- Errors: `NotFound` (message missing or has no pts), plus all errors of `mark_read_to_pts`.
- **Constraint:** if the resolved pts equals 0 or is `None`, return `NotFound`. **Never** silently fall back to `server_message_id` or `local_message_id`.

### 11.14 `send_typing(req)`

```rust
pub struct SendTypingRequest {
    pub channel_id: u64,
    pub channel_type: Option<i32>,
    pub active: bool,
    pub action: TypingActionType,
}
```
- Output: `()`. Best-effort; failures should not block UI flows but should still surface for logging.

### 11.15 `subscribe_events()`

- Output: an `EventStream` wrapping the underlying broadcast receiver. Each call returns an independent receiver; the SDK broadcasts events to all live subscribers.
- Backpressure: if the receiver lags, it sees `Lagged(n)` and must use `events_since` to recover (existing SDK semantics).

### 11.16 `last_event_sequence_id()` / `events_since(cursor, limit)`

- Pure read; reused from existing SDK.

## 12. Compatibility and versioning

**Rust API (the facade itself):**

- The crate `privchat-sdk` follows semver. The `client_service` module is **part of the public API**.
- Adding a new method on `ClientService`: **minor-compatible**.
- Adding a new variant to a `*Request` / `*Response` struct: **minor-compatible** only if the field has `Option<T>` or `#[serde(default)]`.
- Removing a method, renaming a field, changing a signature: **breaking**.
- Removing or renaming a `SdkEvent` variant: **breaking**.
- Adding a new `SdkEvent` variant: minor-compatible only if the enum is `#[non_exhaustive]`. **Action item for Phase 1B: mark `SdkEvent` `#[non_exhaustive]` if it isn't already.**

**Wire (RPC, Phase D):** version negotiation lives in `SDK_CONTROL_RPC_SPEC` (hello / protocol_version / features / methods). The facade does not itself version the wire.

## 13. FFI migration rules

- `privchat-sdk-ffi` rewrites its 14 CLI-used methods to delegate to `client_service`. The FFI signatures **do not change**; the `*View` / `*Input` / `*Result` wrappers stay; the rewrite is internal.
- The other 186 FFI methods remain as direct `self.inner.X(...)` passthroughs until promoted (see §4).
- FFI's existing event polling (`next_event`, `next_event_envelope`, etc.) keeps its current behaviour by subscribing to `client_service.subscribe_events()` and managing its own cursor (per §7).
- `PrivchatFfiError` keeps its current variants. The RPC-only variants (`Unsupported`, transport errors) are added when `RpcBackend` lands in Phase 1D.
- The FFI `set_message_read` historical call from CLI is fixed by routing CLI through `mark_read_by_local_message_id` once the facade ships. Until then the audit's open question (§12 of `SDK_FFI_CLIENT_SERVICE_REFACTOR_AUDIT.md`) remains the gating item for CLI build.

## 14. RpcBackend implications (binds Phase D)

This facade is the contract `SDK_CONTROL_RPC_SPEC` will derive from. Decisions encoded here that Phase D must honour:

- **Method namespace** in Phase D matches facade methods 1:1 (e.g., `sdk.session.login`, `sdk.channels.list`, `sdk.messages.send_text`, `sdk.read.mark_to_pts`, `sdk.read.mark_by_local_id`, `sdk.events.subscribe`). No extra wire-only methods at v1.
- **Payloads** are the `client_service::types` request/response types, serialised with serde.
- **Errors on the wire** carry `SdkError` for facade-level failures, plus the RPC-only error codes defined by Phase D (`METHOD_NOT_IMPLEMENTED`, `UNSUPPORTED_IN_RPC_BACKEND`, `VERSION_MISMATCH`, `PROTOCOL_ERROR`, `TRANSPORT_CLOSED`).
- **No silent fallback.** RpcBackend method gaps return `METHOD_NOT_IMPLEMENTED` with the method name embedded; LocalBackend remains full-coverage default.
- **Cursor stays per-client.** The daemon does not maintain a global event cursor (§7).
- **Account field reserved, default-only.** §9.

## 15. Phase 1B implementation cut

Scope of the next code change, derived from this SPEC. Each step is independently verifiable.

1. **Pts exposure** — if `StoredMessage` lacks a usable pts field, expose it on the struct or via a SDK-internal `pts_by_local_message_id` helper. No public API yet. (Blocks step 5.)
2. **Mark `SdkEvent` `#[non_exhaustive]`** if not already. One-liner. (Unblocks §12 minor-compatible event additions forever.)
3. **Create `privchat-sdk/src/client_service/{mod.rs, types.rs}`.** Empty scaffolding + the 8 request/response structs in §11.
4. **Implement the 18 public + 4 internal methods.** Each one is at most ~10 lines (passthrough to existing SDK with type conversion).
5. **Rewrite the 14 CLI-used FFI methods** in `privchat-sdk-ffi` to delegate to `client_service`. The other 186 FFI methods are untouched.
6. **Fix CLI** by routing `service.rs:300` through `mark_read_by_local_message_id` (resolves the audit open question). Cargo.toml path fix from Phase 1A.1 is preserved.
7. **Verify** by `cargo check` on the workspace + manual CLI flow (login → list channels → tail messages → send text → mark read).

**Out of scope for 1B:**
- `privchatd` binary.
- `privchat-sdk-rpc` crate.
- `RpcBackend` selector in `PrivchatClient::new`.
- Any of the 186 not-yet-promoted FFI methods.
- Multi-account routing.
- Control DTO crate.
