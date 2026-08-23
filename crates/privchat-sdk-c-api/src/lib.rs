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

use privchat_sdk::{NewMessage, PrivchatConfig, PrivchatSdk};
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
/// Pointer stays valid until the next failing c-api call on the same thread.
#[no_mangle]
pub extern "C" fn privchat_capi_last_error() -> *const c_char {
    LAST_ERROR.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|s| s.as_ptr())
            .unwrap_or(ptr::null())
    })
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
    match block_on_timeout(client, timeout_ms, async move {
        sdk.transfer(channel_id, route, body_bytes, timeout_ms).await
    }) {
        Ok(reply) => {
            let data_value = match std::str::from_utf8(&reply.data) {
                Ok(s) => serde_json::Value::String(s.to_string()),
                Err(_) => serde_json::to_value(&reply.data).unwrap_or(serde_json::Value::Null),
            };
            let json = serde_json::json!({
                "request_id": reply.request_id,
                "channel_id": reply.channel_id,
                "code": reply.code,
                "message": reply.message,
                "data": data_value,
            });
            into_c_string(json.to_string())
        }
        Err((_, msg)) => {
            set_last_error(msg);
            ptr::null_mut()
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

            // Pointer-returning entry points must yield NULL, not crash.
            assert!(privchat_capi_connection_state(null, 10).is_null());
            assert!(privchat_capi_session_snapshot(null, 10).is_null());
            assert!(privchat_capi_recent_events(null, 10).is_null());
            assert!(privchat_capi_timeline_events_since(null, 0, 10).is_null());
            assert!(privchat_capi_events_since(null, 0, 10).is_null());
            assert!(privchat_capi_get_message_by_id(null, 1, 10).is_null());
            assert!(privchat_capi_transfer(null, 1, route.as_ptr(), route.as_ptr(), 10).is_null());
            assert!(privchat_capi_rpc_call(null, route.as_ptr(), route.as_ptr(), 10).is_null());
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
            "privchat_capi_rpc_call",
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
