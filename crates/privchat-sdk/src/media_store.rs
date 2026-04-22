use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

/// Derive yyyymm from a message's created_at timestamp (milliseconds).
pub fn yyyymm_from_ms(created_at_ms: i64) -> String {
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
        .join(yyyymm_from_ms(created_at_ms))
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

// ============================================================
// Canonical file naming (Spec §7.5 v2)
// ============================================================

/// The fixed base name for the primary attachment file.
pub const PAYLOAD_BASENAME: &str = "payload";

/// The fixed thumbnail filename (static WebP).
pub const THUMB_FILENAME: &str = "thumb.webp";

/// Fallback thumbnail filename used when the placeholder is a raw PNG
/// (hook unregistered or hook failed). Filename matches the bytes.
pub const THUMB_PNG_FILENAME: &str = "thumb.png";

/// The metadata filename.
pub const META_FILENAME: &str = "meta.json";

/// Normalize a MIME type string and return the canonical file extension (without dot).
///
/// Priority: known MIME mapping > fallback to "bin".
pub fn ext_from_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().trim() {
        // Image
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/bmp" | "image/x-bmp" => "bmp",
        "image/svg+xml" => "svg",
        "image/tiff" => "tiff",
        // Video
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "video/3gpp" => "3gp",
        // Audio
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" | "audio/vorbis" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "audio/opus" => "opus",
        "audio/amr" => "amr",
        // Document
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "text/plain" => "txt",
        // Fallback
        _ => "bin",
    }
}

/// Build the canonical payload filename: `payload.{ext}`.
pub fn payload_filename(mime: &str) -> String {
    format!("{}.{}", PAYLOAD_BASENAME, ext_from_mime(mime))
}

/// Try to extract an extension from an original filename as a weak fallback.
/// Returns `None` if the extension is empty, non-ASCII, non-alphanumeric, or too long.
/// Callers sometimes pass display text (e.g. localized labels like `[视频]`) instead of
/// a real filename; rejecting anything that doesn't look like a sane ext keeps garbage
/// out of the on-disk path.
pub fn ext_from_original_filename(filename: &str) -> Option<&str> {
    let ext = filename.rsplit('.').next()?;
    if ext.is_empty() || ext.len() > 10 {
        return None;
    }
    if !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

/// Build payload filename with fallback chain:
/// 1. Known MIME → extension
/// 2. Original filename extension (weak)
/// 3. `.bin`
pub fn payload_filename_with_fallback(mime: &str, original_filename: Option<&str>) -> String {
    let ext = ext_from_mime(mime);
    if ext != "bin" {
        return format!("{}.{}", PAYLOAD_BASENAME, ext);
    }
    // MIME was unknown/octet-stream, try original filename
    if let Some(name) = original_filename {
        if let Some(orig_ext) = ext_from_original_filename(name) {
            return format!("{}.{}", PAYLOAD_BASENAME, orig_ext.to_ascii_lowercase());
        }
    }
    format!("{}.bin", PAYLOAD_BASENAME)
}

/// Helper: Find the "primary" file in a directory.
/// Strategy: Ignore thumb/meta/JSON. If one file, return it. If multiple, return the largest one.
fn find_primary_file(dir: &Path) -> Option<PathBuf> {
    if let Ok(entries) = fs::read_dir(dir) {
        let mut candidates: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                if !p.is_file() {
                    return false;
                }
                if p.extension().map_or(false, |ext| ext == "json") {
                    return false;
                }
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == META_FILENAME {
                    return false;
                }
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                // Exclude thumbnails: thumb.*, {id}_thumb.*, {id}_thumb_v{n}.*
                if stem == "thumb" || stem.ends_with("_thumb") {
                    return false;
                }
                if let Some(idx) = stem.rfind("_thumb_v") {
                    let suffix = &stem[idx + "_thumb_v".len()..];
                    if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                        return false;
                    }
                }
                true
            })
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
