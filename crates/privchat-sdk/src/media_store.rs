use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

/// Derive yyyymm from a message's created_at timestamp (milliseconds).
fn yyyymm(created_at_ms: i64) -> String {
    DateTime::from_timestamp_millis(created_at_ms)
        .unwrap_or_else(Utc::now)
        .format("%Y%m")
        .to_string()
}

/// Calculate the message media directory under a known user_root.
/// Spec: {user_root}/files/{yyyymm}/{message_id}/
///
/// Use this when you already have `user_root` (e.g. from `StoragePaths`).
pub fn get_message_dir(user_root: &Path, message_id: i64, created_at_ms: i64) -> PathBuf {
    user_root
        .join("files")
        .join(yyyymm(created_at_ms))
        .join(message_id.to_string())
}

/// Calculate the canonical media directory for a message from global root.
/// Spec: {root}/users/{uid}/files/{yyyymm}/{message_id}/
pub fn get_canonical_message_dir(
    root: &Path,
    uid: u64,
    message_id: i64,
    created_at_ms: i64,
) -> PathBuf {
    let user_root = root.join("users").join(uid.to_string());
    get_message_dir(&user_root, message_id, created_at_ms)
}

/// Ensure the attachment directory exists and return its path.
/// Used when downloading/saving new attachments.
pub fn ensure_attachment_dir(
    root: &Path,
    uid: u64,
    message_id: i64,
    created_at_ms: i64,
) -> std::io::Result<PathBuf> {
    let dir = get_canonical_message_dir(root, uid, message_id, created_at_ms);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Resolve the path for an existing attachment.
/// Includes legacy compatibility logic.
pub fn resolve_attachment_path(
    root: &Path,
    uid: u64,
    message_id: i64,
    created_at_ms: i64,
    expected_filename: Option<&str>,
) -> Option<PathBuf> {
    // 1. Try Canonical directory
    let canonical_dir = get_canonical_message_dir(root, uid, message_id, created_at_ms);

    // A. Exact match
    if let Some(name) = expected_filename {
        let target = canonical_dir.join(name);
        if target.exists() {
            return Some(target);
        }
    }

    // B. Scan for primary file
    if let Some(file) = find_primary_file(&canonical_dir) {
        return Some(file);
    }

    // 2. Try Legacy directory (no yyyymm layer)
    let legacy_dir = root
        .join("users")
        .join(uid.to_string())
        .join("files")
        .join(message_id.to_string());
    if legacy_dir != canonical_dir && legacy_dir.exists() {
        if let Some(name) = expected_filename {
            let target = legacy_dir.join(name);
            if target.exists() {
                return Some(target);
            }
        }
        if let Some(file) = find_primary_file(&legacy_dir) {
            return Some(file);
        }
    }

    None
}

/// Helper: Find the "primary" file in a directory.
/// Strategy: Ignore JSON. If one file, return it. If multiple, return the largest one.
fn find_primary_file(dir: &Path) -> Option<PathBuf> {
    if let Ok(entries) = fs::read_dir(dir) {
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().map_or(true, |ext| ext != "json"))
            .collect();

        if candidates.is_empty() {
            return None;
        }
        if candidates.len() == 1 {
            return Some(candidates.remove(0));
        }

        // Multiple files: pick the largest one (likely the original/primary file)
        candidates.sort_by_key(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0));
        candidates.pop()
    } else {
        None
    }
}
