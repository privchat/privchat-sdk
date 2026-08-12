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
                // 🔴 密文缓存和半成品不是附件本体。
                //
                // 挑「最大的那个」会把它们选中：封装后的密文比明文大，写到一半的
                // `.part` / `.tmp` 也可能是。选错的后果不是报错——界面拿密文当图片
                // 渲染出一个坏块，转发时把密文当原图发出去（发送直接失败）。
                //
                // 按**这些文件确切的样子**排除，不是封杀扩展名：用户发一个真叫
                // `xx.sealed` / `xx.part` 的文件完全合法，落盘就是 `payload.sealed` /
                // `payload.part`，封杀扩展名会让这类附件在本地变成「不存在」。
                //
                // 密文缓存的名字是固定的（`body.sealed`），写入中途还会出现同名 `.tmp`
                // （见 `media_download::write_sealed_cache` / `State::seal_once`）。
                if name == "body.sealed" || name == "body.sealed.tmp" {
                    return false;
                }
                // 临时后缀是**追加**上去的（`payload.png.part`、`payload.png.decrypted.part`、
                // `合同.pdf.sealed.tmp`），去掉之后仍带扩展名；用户自己那份去掉后只剩 `payload`。
                if p.extension().map_or(false, |ext| ext == "part" || ext == "tmp")
                    && stem.contains('.')
                {
                    return false;
                }
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

        // 🔴 认得出规范名就别去猜。附件本体自 §7.5 v2 起一律叫 `payload.{ext}`，
        // 「挑最大的那个」只是给老目录留的兜底——让它先跑，就等于让目录里任何一个
        // 更大的内部文件都有机会冒充正文。
        let canonical: Vec<PathBuf> = candidates
            .iter()
            .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(PAYLOAD_BASENAME))
            .cloned()
            .collect();
        if !canonical.is_empty() {
            candidates = canonical;
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


#[cfg(test)]
mod primary_file_tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "privchat-primary-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// 🔴 密文缓存比明文大，而这里挑的是「最大的那个」。
    ///
    /// 选中密文不会报错：界面把它当图片渲染成一个坏块，转发时把它当原图发出去
    /// （发送直接失败）。半成品 `.part` 同理——下到一半的字节被当成完好的附件。
    #[test]
    fn the_ciphertext_cache_is_not_the_attachment() {
        let dir = scratch("sealed");
        fs::write(dir.join("payload.png"), vec![7u8; 512]).expect("payload");
        // 封装后更大：明文 + nonce + tag。
        fs::write(dir.join("body.sealed"), vec![0u8; 4096]).expect("sealed blob");
        fs::write(dir.join("body.sealed.json"), b"{}").expect("sealed meta");
        fs::write(dir.join("payload.png.part"), vec![0u8; 8192]).expect("half-downloaded");
        // 写缓存写到一半：崩在这里就会留下它，而它同样比明文大。
        fs::write(dir.join("body.sealed.tmp"), vec![0u8; 16384]).expect("sealed tmp");
        fs::write(dir.join("thumb.sealed"), vec![2u8; 2048]).expect("thumb sealed");
        fs::write(dir.join("thumb.sealed.tmp"), vec![2u8; 2048]).expect("thumb sealed tmp");
        fs::write(dir.join("thumb.webp"), vec![1u8; 64]).expect("thumb");

        assert_eq!(
            find_primary_file(&dir),
            Some(dir.join("payload.png")),
            "附件本体是明文成品，不是密文缓存也不是半成品"
        );
    }

    /// 🔴 用户发一个真叫 `合同.sealed` / `补丁.part` 的文件是合法的。
    ///
    /// 落盘名是 `payload.sealed` / `payload.part`。按扩展名一刀切排除的话，这类附件
    /// 在本地就成了「不存在」——本地路径解析不出来，界面点开是空的，转发也发不出去。
    #[test]
    fn a_file_the_user_really_named_sealed_is_still_an_attachment() {
        for name in ["payload.sealed", "payload.part"] {
            let dir = scratch("legit");
            fs::write(dir.join(name), vec![7u8; 512]).expect("payload");
            // 它自己的密文缓存照样叫 body.sealed，两者必须能分开。
            fs::write(dir.join("body.sealed"), vec![0u8; 4096]).expect("sealed blob");
            fs::write(dir.join("body.sealed.json"), b"{}").expect("sealed meta");

            assert_eq!(
                find_primary_file(&dir),
                Some(dir.join(name)),
                "{name} 是用户发的那份文件，不能当成内部缓存排掉"
            );
        }
    }

    /// 老目录（`payload.{ext}` 之前那套命名）才靠「挑最大的」兜底；只要规范名在，
    /// 就用它。否则目录里任何一个更大的内部文件都有机会冒充正文。
    #[test]
    fn the_canonical_payload_wins_over_any_larger_neighbour() {
        let dir = scratch("canonical");
        fs::write(dir.join("payload.png"), vec![7u8; 128]).expect("payload");
        fs::write(dir.join("legacy-huge.bin"), vec![9u8; 65536]).expect("legacy leftover");

        assert_eq!(find_primary_file(&dir), Some(dir.join("payload.png")));
    }
}
