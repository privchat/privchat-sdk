# SDK FFI → client_service Refactor Audit (Phase 1A)

Scope: **audit-only**. No code change, no daemon, no rpc implementation, no CLI behavior change.

This audit feeds the upcoming Phase 1B: extracting a `privchat-sdk::client_service` facade so that `privchat-sdk-ffi` can be thinned down to a pure UniFFI binding layer, and so that a future `privchatd` can sit alongside the FFI layer (not below it).

---

## 1. Target dependency direction (frozen)

```
privchat-sdk-ffi        privchatd        privchat-sdk-ffi (RpcBackend)
        \                  /                       |
         \                /                        v
          \              /                 privchat-sdk-rpc (msgtrans-based)
           v            v                          |
            privchat-sdk::client_service           v
                       |                       privchatd
                       v                          |
                  privchat-sdk core              v
                                          privchat-sdk::client_service
```

- `privchat-sdk-ffi` and `privchatd` are **siblings**. Both depend on `privchat-sdk::client_service`.
- **Banned**: `privchatd → privchat-sdk-ffi → privchat-sdk`. Daemon must not drag in UniFFI runtime or language-bridge cost.
- `privchat-sdk-ffi` gains a backend selector (Local vs Rpc), but its public UniFFI surface stays unchanged for CLI/TUI/Kotlin/Swift callers.

## 2. DTO stance (v1)

- **No standalone control-DTO crate in v1.**
- RPC payloads reuse `privchat-sdk::client_service` input/output types directly (serde).
- Promote a separate DTO layer only when one of these actually happens: (a) FFI must keep an old shape while SDK evolves, (b) wire format diverges from in-process shape, (c) cross-version compatibility breaks.

## 3. msgtrans stance (v1)

`msgtrans` already provides `PacketType::{OneWay, Request, Response}` (`msgtrans/src/packet.rs:11`). That covers request/response correlation, frame, transport, session. `privchat-sdk-rpc` is built on top and only adds:

- `method` field (string route, e.g. `sdk.message.send`)
- `hello`/version/features handshake
- unified error model
- event subscribe semantics (uses `OneWay` for daemon → client events)

**No third-party network library. No premature `msgtrans-rpc` generic crate.** Extract a generic crate only after a second consumer appears.

---

## 4. FFI surface inventory

Source: `privchat-sdk/crates/privchat-sdk-ffi/src/lib.rs` (8,536 LoC, single file).

- **1 facade struct**: `PrivchatClient` (line 3115), 18 internal state fields.
- **~200 `pub async fn` methods** on `impl PrivchatClient` (lines 3137–8230).
- **~100 record types** (`*View` / `*Input` / `*Result` / `Stored*`) defined at lines 136–1781.
- **14 enums**: `PrivchatFfiError`, `TransportProtocol`, `ConnectionState`, `NetworkHint`, `ResumeFailureClass`, `ResumeEscalationScope`, `MediaDownloadState`, `ForcedLogoutSource`, `SdkEvent` (150-line variant enum, lines 1119–1269), `TypingActionType`, `MediaProcessOp`, `QrDecodeError`, `QrEncodeError`, `PacketType`-like.
- **1 trait**: `VideoProcessHook` (uniffi callback interface).
- **6 top-level `#[uniffi::export]` fns** (lines 8259, 8264, 8272, 8332, 8384): `sdk_version`, `git_sha`, `build_time`, `qr_decode_luma`, `qr_encode_matrix`.
- **1 reqwest call site** (line 8204, `download_attachment_to_path`). Only HTTP usage in the entire crate.

### 4.1 Method clustering by name prefix

| Cluster | Count (approx.) | Examples |
|---|---|---|
| Connection lifecycle | 12 | `connect`, `disconnect`, `is_connected`, `connection_state`, `last_terminal_reason`, `set_network_hint`, `shutdown`, `enter_background/foreground`, `register_lifecycle_hook`, `start_transport_disconnect_listener` |
| Event polling | 12 | `next_event`, `next_event_envelope`, `next_timeline_event_envelope`, `next_network_event_envelope`, `next_timeline_event`, `next_network_event`, `recent_events`, `events_since`, `recent_timeline_events`, `timeline_events_since`, `recent_network_events`, `network_events_since` |
| Event "subscription" stubs | 4 | `on_connection_state_changed`, `on_message_received`, `on_reaction_changed`, `on_typing_indicator` (no-op flag setters, see §6) |
| Auth | 6 | `login`, `register`, `authenticate`, `logout`, `refresh_access_token`, `auth_logout_remote` |
| Bootstrap / sync | 12 | `run_bootstrap_sync`, `is_bootstrap_completed`, `sync_entities*`, `sync_channel`, `sync_all_channels`, `sync_messages*`, `needs_sync`, `sync_*_remote`, `subscribe_channel`, `unsubscribe_channel` |
| Outbound send | 15 | `send_message`, `send_message_blocking`, `send_message_with_input`, `send_message_with_options`, `send_local_message_now`, `enqueue_text*`, `enqueue_outbound_message`, `enqueue_outbound_file`, `peek_outbound_*`, `ack_outbound_*`, `retry_message` |
| Remote RPC passthrough (`*_remote`) | 35+ | `account_user_detail_remote`, `group_qrcode_get_remote`, `channel_broadcast_create_remote`, `sticker_package_list_remote`, `sync_submit_remote`, ... |
| Local SQL CRUD (`upsert_*` / `list_*` / `get_*`) | 40+ | `upsert_channel`, `list_channels`, `get_channel_by_id`, `upsert_message_reaction`, `list_friends`, ... |
| Reactions | 9 | `add_reaction*`, `remove_reaction`, `list_reactions`, `reaction_stats`, `reactions`, `reactions_batch`, `upsert_message_reaction`, `list_message_reactions` |
| Channel state | 12 | `pin_channel`, `hide_channel`, `mute_channel`, `set_channel_hidden_local`, `delete_channel_local`, `set_channel_notification_mode`, `set_channel_favourite`, `set_channel_low_priority`, `channel_tags`, ... |
| Read / unread | 10 | `mark_read_to_pts*`, `message_read_list`, `message_read_stats`, `get_channel_unread_count`, `get_total_unread_count`, `channel_unread_stats`, `own_last_read`, `is_event_read_by`, `seen_by_for_event`, `mark_fully_read_at` |
| Group ops | 25+ | `create_group`, `get_group_info`, `fetch_group_members_remote`, `group_add_members_remote`, `group_mute_*`, `group_transfer_owner_remote`, `group_qrcode_*`, ... |
| Friend / blacklist | 14 | `send_friend_request`, `accept/reject/recall/get_*_friend_*`, `delete_friend`, `update_user_alias`, `add_to_blacklist`, `remove_from_blacklist`, `get_blacklist`, `check_blacklist`, `check_friend` |
| Profile / QR / privacy | 18 | `get_profile`, `update_profile`, `get/update_privacy_settings`, `qrcode_*`, `user_qrcode_*`, `search_user_by_qrcode`, `account_user_*_remote` |
| Media / attachments | 18 | `send_attachment_*`, `download_attachment_*`, `start/pause/resume/cancel_message_media_download`, `get_media_download_state`, `resolve_attachment_path`, `resolve_thumbnail_path`, `set/remove_video_process_hook`, `submit_media_job_result`, `finalize_local_attachment`, `create_local_attachment_placeholder` |
| Presence / typing | 8 | `get_presence`, `batch_get_presence`, `clear_presence_cache`, `get_presence_stats`, `start_typing*`, `stop_typing`, `send_typing`, `get_typing_stats` |
| Mentions / reminders | 10 | `record_mention`, `get_unread_mention_count`, `list_unread_mention_message_ids`, `mark_mention_read`, `mark_all_mentions_read`, `get_all_unread_mention_counts`, `upsert_reminder`, `list_pending_reminders`, `mark_reminder_done`, `update_thumb_status` |
| Local account / storage | 6 | `data_dir`, `assets_dir`, `storage`, `user_storage_paths`, `list_local_accounts`, `set_current_uid`, `wipe_current_user_full` |
| Config getters | 12 | `config`, `server_config`, `servers`, `connection_timeout`, `timezone_*`, `debug_mode`, `heartbeat_interval`, `image_send_max_edge`, `event_config`, `queue_config`, `retry_config`, `http_client_config` |
| Misc helpers | ~10 | `ping`, `builder`, `build`, `dm_peer_user_id`, `start/stop_supervised_sync`, `clear_local_state`, `transfer`, `rpc_call`, `add_server`, `generate_local_message_id` |

### 4.2 Top-level free functions (`#[uniffi::export]`)

| Fn | Purpose | Bucket |
|---|---|---|
| `sdk_version` | Version string | passthrough |
| `git_sha` | Build metadata | passthrough |
| `build_time` | Build metadata | passthrough |
| `qr_decode_luma` | QR decode (rxing) | self-contained, leave in ffi |
| `qr_encode_matrix` | QR encode (qrcode crate) | self-contained, leave in ffi |

---

## 5. Section classification (7 buckets)

| Bucket | Where in ffi | Stay or sink? |
|---|---|---|
| **A. UniFFI type definitions** | lines 136–1781 (records), 1119–1269 (`SdkEvent` enum), 8293/8350/8373 (QR types) | **Stay** — these are the binding contract. Many `Stored*` types are re-exports of SDK types; the FFI side only adds `#[uniffi::Record]`. |
| **B. Param conversion** | `map_config`, `map_login`, `map_session`, `map_sdk_event`, `map_sequenced_sdk_event`, `to_values`, scattered `From` impls | **Stay** — pure SDK ↔ FFI shape conversion. |
| **C. Error mapping** | `PrivchatFfiError` (line 1001) + `impl From<SdkError>` (line 1006) | **Stay** — single error enum, narrow. |
| **D. Callback adapter** | `event_rx` field, `event_envelope_cursor`, `event_poll_count`, 12 `next_*` polling methods, 4 no-op `on_*` flag setters | **Stay, but simplify** — FFI uses **cursor polling, not callbacks** (see §6). This makes daemon-mode the easy case: RPC stream just feeds the same polling API. |
| **E. Async bridge / runtime** | `pub async fn` + uniffi async support, `AsyncMutex`, `StdMutex`, `AtomicU64`/`AtomicBool` glue | **Stay**. |
| **F. SDK passthrough** | ~150 methods of shape `self.inner.X(...).await.map_err(PrivchatFfiError::from)` — login, register, authenticate, connect, disconnect, shutdown, list_channels, list_messages, get_channel_by_id, send_typing, all `*_remote`, all `upsert_*`/`list_*`/`get_*`, etc. | **Stay** at call site, but **route via `client_service` facade** (the facade for v1 only needs to cover the CLI-used subset; rest can stay direct). |
| **G. SDK orchestration / state wrapping** | `require_current_user_id` (line 3181), `resolve_channel_type` (3194), `resolve_local_message_id_by_server_message_id` (3201), `resolve_channel_id_by_server_message_id` (3214), `is_initialized`/`is_shutting_down` (3644/3649), `user_id` (3688), `get_connection_summary` (3693), `own_last_read` (7796), `persist_user_profile_local` (4203), `send_message_with_options` orchestration (6713), all `*_blocking` aliases, send_message wrapper family (lines 6666–6745, ~8 variants over `enqueue_local_message`) | **Sink to `privchat-sdk::client_service`** — these are real business helpers, not binding glue. |

**Bottom line:** the genuinely-orchestration code (bucket G) is **measured in hundreds of lines**, not thousands. The 8,536-line size is dominated by (A) record definitions + (F) one-line passthroughs.

---

## 6. Callback / listener shape

**This is the most important finding for the daemon design.**

| Aspect | Reality in current ffi |
|---|---|
| UniFFI callback interfaces | **Only one**: `VideoProcessHook` (line 241), used by `set_video_process_hook`/`remove_video_process_hook`/`submit_media_job_result`. |
| Event delivery to client | **Cursor polling** via `next_event(timeout_ms)`, `next_event_envelope`, `next_timeline_event_envelope`, `next_network_event_envelope`, etc. |
| Internal source | `inner.subscribe_events()` returns a `tokio::broadcast::Receiver<SdkEvent>` cached in `event_rx: AsyncMutex<...>`. |
| Cursor state | `event_envelope_cursor: AtomicU64`, initialised from `inner.last_event_sequence_id()`. |
| `on_connection_state_changed` / `on_message_received` / `on_reaction_changed` / `on_typing_indicator` | **No-op stubs.** Each just flips an `AtomicBool` registration flag and `eprintln!`s. Real events flow through polling, not callback. (lines 7772–7794) |
| Listener registry | None. There is no `Vec<Arc<dyn Listener>>`. |

**Implication for daemon-mode:**
- The "cross-process callback" problem that earlier discussion worried about **does not exist in practice**. The FFI surface is already a polling API.
- `privchat-sdk-rpc` only needs to expose `RuntimeEvent` over msgtrans `OneWay` packets. The FFI `RpcBackend` receives them, replays into the same broadcast channel + cursor that `next_*` already drains, and the upper layer notices nothing.
- `VideoProcessHook` (the one real callback interface) is media-pipeline only. CLI does **not** use it. It can be left unsupported in `RpcBackend` v1 with a clear error.

---

## 7. CLI/TUI actually-used FFI subset

Source: `privchat-cli/src/infrastructure/privchat/service.rs` (421 LoC). This is the **only** file in `privchat-cli` that touches `privchat-sdk-ffi`.

| # | FFI item | Used at | Category |
|---|---|---|---|
| 1 | `PrivchatClient::new(config)` | `get_or_init_sdk` | constructor |
| 2 | `connect()` | `validate_token`, `login`, `register`, `EventStreamPort::connect` | lifecycle |
| 3 | `disconnect()` | `EventStreamPort::disconnect` | lifecycle |
| 4 | `get_connection_state()` | `health_check`, `EventStreamPort::connect` | lifecycle |
| 5 | `authenticate(user_id, token, device_id)` | auth | auth |
| 6 | `login(username, password, device_id) -> LoginResult` | auth | auth |
| 7 | `register(username, password, device_id) -> LoginResult` | auth | auth |
| 8 | `list_channels(limit, offset) -> Vec<StoredChannel>` | `ChatDataPort::get_channels` | data |
| 9 | `list_messages(channel_id, channel_type, limit, offset) -> Vec<StoredMessage>` | `ChatDataPort::get_messages` | data |
| 10 | `get_channel_by_id(channel_id) -> Option<StoredChannel>` | `resolve_channel_type` helper | data |
| 11 | `send_message(channel_id, channel_type, user_id, content) -> u64` | `ChatDataPort::send_message` | outbound |
| 12 | `set_message_read(...)` | `ChatDataPort::mark_as_read` | read state (⚠️ name appears in CLI as called but no exact `set_message_read` symbol was located in the ffi inventory — verify during Phase 1B; may map to `mark_read_to_pts` or `update_message_status`) |
| 13 | `send_typing(channel_id, channel_type, bool, TypingActionType)` | `ChatDataPort::send_typing` | typing |
| 14 | `next_event(timeout_ms) -> Option<SdkEvent>` | `ensure_event_pump` background loop | event |

Types referenced: `PrivchatClient`, `PrivchatConfig`, `ServerEndpoint`, `TransportProtocol`, `ConnectionState`, `TypingActionType`, `SdkEvent`, `LoginResult`, `StoredChannel`, `StoredMessage`.

**CLI surface ≈ 14 methods / 10 types — ~7% of the 200-method FFI surface.**

This is the **load-bearing fact** of this audit: `client_service` v1 only needs to host these.

---

## 8. Candidate `privchat-sdk::client_service` v1 methods

Mirroring the CLI subset, but expressed in SDK-native types (no UniFFI annotations, no `*View` wrappers):

```rust
// privchat-sdk/src/client_service/mod.rs (proposed)
impl ClientService {
    // lifecycle
    pub fn new(config: ClientConfig) -> Result<Self, SdkError>;
    pub async fn connect(&self) -> Result<(), SdkError>;
    pub async fn disconnect(&self) -> Result<(), SdkError>;
    pub async fn connection_state(&self) -> Result<ConnectionState, SdkError>;
    pub async fn shutdown(&self) -> Result<(), SdkError>;

    // auth
    pub async fn login(&self, req: LoginRequest) -> Result<LoginOutcome, SdkError>;
    pub async fn register(&self, req: RegisterRequest) -> Result<LoginOutcome, SdkError>;
    pub async fn authenticate(&self, req: AuthenticateRequest) -> Result<(), SdkError>;
    pub async fn logout(&self) -> Result<(), SdkError>;

    // session
    pub async fn current_user_id(&self) -> Result<Option<u64>, SdkError>;
    pub async fn session_snapshot(&self) -> Result<Option<SessionSnapshot>, SdkError>;

    // data
    pub async fn list_channels(&self, req: ListChannelsRequest) -> Result<Vec<StoredChannel>, SdkError>;
    pub async fn list_messages(&self, req: ListMessagesRequest) -> Result<Vec<StoredMessage>, SdkError>;
    pub async fn get_channel(&self, channel_id: u64) -> Result<Option<StoredChannel>, SdkError>;
    pub async fn resolve_channel_type(&self, channel_id: u64) -> Result<i32, SdkError>; // moved from ffi G

    // outbound
    pub async fn send_text(&self, req: SendTextRequest) -> Result<u64, SdkError>;
    pub async fn mark_read(&self, req: MarkReadRequest) -> Result<(), SdkError>;
    pub async fn send_typing(&self, req: SendTypingRequest) -> Result<(), SdkError>;

    // events
    pub async fn next_event(&self, timeout: Duration) -> Result<Option<SdkEvent>, SdkError>;
    pub fn subscribe_events(&self) -> EventStream; // optional, for daemon use
}
```

**Notes:**
- `ClientConfig` / `LoginRequest` / `LoginOutcome` / `ListChannelsRequest` / `SendTextRequest` etc. live in `privchat-sdk::client_service::types` and are **serde-serializable** (this is what RPC payloads will reuse — no separate DTO crate per §2).
- FFI keeps its `PrivchatConfig` / `LoginResult` records; their `map_*` converters now target `ClientService` types instead of going directly to `InnerSdk`.
- `resolve_channel_type` is the only bucket-G item required for the CLI subset; the other G-helpers (`resolve_local_message_id_by_server_message_id`, `own_last_read`, etc.) can sink in later phases as their consumers migrate.

---

## 9. Stay-in-ffi list (Phase 1B does **not** touch these)

- All `#[uniffi::Record]` / `#[uniffi::Enum]` / `#[uniffi::Error]` type definitions.
- All `map_*` conversion functions.
- `PrivchatFfiError` and its `From<SdkError>`.
- All 12 cursor-polling event methods (they become RpcBackend-replayable as-is).
- `VideoProcessHook` callback interface + its `set/remove/submit` helpers.
- All 6 top-level free fns (`sdk_version`, `git_sha`, `build_time`, `qr_decode_luma`, `qr_encode_matrix`).
- 186+ FFI methods that CLI doesn't currently use — left as direct `self.inner.X(...)` passthroughs until a second consumer (Kotlin/Swift/Bot) forces them through `client_service`.

---

## 10. Phase 1B minimal refactor cut plan

**Scope target: ≤ 1 small PR, no behavior change for CLI, no public FFI API change.**

1. Create `privchat-sdk/src/client_service/{mod.rs, types.rs}`.
2. Implement only the 17 methods in §8 (the CLI-driven subset + 2 session helpers).
3. Rewrite the 14 corresponding `PrivchatClient` methods in `privchat-sdk-ffi` to delegate to `ClientService` instead of `self.inner` directly. The FFI methods keep their exact signatures and `*View`/`*Input`/`*Result` wrappers.
4. The remaining 186 FFI methods are **untouched** in Phase 1B.
5. Verify by running existing CLI flow (`privchat` TUI: login → list channels → tail messages → send text). No new tests written — relying on existing CLI integration as the contract test.
6. Resolve open item §7 row 12 (`set_message_read` symbol) during step 3 — may need to point CLI at the actual underlying method (`mark_read_to_pts` or similar).

**Out of scope for 1B (these come later, in order):**

- 1C: extract `privchat-sdk-rpc` crate (envelope + hello + error + event stream over msgtrans `Request`/`Response`/`OneWay`).
- 1D: add `RpcBackend` selector to `PrivchatClient::new(config)`. Default stays `local`.
- 1E: ship `privchatd` binary. Daemon depends on `privchat-sdk::client_service` directly, not on FFI.
- 1F: optional CLI command-mode subcommands reusing `ClientService` for one-shot ops; defaults to in-process.

---

## 11. RpcBackend coverage policy (binds 1C/1D)

- **LocalBackend**: full coverage of the existing FFI surface. Default and unchanged.
- **RpcBackend v1**: implements only the `client_service` facade subset required by CLI/TUI (the 17 methods in §8). Any FFI method outside this subset must return a clear `UnsupportedInRpcBackend` / `METHOD_NOT_IMPLEMENTED` error with the requested method name embedded in the message.
- **No implicit fallback** from RpcBackend to LocalBackend. Falling back silently would hide daemon coverage gaps and let tests pass against in-process SDK while the daemon path is still incomplete.
- Error model on the wire:

  ```json
  {
    "ok": false,
    "error": {
      "code": "METHOD_NOT_IMPLEMENTED",
      "message": "RPC method is not implemented in RpcBackend v1",
      "method": "sdk.media.download_attachment_to_path",
      "retryable": false
    }
  }
  ```

  FFI maps this to `PrivchatFfiError::Unsupported { message: "RpcBackend does not implement <method> in v1" }`.
- Coverage grows only when a real consumer (CLI command-mode, Bot runner, MCP server) drives a specific method through `client_service`. Do not pre-implement RPC routes that have no caller.

---

## 12. Open questions for Phase 1B kickoff

1. **`set_message_read` symbol.** CLI calls it at `service.rs:300` but it does not appear in the FFI surface inventory. Need to confirm whether CLI currently compiles against `mark_read_to_pts`, `update_message_status`, or a re-exported alias. (Could also be that the CLI currently doesn't compile against the latest FFI — verify build status before Phase 1B.)
2. **Should `ClientService` own a `Drop` that calls `shutdown_blocking`?** FFI today has explicit `shutdown` + `shutdown_blocking`. Behavior should be preserved.
3. **Event cursor semantics in `RpcBackend`.** Once §10 step 3 is done, decide whether the daemon advances the cursor or each client process maintains its own. Recommendation: per-client cursor (state lives in the FFI process, daemon is stateless w.r.t. cursors). Defer the decision until 1C.
4. **`VideoProcessHook` in RpcBackend.** The one real callback interface. v1 RpcBackend should return `Unsupported` rather than try to bridge the callback across process. CLI doesn't use it; mobile clients shouldn't be running against a daemon yet.
