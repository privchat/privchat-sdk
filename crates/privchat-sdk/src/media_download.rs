//! Telegram-style streaming media download with HTTP Range resume + pause/cancel.
//!
//! Exposed on [`PrivchatSdk`] via `start_message_media_download`,
//! `pause_message_media_download`, `resume_message_media_download`,
//! `cancel_message_media_download`, and `get_media_download_state`.
//!
//! State transitions are broadcast on the SDK event bus as
//! [`SdkEvent::MediaDownloadStateChanged`] — both the FFI Kotlin/iOS layer and the
//! Rust-native iced UI subscribe to that bus, so they share a single source of truth.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tokio::task::JoinHandle;

use crate::{MediaDownloadState, PrivchatSdk, SdkEvent};
use privchat_protocol::ErrorCode;

/// Progress events are throttled to this interval.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);

/// Per-`message_id` download coordinator.
///
/// Owned by `PrivchatSdk`; call the high-level methods on `PrivchatSdk`
/// (`start_message_media_download` etc.) rather than touching this directly.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<AsyncMutex<HashMap<u64, HandleEntry>>>,
}

struct HandleEntry {
    state: MediaDownloadState,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    task: JoinHandle<()>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    pub async fn get_state(&self, message_id: u64) -> MediaDownloadState {
        let guard = self.inner.lock().await;
        guard
            .get(&message_id)
            .map(|h| h.state.clone())
            .unwrap_or(MediaDownloadState::Idle)
    }

    /// Start (or no-op restart if already Downloading/Paused) a download.
    /// - `target_dir` must already exist.
    /// - `payload_filename` is `payload.<ext>`.
    /// - `download_url` must be an http(s) URL (caller-resolved).
    pub async fn start(
        &self,
        sdk: PrivchatSdk,
        message_id: u64,
        download_url: String,
        target_dir: PathBuf,
        payload_filename: String,
    ) -> Result<(), String> {
        {
            let guard = self.inner.lock().await;
            if let Some(h) = guard.get(&message_id) {
                match &h.state {
                    MediaDownloadState::Downloading { .. } | MediaDownloadState::Paused { .. } => {
                        // Already in-flight: if paused, let `resume` handle the transition.
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }

        let paused = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let pause_notify = Arc::new(Notify::new());

        let manager = self.clone();
        let paused_c = paused.clone();
        let cancelled_c = cancelled.clone();
        let notify_c = pause_notify.clone();
        let sdk_c = sdk.clone();

        // UniFFI's async bridge doesn't provide a Tokio runtime, so `tokio::spawn`
        // would panic with "no reactor running". Dispatch onto the SDK's own
        // multi-thread Tokio runtime so `tokio::time::sleep`, reqwest, etc. work
        // inside `run_download`.
        let task = sdk.runtime_provider().spawn(async move {
            run_download(
                sdk_c,
                manager,
                message_id,
                download_url,
                target_dir,
                payload_filename,
                paused_c,
                cancelled_c,
                notify_c,
            )
            .await;
        });

        let initial = MediaDownloadState::Downloading {
            bytes: 0,
            total: None,
        };
        let mut guard = self.inner.lock().await;
        guard.insert(
            message_id,
            HandleEntry {
                state: initial.clone(),
                paused,
                cancelled,
                pause_notify,
                task,
            },
        );
        drop(guard);
        sdk.emit_event(SdkEvent::MediaDownloadStateChanged {
            message_id,
            state: initial,
        });
        Ok(())
    }

    pub async fn pause(&self, _sdk: &PrivchatSdk, message_id: u64) {
        let guard = self.inner.lock().await;
        let Some(h) = guard.get(&message_id) else {
            return;
        };
        let already = h.paused.swap(true, Ordering::AcqRel);
        if already {
            return;
        }
        // `run_download` emits the Paused event itself once it observes the flag;
        // nothing else to do here — avoid double-emit.
    }

    pub async fn resume(&self, _sdk: &PrivchatSdk, message_id: u64) {
        let guard = self.inner.lock().await;
        let Some(h) = guard.get(&message_id) else {
            return;
        };
        let was_paused = h.paused.swap(false, Ordering::AcqRel);
        if !was_paused {
            return;
        }
        h.pause_notify.notify_one();
    }

    pub async fn cancel(&self, sdk: &PrivchatSdk, message_id: u64) {
        let entry = {
            let mut guard = self.inner.lock().await;
            guard.remove(&message_id)
        };
        let Some(entry) = entry else { return };
        entry.cancelled.store(true, Ordering::Release);
        entry.pause_notify.notify_one();
        entry.task.abort();
        // `.part` file is intentionally left on disk so a later `start` can resume.
        sdk.emit_event(SdkEvent::MediaDownloadStateChanged {
            message_id,
            state: MediaDownloadState::Idle,
        });
    }

    async fn set_state(&self, message_id: u64, state: MediaDownloadState) {
        let mut guard = self.inner.lock().await;
        if let Some(h) = guard.get_mut(&message_id) {
            h.state = state;
        }
    }

    async fn remove(&self, message_id: u64) {
        let mut guard = self.inner.lock().await;
        guard.remove(&message_id);
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_download(
    sdk: PrivchatSdk,
    manager: DownloadManager,
    message_id: u64,
    download_url: String,
    target_dir: PathBuf,
    payload_filename: String,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
) {
    let final_path = target_dir.join(&payload_filename);
    let part_path = target_dir.join(format!("{payload_filename}.part"));

    // If the final file already exists, short-circuit success (caller may race).
    if final_path.exists() {
        let path_str = final_path.to_string_lossy().to_string();
        emit(
            &sdk,
            &manager,
            message_id,
            MediaDownloadState::Done { path: path_str },
        )
        .await;
        manager.remove(message_id).await;
        return;
    }

    // Resume from .part if present.
    let start_offset = fs::metadata(&part_path).map(|m| m.len()).unwrap_or(0);

    // Issue the request.
    let client = reqwest::Client::new();
    let mut builder = client.get(&download_url);
    if start_offset > 0 {
        builder = builder.header("Range", format!("bytes={start_offset}-"));
    }
    let resp = match builder.send().await {
        Ok(r) => r,
        Err(e) => {
            fail(
                &sdk,
                &manager,
                message_id,
                ErrorCode::NetworkError as u32,
                format!("send: {e}"),
            )
            .await;
            return;
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        fail(
            &sdk,
            &manager,
            message_id,
            ErrorCode::NetworkError as u32,
            format!("status={status} body={body}"),
        )
        .await;
        return;
    }

    let got_range = resp.status() == StatusCode::PARTIAL_CONTENT;
    let mut offset = if got_range { start_offset } else { 0 };
    // content_length on 206 is the remaining length; on 200 it is the full length.
    let total = resp.content_length().map(|len| len + offset);

    // Open the .part file: append if resuming via Range; truncate otherwise.
    let file_result = if got_range && start_offset > 0 {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part_path)
    };
    let mut file = match file_result {
        Ok(f) => f,
        Err(e) => {
            fail(
                &sdk,
                &manager,
                message_id,
                ErrorCode::InternalError as u32,
                format!("open part: {e}"),
            )
            .await;
            return;
        }
    };

    emit(
        &sdk,
        &manager,
        message_id,
        MediaDownloadState::Downloading {
            bytes: offset,
            total,
        },
    )
    .await;

    let mut last_emit = Instant::now();
    let mut resp = resp;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return;
        }
        // Pause wait loop — does not block chunk ownership.
        while paused.load(Ordering::Acquire) {
            emit(
                &sdk,
                &manager,
                message_id,
                MediaDownloadState::Paused {
                    bytes: offset,
                    total,
                },
            )
            .await;
            pause_notify.notified().await;
            if cancelled.load(Ordering::Acquire) {
                return;
            }
            if !paused.load(Ordering::Acquire) {
                emit(
                    &sdk,
                    &manager,
                    message_id,
                    MediaDownloadState::Downloading {
                        bytes: offset,
                        total,
                    },
                )
                .await;
                last_emit = Instant::now();
            }
        }

        match resp.chunk().await {
            Ok(Some(bytes)) => {
                if let Err(e) = file.write_all(&bytes) {
                    fail(
                        &sdk,
                        &manager,
                        message_id,
                        ErrorCode::InternalError as u32,
                        format!("write: {e}"),
                    )
                    .await;
                    return;
                }
                offset += bytes.len() as u64;
                if last_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    emit(
                        &sdk,
                        &manager,
                        message_id,
                        MediaDownloadState::Downloading {
                            bytes: offset,
                            total,
                        },
                    )
                    .await;
                    last_emit = Instant::now();
                }
            }
            Ok(None) => break,
            Err(e) => {
                fail(
                    &sdk,
                    &manager,
                    message_id,
                    ErrorCode::NetworkError as u32,
                    format!("chunk: {e}"),
                )
                .await;
                return;
            }
        }
    }

    if let Err(e) = file.sync_all() {
        fail(
            &sdk,
            &manager,
            message_id,
            ErrorCode::InternalError as u32,
            format!("sync: {e}"),
        )
        .await;
        return;
    }
    drop(file);

    if let Err(e) = fs::rename(&part_path, &final_path) {
        fail(
            &sdk,
            &manager,
            message_id,
            ErrorCode::InternalError as u32,
            format!("rename: {e}"),
        )
        .await;
        return;
    }

    if let Err(e) = sdk.update_media_downloaded(message_id, true).await {
        // File is on disk; DB flag will be fixed on the next bootstrap/scan.
        eprintln!("[SDK.media] update_media_downloaded failed message_id={message_id}: {e}");
    }

    let path_str = final_path.to_string_lossy().to_string();
    emit(
        &sdk,
        &manager,
        message_id,
        MediaDownloadState::Done { path: path_str },
    )
    .await;
    manager.remove(message_id).await;
}

async fn emit(
    sdk: &PrivchatSdk,
    manager: &DownloadManager,
    message_id: u64,
    state: MediaDownloadState,
) {
    manager.set_state(message_id, state.clone()).await;
    sdk.emit_event(SdkEvent::MediaDownloadStateChanged { message_id, state });
}

async fn fail(
    sdk: &PrivchatSdk,
    manager: &DownloadManager,
    message_id: u64,
    code: u32,
    message: String,
) {
    emit(
        sdk,
        manager,
        message_id,
        MediaDownloadState::Failed { code, message },
    )
    .await;
    manager.remove(message_id).await;
}
