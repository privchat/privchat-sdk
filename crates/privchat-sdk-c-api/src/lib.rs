//! Stable C ABI for the PrivChat SDK.
//!
//! Design rules (see privchat-godot plan / privchat-sdk-c-api decision):
//! - ABI surface is opaque handles + scalars + UTF-8 JSON strings only;
//!   no structs or callbacks cross the boundary.
//! - All async SDK calls are bridged to blocking calls with a hard timeout
//!   on an internally hosted tokio runtime; callers (e.g. a GDExtension
//!   worker thread) invoke them like plain C functions.
//! - Strings returned by this crate are owned by Rust; callers must release
//!   them via [`privchat_capi_free_string`].
//! - Errors: int32-returning funcs use PRIVCHAT_CAPI_* codes; pointer-
//!   returning funcs return NULL on failure. Details go to a thread-local
//!   last-error message readable via [`privchat_capi_last_error`].

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::Arc;
use std::time::Duration;

use privchat_sdk::{NewMessage, PrivchatConfig, PrivchatSdk, TransferReply};
use tokio::runtime::Runtime;

/// Opaque client handle; fields are private to this crate.
pub struct PrivchatCapiClient {
    rt: Runtime,
    sdk: Arc<PrivchatSdk>,
}

pub const PRIVCHAT_CAPI_OK: i32 = 0;
pub const PRIVCHAT_CAPI_ERR_SDK: i32 = 1;
pub const PRIVCHAT_CAPI_ERR_INVALID_ARG: i32 = 2;
pub const PRIVCHAT_CAPI_ERR_TIMEOUT: i32 = 3;

/// Default hard timeout for bridged async calls when `timeout_ms` is 0.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

// ---------------------------------------------------------------------------
// Thread-local error plumbing
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = RefCell::new(None);
}

fn set_last_error(msg: String) {
    LAST_ERROR.with(|cell| {
        // Drop interior NULs defensively; the message is diagnostic only.
        *cell.borrow_mut() = CString::new(msg.replace('\0', " ")).ok();
    });
}

fn clear_last_error() {
    LAST_ERROR.with(|cell| *cell.borrow_mut() = None);
}

/// Last error message on this thread; NULL when the last call succeeded.
/// The pointer is invalidated by the NEXT c-api call on the same thread —
/// successful calls clear (free) the stored message too. Callers must copy
/// the string before making any further c-api call. Do not free it.
#[no_mangle]
pub extern "C" fn privchat_capi_last_error() -> *const c_char {
    LAST_ERROR.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null())
    })
}

/// Owned byte buffer handed to the caller. Release with
/// [`privchat_capi_free_buffer`]; `data` is NULL and `len` 0 when empty.
/// Binary-safe: unlike the string entry points, embedded NULs are preserved,
/// which is what FlatBuffers/Protobuf payloads require.
#[repr(C)]
pub struct PrivchatCapiBuffer {
    pub data: *mut u8,
    pub len: usize,
}

impl PrivchatCapiBuffer {
    fn empty() -> Self {
        Self { data: ptr::null_mut(), len: 0 }
    }

    fn from_vec(mut v: Vec<u8>) -> Self {
        if v.is_empty() {
            return Self::empty();
        }
        v.shrink_to_fit();
        let len = v.len();
        let data = v.as_mut_ptr();
        std::mem::forget(v);
        Self { data, len }
    }
}

/// Free a buffer produced by this crate. NULL or already-empty is a no-op.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_free_buffer(buffer: *mut PrivchatCapiBuffer) {
    if buffer.is_null() {
        return;
    }
    let b = &mut *buffer;
    if !b.data.is_null() && b.len > 0 {
        drop(Vec::from_raw_parts(b.data, b.len, b.len));
    }
    b.data = ptr::null_mut();
    b.len = 0;
}

/// Free a string previously returned by this crate. NULL is a no-op.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn read_c_str(arg: *const c_char) -> Option<String> {
    if arg.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(arg) }
        .to_str()
        .ok()
        .map(|s| s.to_string())
}

fn into_c_string(value: String) -> *mut c_char {
    match CString::new(value.replace('\0', " ")) {
        Ok(s) => s.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

fn json_to_c_string<T: serde::Serialize>(value: &T) -> *mut c_char {
    match serde_json::to_string(value) {
        Ok(s) => into_c_string(s),
        Err(e) => {
            set_last_error(format!("serialize failed: {e}"));
            ptr::null_mut()
        }
    }
}

fn resolve_timeout(timeout_ms: u64) -> Duration {
    Duration::from_millis(if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else {
        timeout_ms
    })
}

/// Run an SDK future on the client's runtime with a hard timeout.
/// Never panics across the FFI boundary.
fn block_on_timeout<F, T>(client: &PrivchatCapiClient, timeout_ms: u64, fut: F) -> Result<T, (i32, String)>
where
    F: Future<Output = Result<T, privchat_sdk::Error>> + Send + 'static,
    T: Send + 'static,
{
    let timeout = resolve_timeout(timeout_ms);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        client
            .rt
            .block_on(async move { tokio::time::timeout(timeout, fut).await })
    }));
    match outcome {
        Ok(Ok(Ok(v))) => {
            clear_last_error();
            Ok(v)
        }
        Ok(Ok(Err(_elapsed))) => Err((
            PRIVCHAT_CAPI_ERR_TIMEOUT,
            format!("timeout after {} ms", timeout.as_millis()),
        )),
        Ok(Err(e)) => Err((PRIVCHAT_CAPI_ERR_SDK, e.to_string())),
        Err(_) => Err((
            PRIVCHAT_CAPI_ERR_SDK,
            "panic inside SDK call".to_string(),
        )),
    }
}

macro_rules! guard_client {
    ($handle:expr, $err_ret:expr) => {
        match unsafe { $handle.as_ref() } {
            Some(c) => c,
            None => {
                set_last_error("null client handle".to_string());
                return $err_ret;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Create a client from a JSON-serialized `PrivchatConfig`, e.g.:
/// `{"endpoints":[{"protocol":"Quic","host":"127.0.0.1","port":8443,
/// "path":null,"use_tls":false}],"connection_timeout_secs":30,
/// "data_dir":"/tmp/privchat-godot"}`.
/// Returns NULL on failure; see `privchat_capi_last_error`.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_client_create(
    config_json: *const c_char,
) -> *mut PrivchatCapiClient {
    let out = catch_unwind(AssertUnwindSafe(|| {
        let json = match read_c_str(config_json) {
            Some(s) => s,
            None => {
                set_last_error("config_json is null or invalid utf-8".to_string());
                return ptr::null_mut();
            }
        };
        let config: PrivchatConfig = match serde_json::from_str(&json) {
            Ok(c) => c,
            Err(e) => {
                set_last_error(format!("invalid config json: {e}"));
                return ptr::null_mut();
            }
        };
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("privchat-capi")
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                set_last_error(format!("tokio runtime init failed: {e}"));
                return ptr::null_mut();
            }
        };
        // `PrivchatSdk::new` is synchronous and runtime-independent
        // (same contract as the UniFFI constructor path).
        let sdk = Arc::new(PrivchatSdk::new(config));
        Box::into_raw(Box::new(PrivchatCapiClient { rt, sdk }))
    }));
    match out {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic during client create".to_string());
            ptr::null_mut()
        }
    }
}

/// Best-effort graceful shutdown, then drop the client and its runtime.
/// NULL is a no-op. The handle is invalid afterwards.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_client_destroy(handle: *mut PrivchatCapiClient) {
    if handle.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let client = Box::from_raw(handle);
        let sdk = client.sdk.clone();
        let _ = client.rt.block_on(async move {
            let _ = tokio::time::timeout(Duration::from_secs(5), sdk.shutdown()).await;
        });
    }));
}

// ---------------------------------------------------------------------------
// Connect / auth
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_authenticate(
    handle: *const PrivchatCapiClient,
    user_id: u64,
    token: *const c_char,
    device_id: *const c_char,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let (token, device_id) = match (read_c_str(token), read_c_str(device_id)) {
        (Some(t), Some(d)) => (t, d),
        _ => {
            set_last_error("token/device_id is null or invalid utf-8".to_string());
            return PRIVCHAT_CAPI_ERR_INVALID_ARG;
        }
    };
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.authenticate(user_id, token, device_id).await
    }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_connect(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move { sdk.connect().await }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_disconnect(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move { sdk.disconnect().await }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// Bootstrap sync gate: local-first operations (send/create message) are
/// rejected until this completes after authenticate. Mirrors the TS SDK's
/// `bootstrapChannels` step in the login flow.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_run_bootstrap_sync(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.run_bootstrap_sync().await
    }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_shutdown(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    // `shutdown` never fails; the timeout only bounds how long we wait.
    match block_on_timeout(client, timeout_ms, async move {
        sdk.shutdown().await;
        Ok::<(), privchat_sdk::Error>(())
    }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// JSON-encoded `ConnectionState` (serde variant name), NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_connection_state(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.connection_state().await
    }) {
        Ok(state) => json_to_c_string(&state),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// JSON-encoded `SessionSnapshot` (or JSON null when no session), NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_session_snapshot(
    handle: *const PrivchatCapiClient,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.session_snapshot().await
    }) {
        Ok(snapshot) => json_to_c_string(&snapshot),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Channels / subscriptions
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_subscribe_channel(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: u8,
    token: *const c_char,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let token = read_c_str(token);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.subscribe_channel(channel_id, channel_type, token).await
    }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn privchat_capi_unsubscribe_channel(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: u8,
    timeout_ms: u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.unsubscribe_channel(channel_id, channel_type).await
    }) {
        Ok(()) => PRIVCHAT_CAPI_OK,
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// One-shot channel resync; `out_applied` receives the applied event count.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_sync_channel(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    timeout_ms: u64,
    out_applied: *mut u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.sync_channel(channel_id, channel_type).await
    }) {
        Ok(applied) => {
            if !out_applied.is_null() {
                *out_applied = applied as u64;
            }
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// Queue-first text send (same semantics as FFI `send_message_blocking`):
/// creates the local message and enqueues the outbound command atomically.
/// Returns the local message id via `out_message_id`.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_send_text_message(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    from_uid: u64,
    content: *const c_char,
    timeout_ms: u64,
    out_message_id: *mut u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let content = match read_c_str(content) {
        Some(c) => c,
        None => {
            set_last_error("content is null or invalid utf-8".to_string());
            return PRIVCHAT_CAPI_ERR_INVALID_ARG;
        }
    };
    let input = NewMessage {
        channel_id,
        channel_type,
        from_uid,
        message_type: 0,
        content,
        searchable_word: String::new(),
        setting: 0,
        extra: String::new(),
        mime_type: None,
        media_downloaded: false,
        thumb_status: 0,
    };
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.create_local_message_queued(input, None, "message", Vec::new(), None)
            .await
    }) {
        Ok(message_id) => {
            if !out_message_id.is_null() {
                *out_message_id = message_id;
            }
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// JSON array of `SequencedSdkEvent` (newest-first per SDK contract), or NULL.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_recent_events(
    handle: *const PrivchatCapiClient,
    limit: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let out = catch_unwind(AssertUnwindSafe(|| {
        let events = client.sdk.recent_events(limit as usize);
        json_to_c_string(&events)
    }));
    match out {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic in recent_events".to_string());
            ptr::null_mut()
        }
    }
}

/// JSON array of `SequencedSdkEvent` with sequence_id > from_sequence_id.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_timeline_events_since(
    handle: *const PrivchatCapiClient,
    from_sequence_id: u64,
    limit: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let out = catch_unwind(AssertUnwindSafe(|| {
        let events = client
            .sdk
            .timeline_events_since(from_sequence_id, limit as usize);
        json_to_c_string(&events)
    }));
    match out {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic in timeline_events_since".to_string());
            ptr::null_mut()
        }
    }
}

/// Unfiltered variant: ALL sequenced events with sequence_id > from_sequence_id.
/// Needed by poll-based hosts (e.g. the Godot GDExtension) to observe events
/// outside the timeline/network filter sets, such as
/// `SubscriptionMessageReceived` (Room broadcasts).
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_events_since(
    handle: *const PrivchatCapiClient,
    from_sequence_id: u64,
    limit: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let out = catch_unwind(AssertUnwindSafe(|| {
        let events = client
            .sdk
            .events_since(from_sequence_id, limit as usize);
        json_to_c_string(&events)
    }));
    match out {
        Ok(p) => p,
        Err(_) => {
            set_last_error("panic in events_since".to_string());
            ptr::null_mut()
        }
    }
}

/// JSON-encoded `StoredMessage`, JSON null when not found, NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_get_message_by_id(
    handle: *const PrivchatCapiClient,
    message_id: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.get_message_by_id(message_id).await
    }) {
        Ok(message) => json_to_c_string(&message),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Transfer / RPC
// ---------------------------------------------------------------------------

/// Render a [`TransferReply`] as the JSON body `privchat_capi_transfer` returns.
///
/// Split out of the FFI entry point so the "does the business code survive this
/// hop" question can be answered by a unit test instead of a live server: the
/// FFI signature needs a connected client, this does not. `code` is copied
/// verbatim — the bridge never interprets business codes.
fn transfer_reply_json(reply: &TransferReply) -> String {
    let data_value = match std::str::from_utf8(&reply.data) {
        Ok(s) => serde_json::Value::String(s.to_string()),
        Err(_) => serde_json::to_value(&reply.data).unwrap_or(serde_json::Value::Null),
    };
    serde_json::json!({
        "request_id": reply.request_id,
        "channel_id": reply.channel_id,
        "code": reply.code,
        "message": reply.message,
        "data": data_value,
    })
    .to_string()
}

/// Write a successful round trip's result into the caller's out-params.
///
/// Split out for the same reason as [`transfer_reply_json`]: this is the hop
/// where a business code could be silently reinterpreted, and it must be
/// testable without a connected client. Both pointers may be NULL.
///
/// # Safety
/// `out_code` and `out_reply`, when non-NULL, must be valid writable pointers.
/// Takes `reply` **by value**: the payload is moved into the caller's buffer,
/// not copied. This is the binary battle path (FlatBuffers frames), so an extra
/// full copy of the body per round trip is not acceptable just to make the
/// function testable.
unsafe fn write_transfer_bytes_out(
    reply: TransferReply,
    out_code: *mut i32,
    out_reply: *mut PrivchatCapiBuffer,
) {
    if !out_code.is_null() {
        *out_code = reply.code;
    }
    if reply.code != 0 {
        set_last_error(reply.message);
    }
    if !out_reply.is_null() {
        *out_reply = PrivchatCapiBuffer::from_vec(reply.data);
    }
}

/// Channel Transfer round-trip. `body` is passed through as raw bytes
/// (callers send a JSON string). Returns JSON:
/// `{"request_id","channel_id","code","message","data"}` where `data` is a
/// string when the payload is valid UTF-8, otherwise an array of bytes.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_transfer(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    route: *const c_char,
    body: *const c_char,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let (route, body) = match (read_c_str(route), read_c_str(body)) {
        (Some(r), Some(b)) => (r, b),
        _ => {
            set_last_error("route/body is null or invalid utf-8".to_string());
            return ptr::null_mut();
        }
    };
    let body_bytes = body.into_bytes();
    let sdk = client.sdk.clone();
    // Resolve the 0-means-default rule ONCE so the inner SDK timeout matches
    // the outer bridge timeout instead of receiving a literal 0.
    let effective_ms = resolve_timeout(timeout_ms).as_millis() as u64;
    match block_on_timeout(client, effective_ms, async move {
        sdk.transfer(channel_id, route, body_bytes, effective_ms).await
    }) {
        Ok(reply) => into_c_string(transfer_reply_json(&reply)),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Binary-safe Channel Transfer. `body`/`body_len` carry arbitrary bytes
/// (FlatBuffers, Protobuf, ...); the reply payload is returned verbatim in
/// `out_reply`. `out_code` receives the transfer envelope code (0 = success);
/// the envelope message goes to the thread-local last error when non-zero.
///
/// Returns PRIVCHAT_CAPI_OK when the round trip completed — check `*out_code`
/// for the business result. Caller must release `out_reply` via
/// [`privchat_capi_free_buffer`].
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_transfer_bytes(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    route: *const c_char,
    body: *const u8,
    body_len: usize,
    timeout_ms: u64,
    out_code: *mut i32,
    out_reply: *mut PrivchatCapiBuffer,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    if !out_reply.is_null() {
        *out_reply = PrivchatCapiBuffer::empty();
    }
    let route = match read_c_str(route) {
        Some(r) => r,
        None => {
            set_last_error("route is null or invalid utf-8".to_string());
            return PRIVCHAT_CAPI_ERR_INVALID_ARG;
        }
    };
    // A zero-length body is legal (some routes take no payload); only a NULL
    // pointer with a non-zero length is a caller bug.
    let body_bytes: Vec<u8> = if body_len == 0 {
        Vec::new()
    } else if body.is_null() {
        set_last_error("body is null but body_len > 0".to_string());
        return PRIVCHAT_CAPI_ERR_INVALID_ARG;
    } else {
        std::slice::from_raw_parts(body, body_len).to_vec()
    };

    let sdk = client.sdk.clone();
    let effective_ms = resolve_timeout(timeout_ms).as_millis() as u64;
    match block_on_timeout(client, timeout_ms, async move {
        sdk.transfer(channel_id, route, body_bytes, effective_ms).await
    }) {
        Ok(reply) => {
            write_transfer_bytes_out(reply, out_code, out_reply);
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// Global RPC call; returns the server JSON body as a string, NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_rpc_call(
    handle: *const PrivchatCapiClient,
    route: *const c_char,
    body_json: *const c_char,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let (route, body_json) = match (read_c_str(route), read_c_str(body_json)) {
        (Some(r), Some(b)) => (r, b),
        _ => {
            set_last_error("route/body_json is null or invalid utf-8".to_string());
            return ptr::null_mut();
        }
    };
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.rpc_call(route, body_json).await
    }) {
        Ok(resp) => into_c_string(resp),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

// ---------------------------------------------------------------------------
// Conversation history / channel list / read state
//
// Local-first mirrors of the UniFFI surface (MESSAGE_HISTORY spec
// SDK-HISTORY-5/7): local SQLite is the render source of truth; the SDK
// decides when to hydrate from the server and persists the gap watermark.
// ---------------------------------------------------------------------------

/// Open a conversation (mirrors FFI `open_conversation`, SDK-HISTORY-7):
/// local rows are the render truth; when local is empty the SDK hydrates one
/// LATEST window. Empty conversations return an empty list (no placeholder).
/// Returns JSON `{"messages":[StoredMessage,...],"has_more_before":bool,
/// "fetched_from_server":bool}`, NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_open_conversation(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    limit: u32,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.open_conversation(channel_id, channel_type, limit).await
    }) {
        Ok(page) => into_c_string(
            serde_json::json!({
                "messages": page.messages,
                "has_more_before": page.has_more_before,
                "fetched_from_server": page.fetched_from_server,
            })
            .to_string(),
        ),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Scroll-up paging (mirrors FFI `load_older_history`, SDK-HISTORY-5):
/// local-first, the server only fills gaps; the gap watermark is persisted by
/// the SDK, so `has_more_before=false` means "top reached" across sessions.
/// Returns JSON `{"messages":[StoredMessage,...],"has_more_before":bool}`.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_load_older_history(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    before_server_message_id: u64,
    limit: u32,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.load_older_history(channel_id, channel_type, before_server_message_id, limit)
            .await
    }) {
        Ok(page) => into_c_string(
            serde_json::json!({
                "messages": page.messages,
                "has_more_before": page.has_more_before,
            })
            .to_string(),
        ),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Pure local page read (mirrors FFI `list_messages`); no network.
/// Returns JSON `[StoredMessage,...]`, NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_list_messages(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    limit: u64,
    offset: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.list_messages(channel_id, channel_type, limit as usize, offset as usize)
            .await
    }) {
        Ok(messages) => json_to_c_string(&messages),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Local conversation list (mirrors FFI `list_channels`; note the FFI's
/// `get_channel_list_entries(page, page_size)` alias forwards its args as
/// (limit, offset) — this ABI uses the honest core names). Each entry is a
/// `StoredChannel` carrying `unread_count`/`top`/`mute`/`last_msg_timestamp`/
/// `last_msg_content`, i.e. everything a sorted badge list needs.
/// Returns JSON `[StoredChannel,...]`, NULL on failure.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_list_channels(
    handle: *const PrivchatCapiClient,
    limit: u64,
    offset: u64,
    timeout_ms: u64,
) -> *mut c_char {
    let client = guard_client!(handle, ptr::null_mut());
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.list_channels(limit as usize, offset as usize).await
    }) {
        Ok(channels) => json_to_c_string(&channels),
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
        }
    }
}

/// Advance the read cursor (mirrors FFI `mark_read_to_pts`): RPC
/// `message/status/read_pts` then project the server-confirmed cursor into
/// the local store (projection failure is non-fatal, same as the FFI).
/// `out_last_read_pts` receives the server-accepted pts.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_mark_read_to_pts(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    read_pts: u64,
    timeout_ms: u64,
    out_last_read_pts: *mut u64,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        let body = serde_json::json!({
            "channel_id": channel_id,
            "read_pts": read_pts,
        })
        .to_string();
        let resp = sdk
            .rpc_call("message/status/read_pts".to_string(), body)
            .await?;
        let v: serde_json::Value = serde_json::from_str(&resp)
            .map_err(|e| privchat_sdk::Error::Serialization(format!("read_pts resp: {e}")))?;
        let last_read_pts = v["last_read_pts"].as_u64().ok_or_else(|| {
            privchat_sdk::Error::Serialization(format!("read_pts resp missing last_read_pts: {resp}"))
        })?;
        // Same fallback as the FFI's resolve_channel_type: unknown -> direct.
        let channel_type = match sdk.get_channel_by_id(channel_id).await {
            Ok(Some(ch)) => ch.channel_type,
            _ => 1,
        };
        let _ = sdk
            .project_channel_read_cursor(channel_id, channel_type, last_read_pts)
            .await;
        Ok(last_read_pts)
    }) {
        Ok(last_read_pts) => {
            if !out_last_read_pts.is_null() {
                *out_last_read_pts = last_read_pts;
            }
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// Per-channel unread count (mirrors FFI `get_channel_unread_count`; local).
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_get_channel_unread_count(
    handle: *const PrivchatCapiClient,
    channel_id: u64,
    channel_type: i32,
    timeout_ms: u64,
    out_count: *mut i32,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.get_channel_unread_count(channel_id, channel_type).await
    }) {
        Ok(count) => {
            if !out_count.is_null() {
                *out_count = count;
            }
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

/// Global unread badge (mirrors FFI `get_total_unread_count`; local).
/// `exclude_muted != 0` skips muted channels.
#[no_mangle]
pub unsafe extern "C" fn privchat_capi_get_total_unread_count(
    handle: *const PrivchatCapiClient,
    exclude_muted: i32,
    timeout_ms: u64,
    out_count: *mut i32,
) -> i32 {
    let client = guard_client!(handle, PRIVCHAT_CAPI_ERR_INVALID_ARG);
    let sdk = client.sdk.clone();
    match block_on_timeout(client, timeout_ms, async move {
        sdk.get_total_unread_count(exclude_muted != 0).await
    }) {
        Ok(count) => {
            if !out_count.is_null() {
                *out_count = count;
            }
            PRIVCHAT_CAPI_OK
        }
        Err((code, msg)) => {
            set_last_error(msg);
            code
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: ABI guards, lifecycle and error plumbing (no server required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU64, Ordering};

    static DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn cstr(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn reply_with_code(code: i32) -> TransferReply {
        TransferReply {
            request_id: "rid-1".to_string(),
            channel_id: 42,
            code,
            message: format!("biz {code}"),
            data: b"{\"ok\":1}".to_vec(),
        }
    }

    /// 业务码经 JSON 出口这一跳必须原样到达宿主。
    ///
    /// 桥接层不认识 `21655`、`30004` 的语义 —— 认识就意味着每加一个业务
    /// 模块都要改 C ABI。这里只证明它**没有**顺手解释或截断。
    #[test]
    fn transfer_json_preserves_business_code() {
        for code in [0i32, 21401, 21502, 21600, 21655, 30004, 65535] {
            let json = transfer_reply_json(&reply_with_code(code));
            let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
            assert_eq!(v["code"].as_i64(), Some(code as i64), "code 被改写: {json}");
            assert_eq!(v["message"].as_str(), Some(format!("biz {code}").as_str()));
            assert_eq!(v["channel_id"].as_u64(), Some(42));
        }
    }

    /// 负数码同样要原样透传:`code` 在线上是有符号 i32,
    /// 中途若被当成无符号搬运,`-1` 会变成 4294967295。
    #[test]
    fn transfer_json_preserves_negative_code() {
        let v: serde_json::Value =
            serde_json::from_str(&transfer_reply_json(&reply_with_code(-1))).unwrap();
        assert_eq!(v["code"].as_i64(), Some(-1));
    }

    /// 二进制出口的同一问题:`out_code` 指针写入的值必须等于回包里的码,
    /// 且非零时错误文本可从 `last_error` 读回。
    #[test]
    fn transfer_bytes_out_code_mirrors_reply() {
        for code in [0i32, 21600, 21655, 30004] {
            let mut out_code: i32 = i32::MIN;
            let mut out_reply = PrivchatCapiBuffer::empty();
            unsafe {
                write_transfer_bytes_out(reply_with_code(code), &mut out_code, &mut out_reply);
            }
            assert_eq!(out_code, code, "out_code 与回包不一致");
            if code != 0 {
                assert!(
                    last_error_string().contains(&format!("biz {code}")),
                    "非零码的 message 未进入 last_error");
            }
            unsafe { privchat_capi_free_buffer(&mut out_reply) };
        }
    }

    fn last_error_string() -> String {
        let p = privchat_capi_last_error();
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }

    /// Unique throwaway data_dir so parallel tests never share sqlite files.
    fn test_config_json() -> CString {
        let n = DIR_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "privchat-capi-test-{}-{n}",
            std::process::id()
        ));
        cstr(&format!(
            "{{\"endpoints\":[{{\"protocol\":\"Tcp\",\"host\":\"127.0.0.1\",\
             \"port\":1,\"path\":null,\"use_tls\":false}}],\
             \"connection_timeout_secs\":2,\"data_dir\":\"{}\"}}",
            dir.display()
        ))
    }

    #[test]
    fn free_null_string_is_noop() {
        unsafe { privchat_capi_free_string(ptr::null_mut()) };
    }

    #[test]
    fn destroy_null_handle_is_noop() {
        unsafe { privchat_capi_client_destroy(ptr::null_mut()) };
    }

    #[test]
    fn create_rejects_null_and_invalid_config() {
        unsafe {
            assert!(privchat_capi_client_create(ptr::null()).is_null());
            assert!(last_error_string().contains("null"));

            let bad = cstr("{not json");
            assert!(privchat_capi_client_create(bad.as_ptr()).is_null());
            assert!(last_error_string().contains("invalid config json"));
        }
    }

    #[test]
    fn null_handle_returns_invalid_arg_or_null() {
        unsafe {
            let null: *const PrivchatCapiClient = ptr::null();
            let tok = cstr("t");
            let dev = cstr("d");
            let content = cstr("hi");
            let route = cstr("r");

            assert_eq!(
                privchat_capi_authenticate(null, 1, tok.as_ptr(), dev.as_ptr(), 10),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(privchat_capi_connect(null, 10), PRIVCHAT_CAPI_ERR_INVALID_ARG);
            assert_eq!(privchat_capi_disconnect(null, 10), PRIVCHAT_CAPI_ERR_INVALID_ARG);
            assert_eq!(
                privchat_capi_run_bootstrap_sync(null, 10),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(privchat_capi_shutdown(null, 10), PRIVCHAT_CAPI_ERR_INVALID_ARG);
            assert_eq!(
                privchat_capi_subscribe_channel(null, 1, 0, ptr::null(), 10),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_unsubscribe_channel(null, 1, 0, 10),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_sync_channel(null, 1, 0, 10, ptr::null_mut()),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_send_text_message(
                    null, 1, 0, 1, content.as_ptr(), 10, ptr::null_mut()
                ),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert!(last_error_string().contains("null client handle"));

            assert_eq!(
                privchat_capi_mark_read_to_pts(null, 1, 1, 10, ptr::null_mut()),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_get_channel_unread_count(null, 1, 1, 10, ptr::null_mut()),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_get_total_unread_count(null, 0, 10, ptr::null_mut()),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );

            // Pointer-returning entry points must yield NULL, not crash.
            assert!(privchat_capi_connection_state(null, 10).is_null());
            assert!(privchat_capi_session_snapshot(null, 10).is_null());
            assert!(privchat_capi_recent_events(null, 10).is_null());
            assert!(privchat_capi_timeline_events_since(null, 0, 10).is_null());
            assert!(privchat_capi_events_since(null, 0, 10).is_null());
            assert!(privchat_capi_get_message_by_id(null, 1, 10).is_null());
            assert!(privchat_capi_transfer(null, 1, route.as_ptr(), route.as_ptr(), 10).is_null());
            assert!(privchat_capi_rpc_call(null, route.as_ptr(), route.as_ptr(), 10).is_null());
            assert!(privchat_capi_open_conversation(null, 1, 1, 10, 10).is_null());
            assert!(privchat_capi_load_older_history(null, 1, 1, 0, 10, 10).is_null());
            assert!(privchat_capi_list_messages(null, 1, 1, 10, 0, 10).is_null());
            assert!(privchat_capi_list_channels(null, 10, 0, 10).is_null());
        }
    }

    #[test]
    fn string_args_reject_null() {
        let cfg = test_config_json();
        unsafe {
            let h = privchat_capi_client_create(cfg.as_ptr());
            assert!(!h.is_null(), "create failed: {}", last_error_string());

            assert_eq!(
                privchat_capi_authenticate(h, 1, ptr::null(), ptr::null(), 10),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert_eq!(
                privchat_capi_send_text_message(h, 1, 0, 1, ptr::null(), 10, ptr::null_mut()),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            assert!(privchat_capi_rpc_call(h, ptr::null(), ptr::null(), 10).is_null());
            assert!(privchat_capi_transfer(h, 1, ptr::null(), ptr::null(), 10).is_null());

            privchat_capi_client_destroy(h);
        }
    }

    #[test]
    fn offline_lifecycle_and_event_paths() {
        let cfg = test_config_json();
        unsafe {
            let h = privchat_capi_client_create(cfg.as_ptr());
            assert!(!h.is_null(), "create failed: {}", last_error_string());

            // Fresh client: no events yet, JSON arrays must be valid/empty.
            for p in [
                privchat_capi_recent_events(h, 10),
                privchat_capi_timeline_events_since(h, 0, 10),
                privchat_capi_events_since(h, 0, 10),
            ] {
                assert!(!p.is_null());
                let s = CStr::from_ptr(p).to_str().unwrap().to_string();
                assert_eq!(s, "[]", "expected empty event array, got {s}");
                privchat_capi_free_string(p);
            }

            // connect must fail fast (nothing listens on port 1) but must NOT
            // poison the client: sync paths keep working afterwards.
            let rc = privchat_capi_connect(h, 5000);
            assert_ne!(rc, PRIVCHAT_CAPI_OK, "connect to dead endpoint must fail");
            assert!(!last_error_string().is_empty());

            let p = privchat_capi_recent_events(h, 10);
            assert!(!p.is_null(), "client unusable after failed connect");
            privchat_capi_free_string(p);

            // NOTE: the bootstrap gate (send before run_bootstrap_sync must be
            // rejected) is exercised end-to-end in privchat-godot-demo's
            // auto_comm_check.gd; offline the actor never reaches a session,
            // so the command can only time out here — not a meaningful signal.

            assert_eq!(privchat_capi_shutdown(h, 5000), PRIVCHAT_CAPI_OK);
            privchat_capi_client_destroy(h);
        }
    }

    /// Every exported function must be declared in the hand-maintained
    /// header; a missing declaration breaks C/C++ consumers silently.
    /// 二进制安全:含内嵌 NUL 与非 UTF-8 字节的 body 必须原样传递,
    /// 不得像字符串接口那样被截断/拒绝。
    #[test]
    fn transfer_bytes_rejects_bad_args_and_keeps_binary() {
        let cfg = test_config_json();
        unsafe {
            let h = privchat_capi_client_create(cfg.as_ptr());
            assert!(!h.is_null(), "create failed: {}", last_error_string());
            let route = cstr("game/battle/act");
            let mut code: i32 = -1;
            let mut out = PrivchatCapiBuffer::empty();

            // NULL body + 非零长度 = 调用方 bug
            assert_eq!(
                privchat_capi_transfer_bytes(h, 1, route.as_ptr(), ptr::null(), 4, 10,
                        &mut code, &mut out),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            // NULL route
            assert_eq!(
                privchat_capi_transfer_bytes(h, 1, ptr::null(), ptr::null(), 0, 10,
                        &mut code, &mut out),
                PRIVCHAT_CAPI_ERR_INVALID_ARG
            );
            // 含 NUL 与 0xFF 的二进制 body:离线必然超时/失败,但不得因为
            // 内容不是 UTF-8 就在参数校验阶段被拒(那才是字符串接口的毛病)。
            let binary: [u8; 6] = [0x00, 0xFF, 0x41, 0x00, 0xFE, 0x42];
            let rc = privchat_capi_transfer_bytes(h, 1, route.as_ptr(),
                    binary.as_ptr(), binary.len(), 200, &mut code, &mut out);
            assert_ne!(rc, PRIVCHAT_CAPI_ERR_INVALID_ARG,
                    "binary body must not be rejected as an invalid argument");

            privchat_capi_free_buffer(&mut out);
            privchat_capi_free_buffer(&mut out);   // 二次释放必须是 no-op
            privchat_capi_client_destroy(h);
        }
    }

    #[test]
    fn free_buffer_null_is_noop() {
        unsafe { privchat_capi_free_buffer(ptr::null_mut()) };
    }

    #[test]
    fn header_declares_every_export() {
        let header = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/include/privchat_sdk_c_api.h"
        ))
        .expect("header missing");
        const EXPORTS: &[&str] = &[
            "privchat_capi_client_create",
            "privchat_capi_client_destroy",
            "privchat_capi_last_error",
            "privchat_capi_free_string",
            "privchat_capi_authenticate",
            "privchat_capi_connect",
            "privchat_capi_disconnect",
            "privchat_capi_run_bootstrap_sync",
            "privchat_capi_shutdown",
            "privchat_capi_connection_state",
            "privchat_capi_session_snapshot",
            "privchat_capi_subscribe_channel",
            "privchat_capi_unsubscribe_channel",
            "privchat_capi_sync_channel",
            "privchat_capi_send_text_message",
            "privchat_capi_recent_events",
            "privchat_capi_timeline_events_since",
            "privchat_capi_events_since",
            "privchat_capi_get_message_by_id",
            "privchat_capi_transfer",
            "privchat_capi_transfer_bytes",
            "privchat_capi_free_buffer",
            "privchat_capi_rpc_call",
            "privchat_capi_open_conversation",
            "privchat_capi_load_older_history",
            "privchat_capi_list_messages",
            "privchat_capi_list_channels",
            "privchat_capi_mark_read_to_pts",
            "privchat_capi_get_channel_unread_count",
            "privchat_capi_get_total_unread_count",
        ];
        for name in EXPORTS {
            assert!(
                header.contains(name),
                "header is missing declaration for {name}"
            );
        }
    }

    /// First usable C compiler with a clean host-compile environment.
    /// Some environments put an iOS-targeted cc first in PATH (which rejects
    /// a macOS sysroot) or export an iOS-pointing SDKROOT; strip both.
    fn host_cc() -> Option<std::process::Command> {
        let sdkroot = std::process::Command::new("xcrun")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        for cc in ["cc", "clang", "gcc"] {
            let mut cmd = std::process::Command::new(cc);
            cmd.arg("--version");
            if cmd.output().map(|o| o.status.success()).unwrap_or(false) {
                let mut cmd = std::process::Command::new(cc);
                if let Some(root) = &sdkroot {
                    cmd.args(["-isysroot", root]);
                }
                cmd.env_remove("SDKROOT");
                cmd.env_remove("IPHONEOS_DEPLOYMENT_TARGET");
                cmd.env_remove("MACOSX_DEPLOYMENT_TARGET");
                return Some(cmd);
            }
        }
        None
    }

    fn require_macos() -> bool {
        if cfg!(not(target_os = "macos")) {
            eprintln!("skip: C toolchain checks are only wired for macOS hosts");
            return false;
        }
        true
    }

    /// Compile the C smoke test against the header to prove the header is
    /// self-contained and matches C expectations.
    #[test]
    fn c_smoke_compiles_against_header() {
        if !require_macos() {
            return;
        }
        let manifest = env!("CARGO_MANIFEST_DIR");
        let src = format!("{manifest}/tests/c_smoke.c");
        let include = format!("{manifest}/include");
        let mut cmd = host_cc().expect(
            "no C compiler found (cc/clang/gcc); install Xcode command line tools",
        );
        cmd.args(["-fsyntax-only", "-Wall", "-Werror", "-I", &include, &src]);
        let o = cmd.output().expect("failed to invoke cc");
        assert!(
            o.status.success(),
            "cc reported header/usage errors:\n{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Directory holding the debug cdylib (cargo test does not build the
    /// cdylib target itself, so build it on demand).
    fn cdylib_dir() -> std::path::PathBuf {
        if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
            return std::path::PathBuf::from(dir).join("debug");
        }
        // Workspace layout: crates/privchat-sdk-c-api -> privchat-sdk/target.
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/debug")
    }

    /// Real ABI proof: link the C smoke test against the built cdylib and
    /// run it. This exercises actual symbol names and signatures end to end
    /// (create -> invalid config -> event poll -> connect failure -> destroy),
    /// which `-fsyntax-only` against the header cannot.
    #[test]
    fn c_smoke_links_and_runs() {
        if !require_macos() {
            return;
        }
        let manifest = env!("CARGO_MANIFEST_DIR");
        let lib_dir = cdylib_dir();
        let dylib = lib_dir.join("libprivchat_sdk_c_api.dylib");
        if !dylib.exists() {
            let status = std::process::Command::new(env!("CARGO"))
                .args(["build", "-p", "privchat-sdk-c-api"])
                .current_dir(manifest)
                .status()
                .expect("failed to invoke cargo");
            assert!(status.success(), "cargo build of the cdylib failed");
        }
        assert!(dylib.exists(), "cdylib missing at {}", dylib.display());

        let bin = std::env::temp_dir().join(format!(
            "privchat-capi-c-smoke-{}",
            std::process::id()
        ));
        let mut cc = host_cc().expect("no C compiler found (cc/clang/gcc)");
        let rpath = format!("-Wl,-rpath,{}", lib_dir.display());
        let lib_dir_str = lib_dir.display().to_string();
        cc.args([
            "-Wall",
            "-Werror",
            "-I",
            &format!("{manifest}/include"),
            &format!("{manifest}/tests/c_smoke.c"),
            "-L",
            &lib_dir_str,
            "-lprivchat_sdk_c_api",
            &rpath,
            "-o",
        ]);
        cc.arg(&bin);
        let o = cc.output().expect("failed to invoke cc");
        assert!(
            o.status.success(),
            "linking c_smoke against the cdylib failed:\n{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );

        let o = std::process::Command::new(&bin)
            .output()
            .expect("failed to run c_smoke");
        assert!(
            o.status.success(),
            "c_smoke runtime failure (rc={:?}):\n{}{}",
            o.status.code(),
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let _ = std::fs::remove_file(&bin);
    }
}
