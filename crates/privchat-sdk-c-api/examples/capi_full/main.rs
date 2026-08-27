// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Full-flow example for the privchat-sdk **C ABI**.
//!
//! Mirrors the spirit of `crates/privchat-sdk/examples/accounts` but goes
//! through the stable C surface only (`privchat_capi_*`), i.e. exactly what
//! a C / C++ / GDExtension consumer would call.
//!
//! Flow:
//!   1. provision two accounts via the service admin API (create user + issue
//!      IM token) — the C ABI has no register/login by design: the host app
//!      brings its own credentials, just like the Godot addon does.
//!   2. client_create -> connect -> authenticate -> run_bootstrap_sync (x2)
//!   3. direct channel: rpc_call(channel/direct/get_or_create) + sync_channel,
//!      send_text_message on A, B observes TimelineUpdated via events_since
//!      and reads the content with get_message_by_id
//!   4. room: create room + issue ticket via admin API, subscribe_channel on
//!      A, server-side broadcast, A observes SubscriptionMessageReceived
//!      (only visible through the unfiltered events_since), unsubscribe
//!
//! Prerequisites: privchat-server running locally.
//!
//! Env overrides:
//!   PRIVCHAT_HOST            (default 127.0.0.1)
//!   PRIVCHAT_TCP_PORT        (default 9001)
//!   PRIVCHAT_SERVICE_API     (default http://127.0.0.1:9090)
//!   PRIVCHAT_SERVICE_KEY     (default your_service_master_key_here)

// NOTE: main() is deliberately synchronous. The C ABI bridges every call to
// `Runtime::block_on` on the client's own runtime; calling that from inside
// another Tokio runtime is a nested-block_on panic when the crate is linked
// as an rlib (it only "works" across the cdylib boundary by TLS accident).
// A real C/C++ host has no ambient runtime either — so neither does this
// example. Admin API calls run on a private runtime via `rt.block_on`.

use std::ffi::{c_char, CStr, CString};
use std::ptr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

// The C ABI surface: opaque handle + scalars + UTF-8 JSON strings.
// A real C/C++ consumer gets these declarations from privchat_sdk_c_api.h;
// the Rust mirror below must stay in sync with it.
#[allow(non_camel_case_types)]
enum PrivchatCapiClient {}

extern "C" {
    fn privchat_capi_client_create(config_json: *const c_char) -> *mut PrivchatCapiClient;
    fn privchat_capi_client_destroy(client: *mut PrivchatCapiClient);
    fn privchat_capi_last_error() -> *const c_char;
    fn privchat_capi_free_string(s: *mut c_char);
    fn privchat_capi_authenticate(
        client: *const PrivchatCapiClient,
        user_id: u64,
        token: *const c_char,
        device_id: *const c_char,
        timeout_ms: u64,
    ) -> i32;
    fn privchat_capi_connect(client: *const PrivchatCapiClient, timeout_ms: u64) -> i32;
    fn privchat_capi_run_bootstrap_sync(client: *const PrivchatCapiClient, timeout_ms: u64)
        -> i32;
    fn privchat_capi_subscribe_channel(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: u8,
        token: *const c_char,
        timeout_ms: u64,
    ) -> i32;
    fn privchat_capi_unsubscribe_channel(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: u8,
        timeout_ms: u64,
    ) -> i32;
    fn privchat_capi_sync_channel(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: i32,
        timeout_ms: u64,
        out_applied: *mut u64,
    ) -> i32;
    fn privchat_capi_send_text_message(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: i32,
        from_uid: u64,
        content: *const c_char,
        timeout_ms: u64,
        out_message_id: *mut u64,
    ) -> i32;
    fn privchat_capi_events_since(
        client: *const PrivchatCapiClient,
        from_sequence_id: u64,
        limit: u64,
    ) -> *mut c_char;
    fn privchat_capi_get_message_by_id(
        client: *const PrivchatCapiClient,
        message_id: u64,
        timeout_ms: u64,
    ) -> *mut c_char;
    fn privchat_capi_rpc_call(
        client: *const PrivchatCapiClient,
        route: *const c_char,
        body_json: *const c_char,
        timeout_ms: u64,
    ) -> *mut c_char;
    fn privchat_capi_open_conversation(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: i32,
        limit: u32,
        timeout_ms: u64,
    ) -> *mut c_char;
    fn privchat_capi_list_channels(
        client: *const PrivchatCapiClient,
        limit: u64,
        offset: u64,
        timeout_ms: u64,
    ) -> *mut c_char;
    fn privchat_capi_mark_read_to_pts(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        read_pts: u64,
        timeout_ms: u64,
        out_last_read_pts: *mut u64,
    ) -> i32;
    fn privchat_capi_get_channel_unread_count(
        client: *const PrivchatCapiClient,
        channel_id: u64,
        channel_type: i32,
        timeout_ms: u64,
        out_count: *mut i32,
    ) -> i32;
    fn privchat_capi_shutdown(client: *const PrivchatCapiClient, timeout_ms: u64) -> i32;
}

const TIMEOUT_MS: u64 = 15_000;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type BoxResult<T> = Result<T, BoxError>;

// ---------------------------------------------------------------------------
// Small C ABI helpers (the ownership rules documented in the header)
// ---------------------------------------------------------------------------

fn last_error() -> String {
    unsafe {
        let p = privchat_capi_last_error();
        if p.is_null() {
            "(none)".to_string()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Take ownership of a returned string, copy it out, and free the original.
unsafe fn take_string(p: *mut c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
    privchat_capi_free_string(p);
    Some(s)
}

fn cstr(s: &str) -> CString {
    CString::new(s).expect("internal NUL byte")
}

fn check_ok(rc: i32, what: &str) -> BoxResult<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("{what} failed: rc={rc} err={}", last_error()).into())
    }
}

// ---------------------------------------------------------------------------
// Service admin API (provisioning only; this is host-app territory)
// ---------------------------------------------------------------------------

struct Service {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl Service {
    async fn post(&self, path: &str, body: Value) -> BoxResult<Value> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-Service-Key", &self.key)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if v.get("code").and_then(Value::as_i64) == Some(0) {
            Ok(v["data"].clone())
        } else {
            Err(format!("{path} -> HTTP {status}: {v}").into())
        }
    }

    /// Create a user (idempotent by username) and issue an IM token for it.
    async fn provision(&self, username: &str) -> BoxResult<(u64, String, String)> {
        let user = self
            .post(
                "/api/service/users",
                serde_json::json!({ "username": username }),
            )
            .await?;
        let user_id = user["user_id"]
            .as_u64()
            .ok_or_else(|| -> BoxError { format!("create user: no user_id in {user}").into() })?;

        let token_resp = self
            .post(
                "/api/service/token/issue",
                serde_json::json!({
                    "user_id": user_id,
                    "business_system_id": "privchat-capi-example",
                    "device_info": {
                        "app_id": "macos",
                        "device_name": "capi-full-example",
                        "device_model": "Mac",
                        "os_version": "macOS",
                        "app_version": "0.1.0"
                    }
                }),
            )
            .await?;
        let im_token = token_resp["im_token"]
            .as_str()
            .ok_or("issue token: no im_token")?
            .to_string();
        let device_id = token_resp["device_id"]
            .as_str()
            .ok_or("issue token: no device_id")?
            .to_string();
        Ok((user_id, im_token, device_id))
    }
}

// ---------------------------------------------------------------------------
// One SDK client behind the C ABI
// ---------------------------------------------------------------------------

struct CapiAccount {
    name: &'static str,
    user_id: u64,
    device_id: String,
    handle: *mut PrivchatCapiClient,
    cursor: u64,
}

// The handle is only touched from the main task, sequentially.
unsafe impl Send for CapiAccount {}

impl CapiAccount {
    fn create(name: &'static str, host: &str, port: u16, data_dir: String) -> BoxResult<Self> {
        // Placeholder credentials; filled in after provisioning.
        let mut acc = Self {
            name,
            user_id: 0,
            device_id: String::new(),
            handle: ptr::null_mut(),
            cursor: 0,
        };
        let cfg = cstr(&format!(
            "{{\"endpoints\":[{{\"protocol\":\"Tcp\",\"host\":\"{host}\",\"port\":{port},\
             \"path\":null,\"use_tls\":false}}],\
             \"connection_timeout_secs\":10,\"data_dir\":\"{data_dir}\"}}"
        ));
        unsafe {
            acc.handle = privchat_capi_client_create(cfg.as_ptr());
        }
        if acc.handle.is_null() {
            return Err(format!("client_create({name}): {}", last_error()).into());
        }
        Ok(acc)
    }

    fn login(&mut self, user_id: u64, token: &str, device_id: &str) -> BoxResult<()> {
        self.user_id = user_id;
        self.device_id = device_id.to_string();
        unsafe {
            check_ok(
                privchat_capi_connect(self.handle, TIMEOUT_MS),
                &format!("connect({})", self.name),
            )?;
            check_ok(
                privchat_capi_authenticate(
                    self.handle,
                    user_id,
                    cstr(token).as_ptr(),
                    cstr(device_id).as_ptr(),
                    TIMEOUT_MS,
                ),
                &format!("authenticate({})", self.name),
            )?;
            // Local-first gate: without this, sends and timeline reads are
            // rejected by the SDK.
            check_ok(
                privchat_capi_run_bootstrap_sync(self.handle, TIMEOUT_MS),
                &format!("bootstrap({})", self.name),
            )?;
        }
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<Value> {
        let mut out = Vec::new();
        loop {
            let raw =
                unsafe { take_string(privchat_capi_events_since(self.handle, self.cursor, 256)) };
            let Some(raw) = raw else { break };
            let arr: Vec<Value> = serde_json::from_str(&raw).unwrap_or_default();
            if arr.is_empty() {
                break;
            }
            // SDK returns newest-first; normalize to chronological order and
            // advance the cursor to the highest sequence id seen.
            for e in arr.iter().rev() {
                if let Some(seq) = e["sequence_id"].as_u64() {
                    self.cursor = self.cursor.max(seq);
                }
            }
            out.extend(arr.into_iter().rev());
        }
        out
    }

    fn get_message(&self, message_id: u64) -> BoxResult<Value> {
        let raw = unsafe {
            take_string(privchat_capi_get_message_by_id(
                self.handle,
                message_id,
                TIMEOUT_MS,
            ))
        }
        .ok_or_else(|| -> BoxError {
            format!("get_message_by_id({message_id}): {}", last_error()).into()
        })?;
        Ok(serde_json::from_str(&raw)?)
    }
}

impl Drop for CapiAccount {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                privchat_capi_shutdown(self.handle, 5_000);
                privchat_capi_client_destroy(self.handle);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Flow
// ---------------------------------------------------------------------------

fn main() -> BoxResult<()> {
    println!("\nPrivChat C API Full-Flow Example (capi_full)");
    println!("=============================================");

    // Private runtime for admin-API calls only. C API calls stay OUTSIDE any
    // async context (see the module note above).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("PRIVCHAT_TCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9001);
    let service = Service {
        // .no_proxy(): reqwest 0.12 honours the macOS *system* proxy by
        // default but not its 127.0.0.1 exception list — a local dev proxy
        // (e.g. :7890) would swallow these loopback admin calls and return
        // an empty body ("EOF while parsing a value").
        http: reqwest::Client::builder().no_proxy().build()?,
        base: std::env::var("PRIVCHAT_SERVICE_API")
            .unwrap_or_else(|_| "http://127.0.0.1:9090".into()),
        key: std::env::var("PRIVCHAT_SERVICE_KEY")
            .unwrap_or_else(|_| "your_service_master_key_here".into()),
    };

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    println!("\n[1/5] provision two accounts (service admin API)");
    let (uid_a, tok_a, dev_a) = rt.block_on(service.provision(&format!("capi_full_a_{stamp}")))?;
    let (uid_b, tok_b, dev_b) = rt.block_on(service.provision(&format!("capi_full_b_{stamp}")))?;
    println!("  A uid={uid_a}  B uid={uid_b}");

    println!("\n[2/5] create clients -> connect -> authenticate -> bootstrap");
    let tmp = std::env::temp_dir().join(format!("privchat-capi-full-{stamp}-{}", std::process::id()));
    let mut alice = CapiAccount::create("alice", &host, port, format!("{}/alice", tmp.display()))?;
    let mut bob = CapiAccount::create("bob", &host, port, format!("{}/bob", tmp.display()))?;
    alice.login(uid_a, &tok_a, &dev_a)?;
    bob.login(uid_b, &tok_b, &dev_b)?;
    println!("  both accounts connected + bootstrapped");

    println!("\n[3/5] direct chat: A -> B");
    // Open the channel exactly like the GDScript facade does: rpc + sync.
    let rpc_body = cstr(&serde_json::json!({
        "target_user_id": uid_b,
        "source": null,
        "source_id": null,
        "user_id": 0
    })
    .to_string());
    let route = cstr("channel/direct/get_or_create");
    let rpc_resp = unsafe {
        take_string(privchat_capi_rpc_call(
            alice.handle,
            route.as_ptr(),
            rpc_body.as_ptr(),
            TIMEOUT_MS,
        ))
    }
    .ok_or_else(|| -> BoxError { format!("rpc_call: {}", last_error()).into() })?;
    let rpc_json: Value = serde_json::from_str(&rpc_resp)?;
    let channel_id = rpc_json["channel_id"]
        .as_u64()
        .ok_or_else(|| -> BoxError { format!("direct channel rpc: {rpc_resp}").into() })?;
    let mut applied: u64 = 0;
    unsafe {
        check_ok(
            privchat_capi_sync_channel(alice.handle, channel_id, 1, TIMEOUT_MS, &mut applied),
            "sync_channel(alice)",
        )?;
        check_ok(
            privchat_capi_sync_channel(bob.handle, channel_id, 1, TIMEOUT_MS, &mut applied),
            "sync_channel(bob)",
        )?;
    }
    println!("  channel_id={channel_id}");

    let text = format!("hello-from-capi-{stamp}");
    let mut local_id: u64 = 0;
    unsafe {
        check_ok(
            privchat_capi_send_text_message(
                alice.handle,
                channel_id,
                1,
                uid_a,
                cstr(&text).as_ptr(),
                TIMEOUT_MS,
                &mut local_id,
            ),
            "send_text_message",
        )?;
    }
    println!("  sent: {text} (local_id={local_id})");

    // B polls the unfiltered event stream for the TimelineUpdated carrying
    // the realtime message. TimelineUpdated fires for other reasons too
    // (sync_channel applied, receipt acks...), so we must match the one whose
    // reason is "realtime_message". `SequencedSdkEvent` serializes the inner
    // enum externally tagged: {"sequence_id":..,"event":{"TimelineUpdated":{..}}}.
    let msg_id = wait_for(
        &mut bob,
        |e| {
            let (kind, body) = event_view(e);
            kind == "TimelineUpdated"
                && body["channel_id"].as_u64() == Some(channel_id)
                && body["reason"].as_str() == Some("realtime_message")
                && body["message_id"].as_u64().unwrap_or(0) != 0
        },
        Duration::from_secs(20),
    )
    .map(|e| event_view(&e).1["message_id"].as_u64().unwrap_or(0))
    .ok_or("no realtime TimelineUpdated received by bob")?;
    let stored = bob.get_message(msg_id)?;
    let content = stored["content"].as_str().unwrap_or_default();
    if content != text {
        return Err(format!("content mismatch: got {content:?}, want {text:?}").into());
    }
    println!("  bob received message_id={msg_id} content={content:?}");

    println!("\n[4/5] local-first history / unread / mark-read on B");
    // open_conversation: local rows are the render truth; the freshly received
    // message must be in the latest window.
    let conv = unsafe {
        take_string(privchat_capi_open_conversation(
            bob.handle, channel_id, 1, 50, TIMEOUT_MS,
        ))
    }
    .ok_or_else(|| -> BoxError { format!("open_conversation: {}", last_error()).into() })?;
    let conv: Value = serde_json::from_str(&conv)?;
    let found = conv["messages"]
        .as_array()
        .map(|m| m.iter().any(|msg| msg["content"].as_str() == Some(text.as_str())))
        .unwrap_or(false);
    if !found {
        return Err(format!("open_conversation missing sent text: {conv}").into());
    }
    println!(
        "  open_conversation: {} msgs, has_more_before={}",
        conv["messages"].as_array().map(Vec::len).unwrap_or(0),
        conv["has_more_before"]
    );

    // channel list must show the direct channel (unread bookkeeping is
    // cursor-dependent; presence is the hard contract here).
    let channels = unsafe {
        take_string(privchat_capi_list_channels(bob.handle, 50, 0, TIMEOUT_MS))
    }
    .ok_or_else(|| -> BoxError { format!("list_channels: {}", last_error()).into() })?;
    let channels: Value = serde_json::from_str(&channels)?;
    let entry = channels
        .as_array()
        .and_then(|cs| {
            cs.iter()
                .find(|c| c["channel_id"].as_u64() == Some(channel_id))
                .cloned()
        })
        .ok_or_else(|| -> BoxError {
            format!("direct channel {channel_id} missing from list_channels: {channels}").into()
        })?;
    println!(
        "  list_channels: channel present, unread_count={}",
        entry["unread_count"]
    );

    // mark read up to the received message's pts; server echoes accepted pts,
    // local unread must drop to 0 afterwards.
    let msg_pts = stored["pts"].as_u64().unwrap_or(0);
    let mut last_read_pts: u64 = 0;
    unsafe {
        check_ok(
            privchat_capi_mark_read_to_pts(
                bob.handle, channel_id, msg_pts, TIMEOUT_MS, &mut last_read_pts,
            ),
            "mark_read_to_pts",
        )?;
    }
    let mut unread: i32 = -1;
    unsafe {
        check_ok(
            privchat_capi_get_channel_unread_count(
                bob.handle, channel_id, 1, TIMEOUT_MS, &mut unread,
            ),
            "get_channel_unread_count",
        )?;
    }
    if unread != 0 {
        return Err(format!(
            "unread should be 0 after mark_read_to_pts(pts={msg_pts}), got {unread}"
        )
        .into());
    }
    println!("  mark_read_to_pts -> last_read_pts={last_read_pts}, unread=0");

    println!("\n[5/5] room: create -> ticket -> subscribe -> broadcast -> receive");
    let room = rt.block_on(service.post(
        "/api/service/room",
        serde_json::json!({ "name": format!("capi-full-{stamp}") }),
    ))?;
    let room_id = room["channel_id"]
        .as_u64()
        .ok_or_else(|| -> BoxError { format!("create room: {room}").into() })?;
    let ticket_resp = rt.block_on(service.post(
        "/api/service/room-tickets/issue",
        serde_json::json!({
            "channel_id": room_id,
            "user_id": uid_a,
            "device_id": alice.device_id
        }),
    ))?;
    let ticket = ticket_resp["ticket"].as_str().ok_or("no ticket")?.to_string();

    unsafe {
        check_ok(
            privchat_capi_subscribe_channel(
                alice.handle,
                room_id,
                2, // Room
                cstr(&ticket).as_ptr(),
                TIMEOUT_MS,
            ),
            "subscribe_channel(room)",
        )?;
    }
    let room_text = format!("room-hello-{stamp}");
    rt.block_on(service.post(
        &format!("/api/service/room/{room_id}/broadcast"),
        serde_json::json!({ "content": room_text, "sender_id": uid_a }),
    ))?;

    // Room broadcasts arrive as SubscriptionMessageReceived — visible only
    // through the unfiltered events_since, never timeline_events_since.
    let event = wait_for(
        &mut alice,
        |e| {
            let (kind, body) = event_view(e);
            kind == "SubscriptionMessageReceived" && body["channel_id"].as_u64() == Some(room_id)
        },
        Duration::from_secs(20),
    )
    .ok_or("room broadcast not received")?;
    let payload: Vec<u8> = serde_json::from_value(event_view(&event).1["payload"].clone())?;
    let payload_text = String::from_utf8(payload)?;
    if payload_text != room_text {
        return Err(format!("room payload mismatch: {payload_text:?}").into());
    }
    println!("  alice received room broadcast: {payload_text:?}");

    unsafe {
        check_ok(
            privchat_capi_unsubscribe_channel(alice.handle, room_id, 2, TIMEOUT_MS),
            "unsubscribe_channel(room)",
        )?;
    }

    drop(alice);
    drop(bob);
    let _ = std::fs::remove_dir_all(&tmp);

    println!("\nEXAMPLE_OK");
    Ok(())
}

/// `("VariantName", &body)` view over a `SequencedSdkEvent` JSON value.
/// The inner `SdkEvent` enum is externally tagged: struct variants come as
/// `{"event":{"TimelineUpdated":{...}}}`, unit variants as
/// `{"event":"ShutdownStarted"}`.
fn event_view(e: &Value) -> (&str, &Value) {
    static NULL: Value = Value::Null;
    match &e["event"] {
        Value::String(kind) => (kind.as_str(), &NULL),
        Value::Object(map) if map.len() == 1 => {
            let (kind, body) = map.iter().next().unwrap();
            (kind.as_str(), body)
        }
        _ => ("", &NULL),
    }
}

/// Poll `acc`'s event stream until an event matching `pred` shows up (or
/// timeout). Consumes events along the way.
fn wait_for<F: Fn(&Value) -> bool>(
    acc: &mut CapiAccount,
    pred: F,
    timeout: Duration,
) -> Option<Value> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for e in acc.drain_events() {
            if pred(&e) {
                return Some(e);
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    None
}
