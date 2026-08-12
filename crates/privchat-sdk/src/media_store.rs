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

/// 下载一份附件时，它在磁盘上叫什么。
///
/// 🔴 **物理名不是展示名**。磁盘名用 `file_id + 扩展名`：两个人各发一张
/// `photo.png`，用展示名当文件名就会互相覆盖，第二条消息打开是第一张图。
/// 展示名另走 [`display_file_name`]。
///
/// 扩展名的权威来源是**服务端 MIME**（`file/get_url` 的 `mime_type`），
/// 原文件名只是 MIME 认不出时的兜底：`a.jpg` 配 `image/png` 时，字节是 PNG，
/// 按 `.jpg` 存会让本地解码和后续类型推断都跟着错。
///
/// 🔴 **不能用 `file_type` 推扩展名**：它只说得出「这是图片」，而图片可能是
/// jpg/webp/gif。`file_type` 只决定消息类型。
pub fn resolve_downloaded_file_name(
    file_id: &str,
    server_filename: &str,
    server_mime: &str,
    message_filename: Option<&str>,
    message_mime: Option<&str>,
) -> String {
    let base: String = file_id
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    let base = if base.is_empty() { "attachment".to_string() } else { base };

    let ext = ext_from_known_mime(server_mime)
        .or_else(|| safe_extension_of(server_filename))
        .or_else(|| message_mime.and_then(ext_from_known_mime))
        .or_else(|| message_filename.and_then(safe_extension_of))
        .unwrap_or_else(|| "bin".to_string());

    format!("{base}.{ext}")
}

/// 这条消息在界面上显示的文件名。
///
/// 与物理名相反，这里**保留用户看到的那个名字**（服务端 `original_filename`），
/// 只做安全清洗：取最后一段路径、去掉控制字符。名字来自别人的消息，
/// 直接往路径里拼是路径穿越，直接往界面上放是伪造后缀的钓鱼。
///
/// 服务端没有名字时退回本地消息的名字，再没有就返回 None——由上层决定显示什么，
/// 而不是在这里编一个假名字。
pub fn display_file_name(server_filename: &str, message_filename: Option<&str>) -> Option<String> {
    [server_filename, message_filename.unwrap_or("")]
        .into_iter()
        .filter_map(|raw| {
            let base = raw.rsplit(['/', '\\']).next()?.trim();
            let cleaned: String = base
                .chars()
                .filter(|c| !c.is_control() && *c != '\u{202e}')
                .take(120)
                .collect();
            let cleaned = cleaned.trim().trim_matches('.').to_string();
            (!cleaned.is_empty()).then_some(cleaned)
        })
        .next()
}

/// 文件名里那截扩展名，**清洗过**才返回。
///
/// 名字来自别人的消息，不可信：只取最后一段路径（挡 `../`），扩展名只认
/// 小写字母数字且不超过 8 位。
fn safe_extension_of(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    let ext = base.rsplit_once('.')?.1.to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .then_some(ext)
}

/// 认得出的 MIME 才给扩展名。认不出时返回 None，让调用方继续往下退。
fn ext_from_known_mime(mime: &str) -> Option<String> {
    let ext = ext_from_mime(mime);
    (ext != "bin").then(|| ext.to_string())
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


#[cfg(test)]
mod downloaded_file_name_tests {
    use super::*;

    /// 🔴 本地那条消息可能既没有文件名也没有 MIME（别的客户端发的）。
    /// 只看本地就退回 `.bin`，转发出去的 PNG 就变成了「文件」消息——
    /// 类型、缩略图、预览全丢。服务端一直知道它是什么。
    #[test]
    fn the_server_metadata_decides_when_the_message_says_nothing() {
        assert_eq!(
            resolve_downloaded_file_name("25865", "dedup-test2.png", "image/png", None, None),
            "25865.png"
        );
    }


    /// 🔴 物理名必须带 `file_id`：两个人各发一张 `photo.png`，
    /// 用展示名当磁盘名就会互相覆盖——第二条消息点开是第一张图。
    #[test]
    fn two_files_with_the_same_display_name_do_not_collide() {
        let a = resolve_downloaded_file_name("101", "photo.png", "image/png", None, None);
        let b = resolve_downloaded_file_name("102", "photo.png", "image/png", None, None);
        assert_eq!((a.as_str(), b.as_str()), ("101.png", "102.png"));
        assert_ne!(a, b);
    }

    /// 🔴 原文件名与 MIME 冲突时，字节说了算：`a.jpg` + `image/png` 的内容是 PNG。
    /// 存成 `.jpg` 会让本地解码器按 JPEG 去读，也会让后续按扩展名推断的地方全错；
    /// 但**展示名仍是用户看到的那个**，不能被物理名改写。
    #[test]
    fn the_bytes_decide_the_extension_but_not_the_label() {
        assert_eq!(
            resolve_downloaded_file_name("103", "a.jpg", "image/png", None, None),
            "103.png"
        );
        assert_eq!(display_file_name("a.jpg", None).as_deref(), Some("a.jpg"));
    }

    /// 展示名同样不可信：只取最后一段路径，控制字符和 RTL 覆盖符去掉
    /// （`report<RLO>gpj.exe` 这种在界面上看起来是 .jpg）。
    #[test]
    fn the_display_name_is_cleaned_but_kept_human() {
        assert_eq!(
            display_file_name("../../etc/年度报告.pdf", None).as_deref(),
            Some("年度报告.pdf")
        );
        assert_eq!(
            display_file_name("report\u{202e}gpj.exe", None).as_deref(),
            Some("reportgpj.exe")
        );
        assert_eq!(display_file_name("", Some("local.png")).as_deref(), Some("local.png"));
        assert_eq!(display_file_name("", None), None);
    }

    /// 🔴 `file_type=image` 说不出是 jpg 还是 png。这里只认 MIME，
    /// 认错就是把 JPEG 存成 `.png`：本地预览、再次发送的类型推断全跟着错。
    #[test]
    fn an_image_is_not_automatically_a_png() {
        assert_eq!(
            resolve_downloaded_file_name("7", "", "image/jpeg", None, None),
            "7.jpg"
        );
        assert_eq!(
            resolve_downloaded_file_name("8", "", "image/webp", None, None),
            "8.webp"
        );
        assert_eq!(
            resolve_downloaded_file_name("9", "", "video/mp4", None, None),
            "9.mp4"
        );
    }

    /// 服务端没有可用名字时才轮到本地消息，最后才是 bin。
    #[test]
    fn the_message_is_only_a_fallback() {
        assert_eq!(
            resolve_downloaded_file_name("10", "", "", None, Some("image/heic")),
            "10.heic"
        );
        // 服务端 MIME 认不出时，才轮到原文件名的扩展名。
        assert_eq!(
            resolve_downloaded_file_name("10b", "report.HEIC", "application/octet-stream", None, None),
            "10b.heic"
        );
        assert_eq!(
            resolve_downloaded_file_name("11", "", "", None, Some("audio/mp4")),
            "11.m4a"
        );
        assert_eq!(resolve_downloaded_file_name("12", "", "", None, None), "12.bin");
    }

    /// 🔴 文件名来自别人的消息。拼进缓存路径之前必须清洗，
    /// 否则 `../` 或分隔符能把文件写到缓存目录之外。
    #[test]
    fn a_hostile_name_cannot_escape_the_cache_directory() {
        for hostile in [
            "../../../../etc/passwd",
            "..\\..\\windows\\system32\\evil.exe",
            "payload.png/../../x",
            "no-extension",
            "trailing.",
        ] {
            let name = resolve_downloaded_file_name("13", hostile, "", None, None);
            assert!(!name.contains('/') && !name.contains('\\'), "{hostile} -> {name}");
            assert!(!name.contains(".."), "{hostile} -> {name}");
        }
        // 合法扩展名照常保留（上面那条 exe 是合法字符，但目录部分必须被丢掉）。
        assert_eq!(
            resolve_downloaded_file_name("13", "../../etc/report.pdf", "", None, None),
            "13.pdf"
        );
    }

    /// file_id 也进路径，同样要清洗。
    #[test]
    fn the_file_id_is_sanitised_too() {
        assert_eq!(
            resolve_downloaded_file_name("../7", "x.png", "", None, None),
            "7.png"
        );
        assert_eq!(resolve_downloaded_file_name("", "x.png", "", None, None), "attachment.png");
    }
}
