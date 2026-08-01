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
use std::future::Future;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use tokio::sync::{oneshot, Notify, Semaphore};
use tokio::task::JoinHandle;

use crate::{MediaDownloadState, PrivchatSdk, ResolvedFileDownload, SdkEvent};
use privchat_protocol::ErrorCode;

/// Progress events are throttled to this interval.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(200);
/// Receiver-side media work is intentionally bounded. Foreground payload and
/// visible-thumbnail work may use all slots; background thumbnail hydration is
/// capped at two so one slot remains available to visible work.
const MAX_ACTIVE_DOWNLOADS: usize = 3;
const MAX_BACKGROUND_DOWNLOADS: usize = 2;
const MAX_TRACKED_DOWNLOADS: usize = 512;
const MAX_BACKGROUND_TRACKED_DOWNLOADS: usize = 480;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MediaKind {
    Payload,
    Thumbnail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadPriority {
    Visible,
    Background,
}

/// Identity of receiver-side media work.
///
/// `message_id` alone is not an identity: different accounts may legitimately
/// have the same local id, and the same account may establish a new session
/// while an old task is still running.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MediaTaskKey {
    pub owner_uid: String,
    pub session_epoch: u64,
    pub message_id: u64,
    pub media_kind: MediaKind,
}

impl MediaTaskKey {
    pub(crate) fn payload(owner_uid: String, session_epoch: u64, message_id: u64) -> Self {
        Self {
            owner_uid,
            session_epoch,
            message_id,
            media_kind: MediaKind::Payload,
        }
    }

    pub(crate) fn thumbnail(owner_uid: String, session_epoch: u64, message_id: u64) -> Self {
        Self {
            owner_uid,
            session_epoch,
            message_id,
            media_kind: MediaKind::Thumbnail,
        }
    }
}

/// Account/session-scoped receiver media coordinator.
///
/// Owned by `PrivchatSdk`; call the high-level methods on `PrivchatSdk`
/// (`start_message_media_download` etc.) rather than touching this directly.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<ManagerInner>,
}

struct ManagerInner {
    entries: Mutex<HashMap<MediaTaskKey, HandleEntry>>,
    active_slots: Arc<Semaphore>,
    background_slots: Arc<Semaphore>,
    next_task_id: AtomicU64,
}

struct HandleEntry {
    task_id: u64,
    state: MediaDownloadState,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
    task: JoinHandle<()>,
}

/// Remove a tracked task regardless of how its future exits. The task id
/// prevents an old aborted future from deleting a newer task with the same key.
struct EntryCleanup {
    manager: DownloadManager,
    key: MediaTaskKey,
    task_id: u64,
}

impl Drop for EntryCleanup {
    fn drop(&mut self) {
        self.manager.remove_if_current(&self.key, self.task_id);
    }
}

impl DownloadManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                entries: Mutex::new(HashMap::new()),
                active_slots: Arc::new(Semaphore::new(MAX_ACTIVE_DOWNLOADS)),
                background_slots: Arc::new(Semaphore::new(MAX_BACKGROUND_DOWNLOADS)),
                next_task_id: AtomicU64::new(1),
            }),
        }
    }

    pub(crate) async fn get_state(&self, key: &MediaTaskKey) -> MediaDownloadState {
        let guard = self.inner.entries.lock().expect("download manager poisoned");
        guard
            .get(key)
            .map(|h| h.state.clone())
            .unwrap_or(MediaDownloadState::Idle)
    }

    /// Submit non-payload receiver work to the same bounded coordinator.
    ///
    /// The key remains present until `job` actually finishes, so duplicate UI
    /// composition and sync/realtime overlap cannot create parallel downloads.
    pub(crate) fn submit<F>(&self, key: MediaTaskKey, priority: DownloadPriority, job: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut entries = self.inner.entries.lock().expect("download manager poisoned");
        if entries.contains_key(&key)
            || entries.len() >= MAX_TRACKED_DOWNLOADS
            || (priority == DownloadPriority::Background
                && entries.len() >= MAX_BACKGROUND_TRACKED_DOWNLOADS)
        {
            return false;
        }

        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed);
        let manager = self.clone();
        let task_key = key.clone();
        let active_slots = self.inner.active_slots.clone();
        let background_slots = self.inner.background_slots.clone();
        // A spawned future may complete on another runtime thread before the
        // caller inserts its handle. Gate it until registration is complete,
        // otherwise completion removes nothing and a dead entry is inserted.
        let (start_tx, start_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _cleanup = EntryCleanup {
                manager,
                key: task_key,
                task_id,
            };
            if start_rx.await.is_err() {
                return;
            }
            let _background_permit = match priority {
                DownloadPriority::Visible => None,
                DownloadPriority::Background => background_slots.acquire_owned().await.ok(),
            };
            let Some(_active_permit) = active_slots.acquire_owned().await.ok() else {
                return;
            };
            job.await;
        });

        entries.insert(
            key,
            HandleEntry {
                task_id,
                state: MediaDownloadState::Idle,
                paused: Arc::new(AtomicBool::new(false)),
                cancelled: Arc::new(AtomicBool::new(false)),
                pause_notify: Arc::new(Notify::new()),
                task,
            },
        );
        drop(entries);
        let _ = start_tx.send(());
        true
    }

    /// Abort every receiver-side task from the old session. This is synchronous
    /// so account switching can invalidate work before committing the new owner.
    pub(crate) fn cancel_all_scoped(&self) {
        let entries = {
            let mut guard = self.inner.entries.lock().expect("download manager poisoned");
            guard.drain().map(|(_, entry)| entry).collect::<Vec<_>>()
        };
        for entry in entries {
            entry.cancelled.store(true, Ordering::Release);
            entry.pause_notify.notify_waiters();
            entry.task.abort();
        }
    }

    #[cfg(test)]
    pub(crate) fn tracked_count(&self) -> usize {
        self.inner
            .entries
            .lock()
            .expect("download manager poisoned")
            .len()
    }

    /// Start a download from a legacy plaintext URL (no attachment encryption).
    /// Backwards-compatible entry: builds a v0 ticket and delegates to
    /// [`start_with_ticket`](Self::start_with_ticket). Prefer the ticket form for
    /// encrypted (v1) attachments so the blob is decrypted on completion.
    pub(crate) async fn start(
        &self,
        sdk: PrivchatSdk,
        key: MediaTaskKey,
        download_url: String,
        target_dir: PathBuf,
        payload_filename: String,
    ) -> Result<(), String> {
        self.start_with_ticket(
            sdk,
            key,
            ResolvedFileDownload::legacy_url(download_url),
            target_dir,
            payload_filename,
        )
        .await
    }

    /// Start (or no-op restart if already Downloading/Paused) a download from a
    /// resolved ticket (`url` + `encryption_version` + optional `cek`).
    /// - `target_dir` must already exist.
    /// - `payload_filename` is `payload.<ext>`.
    /// - On completion, a v1 ticket's `.part` blob is AES-GCM decrypted before
    ///   becoming the final file; a v0 ticket is renamed as-is.
    pub(crate) async fn start_with_ticket(
        &self,
        sdk: PrivchatSdk,
        key: MediaTaskKey,
        ticket: ResolvedFileDownload,
        target_dir: PathBuf,
        payload_filename: String,
    ) -> Result<(), String> {
        let mut guard = self.inner.entries.lock().expect("download manager poisoned");
        if guard.contains_key(&key) {
            return Ok(());
        }
        if guard.len() >= MAX_TRACKED_DOWNLOADS {
            return Err("receiver download queue is full".to_string());
        }

        let paused = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let pause_notify = Arc::new(Notify::new());

        let manager = self.clone();
        let message_id = key.message_id;
        let task_key = key.clone();
        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed);
        let paused_c = paused.clone();
        let cancelled_c = cancelled.clone();
        let notify_c = pause_notify.clone();
        let sdk_c = sdk.clone();
        let active_slots = self.inner.active_slots.clone();
        let (start_tx, start_rx) = oneshot::channel();

        // UniFFI's async bridge doesn't provide a Tokio runtime, so `tokio::spawn`
        // would panic with "no reactor running". Dispatch onto the SDK's own
        // multi-thread Tokio runtime so `tokio::time::sleep`, reqwest, etc. work
        // inside `run_download`.
        let task = sdk.runtime_provider().spawn(async move {
            let _cleanup = EntryCleanup {
                manager: manager.clone(),
                key: task_key.clone(),
                task_id,
            };
            if start_rx.await.is_err() {
                return;
            }
            let Some(_active_permit) = active_slots.acquire_owned().await.ok() else {
                return;
            };
            run_download(
                sdk_c,
                manager,
                task_key,
                ticket,
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
        guard.insert(
            key,
            HandleEntry {
                task_id,
                state: initial.clone(),
                paused,
                cancelled,
                pause_notify,
                task,
            },
        );
        drop(guard);
        let _ = start_tx.send(());
        sdk.emit_event(SdkEvent::MediaDownloadStateChanged {
            message_id,
            state: initial,
        });
        Ok(())
    }

    pub(crate) async fn pause(&self, _sdk: &PrivchatSdk, key: &MediaTaskKey) {
        let guard = self.inner.entries.lock().expect("download manager poisoned");
        let Some(h) = guard.get(key) else {
            return;
        };
        let already = h.paused.swap(true, Ordering::AcqRel);
        if already {
            return;
        }
        // `run_download` emits the Paused event itself once it observes the flag;
        // nothing else to do here — avoid double-emit.
    }

    pub(crate) async fn resume(&self, _sdk: &PrivchatSdk, key: &MediaTaskKey) {
        let guard = self.inner.entries.lock().expect("download manager poisoned");
        let Some(h) = guard.get(key) else {
            return;
        };
        let was_paused = h.paused.swap(false, Ordering::AcqRel);
        if !was_paused {
            return;
        }
        h.pause_notify.notify_one();
    }

    pub(crate) async fn cancel(&self, sdk: &PrivchatSdk, key: &MediaTaskKey) {
        let entry = {
            let mut guard = self.inner.entries.lock().expect("download manager poisoned");
            guard.remove(key)
        };
        let Some(entry) = entry else { return };
        entry.cancelled.store(true, Ordering::Release);
        entry.pause_notify.notify_one();
        entry.task.abort();
        // `.part` file is intentionally left on disk so a later `start` can resume.
        sdk.emit_event(SdkEvent::MediaDownloadStateChanged {
            message_id: key.message_id,
            state: MediaDownloadState::Idle,
        });
    }

    fn set_state(&self, key: &MediaTaskKey, state: MediaDownloadState) {
        let mut guard = self.inner.entries.lock().expect("download manager poisoned");
        if let Some(h) = guard.get_mut(key) {
            h.state = state;
        }
    }

    fn remove_if_current(&self, key: &MediaTaskKey, task_id: u64) {
        let mut guard = self.inner.entries.lock().expect("download manager poisoned");
        if guard.get(key).map(|entry| entry.task_id) == Some(task_id) {
            guard.remove(key);
        }
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
    key: MediaTaskKey,
    ticket: ResolvedFileDownload,
    target_dir: PathBuf,
    payload_filename: String,
    paused: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    pause_notify: Arc<Notify>,
) {
    let message_id = key.message_id;
    let download_url = ticket.url;
    let final_path = target_dir.join(&payload_filename);
    let part_path = target_dir.join(format!("{payload_filename}.part"));

    // If the final file already exists, short-circuit success (caller may race).
    if final_path.exists() {
        let path_str = final_path.to_string_lossy().to_string();
        emit(
            &sdk,
            &manager,
            &key,
            MediaDownloadState::Done { path: path_str },
        )
        .await;
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
                &key,
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
            &key,
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
                &key,
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
        &key,
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
                &key,
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
                    &key,
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
                        &key,
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
                        &key,
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
                    &key,
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
            &key,
            ErrorCode::InternalError as u32,
            format!("sync: {e}"),
        )
        .await;
        return;
    }
    drop(file);

    // Finalize: v0 (legacy plaintext) renames the .part as-is; v1 reads the full
    // blob (nonce||ciphertext||tag), AES-GCM decrypts it, and writes the plaintext
    // as the final file. A decrypt failure deletes the .part (it is unusable and
    // must NOT masquerade as resumable data) and surfaces as a hard failure — we
    // never fall back to writing the encrypted bytes.
    if ticket.encryption_version == 0 {
        if let Err(e) = fs::rename(&part_path, &final_path) {
            fail(
                &sdk,
                &manager,
                &key,
                ErrorCode::InternalError as u32,
                format!("rename: {e}"),
            )
            .await;
            return;
        }
    } else {
        let blob = match fs::read(&part_path) {
            Ok(b) => b,
            Err(e) => {
                fail(
                    &sdk,
                    &manager,
                    &key,
                    ErrorCode::InternalError as u32,
                    format!("read part for decrypt: {e}"),
                )
                .await;
                return;
            }
        };
        let plaintext = match crate::attachment_crypto::decrypt_downloaded_attachment_bytes(
            ticket.encryption_version,
            ticket.cek.as_deref(),
            &blob,
        ) {
            Ok(p) => p,
            Err(e) => {
                let _ = fs::remove_file(&part_path);
                fail(
                    &sdk,
                    &manager,
                    &key,
                    ErrorCode::InternalError as u32,
                    format!("decrypt attachment: {e}"),
                )
                .await;
                return;
            }
        };
        let decrypted_part = final_path.with_extension("decrypted.part");
        let write_result = (|| -> std::io::Result<()> {
            let mut output = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&decrypted_part)?;
            output.write_all(&plaintext)?;
            output.sync_all()?;
            drop(output);
            fs::rename(&decrypted_part, &final_path)
        })();
        if let Err(e) = write_result {
            let _ = fs::remove_file(&decrypted_part);
            fail(
                &sdk,
                &manager,
                &key,
                ErrorCode::InternalError as u32,
                format!("write decrypted: {e}"),
            )
            .await;
            return;
        }
        let _ = fs::remove_file(&part_path);
    }

    if let Err(e) = sdk.update_media_downloaded_scoped(&key, true).await {
        // File is on disk; DB flag will be fixed on the next bootstrap/scan.
        eprintln!("[SDK.media] update_media_downloaded failed message_id={message_id}: {e}");
    }

    let path_str = final_path.to_string_lossy().to_string();
    emit(
        &sdk,
        &manager,
        &key,
        MediaDownloadState::Done { path: path_str },
    )
    .await;
}

async fn emit(
    sdk: &PrivchatSdk,
    manager: &DownloadManager,
    key: &MediaTaskKey,
    state: MediaDownloadState,
) {
    manager.set_state(key, state.clone());
    sdk.emit_event(SdkEvent::MediaDownloadStateChanged {
        message_id: key.message_id,
        state,
    });
}

async fn fail(
    sdk: &PrivchatSdk,
    manager: &DownloadManager,
    key: &MediaTaskKey,
    code: u32,
    message: String,
) {
    emit(
        sdk,
        manager,
        key,
        MediaDownloadState::Failed { code, message },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn blocked_job(
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    ) -> impl Future<Output = ()> + Send + 'static {
        async move {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
    }

    async fn wait_until(predicate: impl Fn() -> bool) {
        for _ in 0..500 {
            if predicate() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("condition did not become true");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_work_is_capped_and_visible_work_keeps_a_slot() {
        let manager = DownloadManager::new();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        for id in 1..=8 {
            assert!(manager.submit(
                MediaTaskKey::thumbnail("a".to_string(), 1, id),
                DownloadPriority::Background,
                blocked_job(active.clone(), max_active.clone()),
            ));
        }
        wait_until(|| active.load(Ordering::SeqCst) == MAX_BACKGROUND_DOWNLOADS).await;
        assert_eq!(max_active.load(Ordering::SeqCst), 2);

        assert!(manager.submit(
            MediaTaskKey::thumbnail("a".to_string(), 1, 99),
            DownloadPriority::Visible,
            blocked_job(active.clone(), max_active.clone()),
        ));
        wait_until(|| active.load(Ordering::SeqCst) == MAX_ACTIVE_DOWNLOADS).await;
        assert_eq!(max_active.load(Ordering::SeqCst), 3);

        manager.cancel_all_scoped();
        assert_eq!(manager.tracked_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn payload_and_thumbnail_for_the_same_message_are_distinct() {
        let manager = DownloadManager::new();
        assert!(manager.submit(
            MediaTaskKey::payload("a".to_string(), 1, 7),
            DownloadPriority::Visible,
            std::future::pending(),
        ));
        assert!(manager.submit(
            MediaTaskKey::thumbnail("a".to_string(), 1, 7),
            DownloadPriority::Visible,
            std::future::pending(),
        ));
        assert_eq!(manager.tracked_count(), 2);
        manager.cancel_all_scoped();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_work_does_not_leave_a_stale_singleflight_entry() {
        let manager = DownloadManager::new();
        let key = MediaTaskKey::thumbnail("a".to_string(), 1, 8);
        assert!(manager.submit(key.clone(), DownloadPriority::Visible, async {}));
        wait_until(|| manager.tracked_count() == 0).await;

        // A completed task must not permanently suppress a later retry.
        assert!(manager.submit(key, DownloadPriority::Visible, async {}));
        wait_until(|| manager.tracked_count() == 0).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn background_admission_cannot_consume_visible_capacity() {
        let manager = DownloadManager::new();
        for id in 0..(MAX_BACKGROUND_TRACKED_DOWNLOADS as u64 + 20) {
            let _ = manager.submit(
                MediaTaskKey::thumbnail("a".to_string(), 1, id),
                DownloadPriority::Background,
                std::future::pending(),
            );
        }
        assert_eq!(manager.tracked_count(), MAX_BACKGROUND_TRACKED_DOWNLOADS);
        assert!(manager.submit(
            MediaTaskKey::thumbnail("a".to_string(), 1, 999_999),
            DownloadPriority::Visible,
            std::future::pending(),
        ));
        manager.cancel_all_scoped();
    }
}
