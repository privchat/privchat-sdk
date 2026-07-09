//! 头像本地缓存（AVATAR_CACHE_SPEC P1，user 头像）。
//!
//! 服务端头像是内容寻址的（fileId 由内容 SHA-256 派生）：换头像 ⇒ 新 URL。
//! 因此 **URL 即缓存键**——`avatar_cached_url == avatar` 且本地文件存在时缓存
//! 永远有效，无需 ETag/If-Modified-Since 协商。
//!
//! 布局（spec §3，按目标用户一层便于定向清理）：
//! `{dataDir}/users/{selfUid}/avatars/u/{targetUid}/{sha16(url)}.{ext}`，
//! 登出清空 selfUid 目录时一并回收。下载完成后更新 user 行的
//! `avatar_local_path` + `avatar_cached_url`、清理该 uid 目录下旧文件，并发既有
//! `SyncEntityChanged{entity_type:"user"}` 事件，UI 重查即得本地路径。
//! 失败静默（下次触发自然重试）。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};

use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use crate::storage_actor::StorageHandle;
use crate::{emit_sequenced_event, SdkEvent, SequencedSdkEvent};

/// 事件发射所需的三件套（与 `State` 上的字段一一对应，spawn 进后台任务用）。
pub(crate) struct AvatarEventSinks {
    pub event_tx: Option<broadcast::Sender<SdkEvent>>,
    pub event_history: Option<Arc<StdMutex<VecDeque<SequencedSdkEvent>>>>,
    pub event_seq: Option<Arc<AtomicU64>>,
    pub event_history_limit: usize,
}

/// 计算某 URL 的头像缓存文件路径（spec §3）：
/// `{user_root}/avatars/u/{targetUid}/{sha16(url)}.{ext}`，
/// sha16 = url 的 SHA-256 前 16 hex。
pub(crate) fn avatar_cache_path(user_root: &Path, target_uid: u64, url: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let hash = hex::encode(hasher.finalize());
    let sha16 = &hash[..16];
    user_root
        .join("avatars")
        .join("u")
        .join(target_uid.to_string())
        .join(format!("{sha16}.{}", infer_ext(url)))
}

/// 下载成功换代后清理 `u/{targetUid}/` 目录下除 keep 外的所有旧文件
/// （同人换头像不堆积；也顺带回收 `.part` 残留）。失败静默。
fn cleanup_stale_avatar_files(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != keep && path.is_file() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 从 URL path 部分推断扩展名；推断不出（或太长/含怪字符）兜底 `img`。
fn infer_ext(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            ext.to_ascii_lowercase()
        }
        _ => "img".to_string(),
    }
}

/// 下载 URL 到 dest：先写 `.part` 临时文件再 rename（原子换入）。
/// 头像是 PUBLIC 类匿名可读文件，不带鉴权头；明文落盘（无附件加密信封）。
async fn download_to_file(
    url: &str,
    dest: &Path,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let resp = reqwest::Client::new().get(url).send().await?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()).into());
    }
    let bytes = resp.bytes().await?;
    if bytes.is_empty() {
        return Err("empty body".into());
    }
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// AVATAR_CACHE_SPEC §8: 头像上传目标边长（server ImagePolicy targetSize 对齐）。
pub(crate) const AVATAR_UPLOAD_EDGE: u32 = 480;

/// AVATAR_CACHE_SPEC §8: 头像上传前客户端预处理。
///
/// 1. decode（image crate 仅编译 jpeg/png/webp 特性，gif/损坏格式天然解码失败
///    即拒，上传前报错不消耗流量）；EXIF orientation 已应用；
/// 2. 中心裁剪为正方形；
/// 3. 边长 >480 缩放到 480x480（≤480 保持原尺寸，不放大）；
/// 4. 编码 PNG 写系统临时目录，返回处理后文件路径（交上传管道）。
pub(crate) fn prepare_avatar_image_sync(src_path: &Path) -> crate::Result<PathBuf> {
    // 复用消息缩略图同一 decode（EXIF orientation 已应用）；actor `State` 上的
    // 无状态 helper，直接静态调用。
    let img = crate::State::decode_image_oriented(src_path)?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(crate::Error::Storage(
            "prepare avatar: empty image".to_string(),
        ));
    }
    let side = w.min(h);
    let x = (w - side) / 2;
    let y = (h - side) / 2;
    let mut square = img.crop_imm(x, y, side, side);
    if side > AVATAR_UPLOAD_EDGE {
        square = square.resize_exact(
            AVATAR_UPLOAD_EDGE,
            AVATAR_UPLOAD_EDGE,
            image::imageops::FilterType::Triangle,
        );
    }
    // PNG 编码统一走 RGBA8，避免个别源色型（如 16bit）编码分歧。
    let square = image::DynamicImage::ImageRgba8(square.to_rgba8());
    let out = std::env::temp_dir().join(format!(
        "privchat-avatar-{}-{}.png",
        std::process::id(),
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
    ));
    square
        .save_with_format(&out, image::ImageFormat::Png)
        .map_err(|e| crate::Error::Storage(format!("prepare avatar: encode png failed: {e}")))?;
    Ok(out)
}

#[derive(Default)]
struct CacheState {
    /// 下载中（按 selfUid|user_id|url 去重），完成后移除。
    inflight: HashSet<String>,
    /// 本进程内已验证「库列一致 + 文件存在」的键——sync 循环里同一批用户反复
    /// 触发时走同步快速路径，不再 spawn 任务打洪峰。
    verified: HashSet<String>,
}

/// 头像缓存管理器（挂在 actor `State` 上；Clone 共享同一份去重状态）。
#[derive(Clone, Default)]
pub(crate) struct AvatarCacheManager {
    inner: Arc<StdMutex<CacheState>>,
}

impl AvatarCacheManager {
    /// 确保 `user_id` 当前 `avatar_url` 已缓存到本地。
    ///
    /// 同步快速路径：空/非 http URL、进程内已验证、或已在下载中 ⇒ 直接返回，
    /// 不 spawn。否则后台任务：读 user 行缓存态 → 命中即标记 verified；未命中
    /// 则下载 → `set_user_avatar_cache`（URL 已再变则不写）→ 删旧文件 → 发
    /// `SyncEntityChanged{entity_type:"user"}`。
    pub(crate) fn ensure(
        &self,
        storage: StorageHandle,
        sinks: AvatarEventSinks,
        self_uid: &str,
        user_id: u64,
        avatar_url: &str,
    ) {
        let url = avatar_url.trim();
        if url.is_empty() || !url.starts_with("http") {
            return;
        }
        let key = format!("{self_uid}|{user_id}|{url}");
        {
            let Ok(mut st) = self.inner.lock() else {
                return;
            };
            if st.verified.contains(&key) || st.inflight.contains(&key) {
                return;
            }
            st.inflight.insert(key.clone());
        }
        let mgr = self.clone();
        let url = url.to_string();
        tokio::spawn(async move {
            let ok = run_ensure(&storage, &sinks, user_id, &url).await;
            if let Ok(mut st) = mgr.inner.lock() {
                st.inflight.remove(&key);
                if ok {
                    st.verified.insert(key);
                }
            }
        });
    }
}

/// 返回 true = 缓存已就绪（可进程内记忆化）；false = 失败/放弃（下次触发重试）。
async fn run_ensure(
    storage: &StorageHandle,
    sinks: &AvatarEventSinks,
    user_id: u64,
    url: &str,
) -> bool {
    // 读 user 行当前缓存态。行不存在（upsert 被 version 门控拒绝等）直接放弃。
    let row = match storage.get_user_avatar_cache(user_id).await {
        Ok(Some(row)) => row,
        Ok(None) => return false,
        Err(e) => {
            eprintln!("[SDK.avatar] read cache state failed user_id={user_id}: {e}");
            return false;
        }
    };
    if row.avatar_cached_url == url
        && !row.avatar_local_path.is_empty()
        && Path::new(&row.avatar_local_path).exists()
    {
        return true;
    }
    let paths = match storage.get_storage_paths().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[SDK.avatar] get storage paths failed user_id={user_id}: {e}");
            return false;
        }
    };
    let dest = avatar_cache_path(&paths.user_root, user_id, url);
    // 同 uid 同 URL 文件已在（此前下载成功但落库被竞态挡掉等）则跳过下载。
    if !dest.exists() {
        if let Err(e) = download_to_file(url, &dest).await {
            eprintln!("[SDK.avatar] download failed user_id={user_id} url={url}: {e}");
            return false;
        }
    }
    let dest_str = dest.to_string_lossy().to_string();
    match storage
        .set_user_avatar_cache(user_id, url.to_string(), dest_str.clone())
        .await
    {
        Ok(true) => {
            // 换代成功后清理该 uid 目录下其它旧文件（同人换头像不堆积）。
            if let Some(dir) = dest.parent() {
                cleanup_stale_avatar_files(dir, &dest);
            }
            // 旧路径可能在别的目录（历史布局/异常残留），单独兜底删除。
            if !row.avatar_local_path.is_empty() && row.avatar_local_path != dest_str {
                let _ = std::fs::remove_file(&row.avatar_local_path);
            }
            let event = SdkEvent::SyncEntityChanged {
                entity_type: "user".to_string(),
                entity_id: user_id.to_string(),
                deleted: false,
            };
            if let (Some(tx), Some(history), Some(seq)) =
                (&sinks.event_tx, &sinks.event_history, &sinks.event_seq)
            {
                emit_sequenced_event(tx, history, seq, sinks.event_history_limit, event);
            } else if let Some(tx) = &sinks.event_tx {
                let _ = tx.send(event);
            }
            true
        }
        // user.avatar 在下载期间又变了：本轮结果作废，新 URL 会另起一轮 ensure。
        Ok(false) => false,
        Err(e) => {
            eprintln!("[SDK.avatar] persist cache state failed user_id={user_id}: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_layered_by_target_uid_with_sha16() {
        let root = Path::new("/data/users/1001");
        let p = avatar_cache_path(root, 42, "https://cdn.example.com/a/b/c.webp?sig=1");
        let s = p.to_string_lossy();
        assert!(s.starts_with("/data/users/1001/avatars/u/42/"));
        assert!(s.ends_with(".webp"));
        // 文件名 = sha16 + ext
        let name = p.file_stem().unwrap().to_string_lossy();
        assert_eq!(name.len(), 16);
        // 同 URL 稳定
        assert_eq!(
            p,
            avatar_cache_path(root, 42, "https://cdn.example.com/a/b/c.webp?sig=1")
        );
        // 不同 URL / 不同目标 uid 都不同文件
        assert_ne!(p, avatar_cache_path(root, 42, "https://cdn.example.com/a/b/d.webp"));
        assert_ne!(
            p,
            avatar_cache_path(root, 43, "https://cdn.example.com/a/b/c.webp?sig=1")
        );
    }

    #[test]
    fn prepare_avatar_center_crops_and_caps_at_480() {
        let dir = std::env::temp_dir();
        // 大图：800x600 → 中心裁 600x600 → 缩到 480x480
        let src = dir.join(format!("privchat-avatar-test-big-{}.png", std::process::id()));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            800,
            600,
            image::Rgba([1u8, 2, 3, 255]),
        ))
        .save_with_format(&src, image::ImageFormat::Png)
        .unwrap();
        let out = prepare_avatar_image_sync(&src).unwrap();
        let processed = image::open(&out).unwrap();
        assert_eq!((processed.width(), processed.height()), (480, 480));
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);

        // 小图：100x50 → 50x50，不放大
        let src2 = dir.join(format!("privchat-avatar-test-small-{}.png", std::process::id()));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            100,
            50,
            image::Rgba([9u8, 9, 9, 255]),
        ))
        .save_with_format(&src2, image::ImageFormat::Png)
        .unwrap();
        let out2 = prepare_avatar_image_sync(&src2).unwrap();
        let processed2 = image::open(&out2).unwrap();
        assert_eq!((processed2.width(), processed2.height()), (50, 50));
        let _ = std::fs::remove_file(&src2);
        let _ = std::fs::remove_file(&out2);

        // 非法格式（非图片字节）直接 Err
        let bad = dir.join(format!("privchat-avatar-test-bad-{}.bin", std::process::id()));
        std::fs::write(&bad, b"definitely not an image").unwrap();
        assert!(prepare_avatar_image_sync(&bad).is_err());
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn infer_ext_falls_back_to_img() {
        assert_eq!(infer_ext("https://x/y/avatar.PNG"), "png");
        assert_eq!(infer_ext("https://x/y/avatar"), "img");
        assert_eq!(infer_ext("https://x/y/.hidden"), "img");
        assert_eq!(infer_ext("https://x/file/get/12345"), "img");
        assert_eq!(infer_ext("https://x/a.verylongext"), "img");
    }
}
