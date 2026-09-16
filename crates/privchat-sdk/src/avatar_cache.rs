//! 头像本地缓存（AVATAR_CACHE_SPEC P1，user 头像）。
//!
//! 服务端头像是内容寻址的（fileId 由内容 SHA-256 派生）：换头像 ⇒ 新 URL。
//! 因此 **URL 即缓存键**——`avatar_cached_url == avatar` 且本地文件存在时缓存
//! 永远有效，无需 ETag/If-Modified-Since 协商。
//!
//! 布局（spec §3，按主键命名——路径完全由 userId 决定，渲染时不查库、
//! 直接 `exists()` 判定，换头像原地覆盖不堆积孤儿）：
//! `{dataDir}/users/{selfUid}/avatars/users/{targetUid}.img`
//! （扩展名固定 `.img`：解码器按内容嗅探，无需真实后缀；已在 iOS 验证可渲染）。
//! 登出清空 selfUid 目录时一并回收。下载完成后更新 user 行的
//! `avatar_cached_url`（本地文件对应的源 URL，与最新 avatar 不等 ⇒ 需重下），
//! 并发既有 `SyncEntityChanged{entity_type:"user"}` 事件，UI 重查即得本地路径。
//! 失败静默（下次触发自然重试）。

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{broadcast, Semaphore};

use crate::storage_actor::StorageHandle;
use crate::{emit_sequenced_event, SdkEvent, SequencedSdkEvent};

/// 事件发射所需的三件套（与 `State` 上的字段一一对应，spawn 进后台任务用）。
pub(crate) struct AvatarEventSinks {
    pub event_tx: Option<broadcast::Sender<SdkEvent>>,
    pub event_history: Option<Arc<StdMutex<VecDeque<SequencedSdkEvent>>>>,
    pub event_seq: Option<Arc<AtomicU64>>,
    pub event_history_limit: usize,
}

/// 头像缓存文件路径（spec §3）：`{user_root}/avatars/users/{targetUid}-{tag}.img`，
/// `tag` 是源 URL 的短指纹。
///
/// 🔴 **文件名必须随内容变**。这里曾经只按 `target_uid` 命名、换头像原地覆盖同一文件，
/// 理由是"渲染时不查库、直接 exists() 即可判定，不堆积孤儿"。代价是客户端换完头像
/// 界面不刷新：图片加载器按 URL 缓存，文件名不变就一直给旧位图，要杀进程重进才看得到
/// 新头像。也别想用 `?v=` / `#v=` 去骗缓存——两者都会被当成路径的一部分，文件直接打不开
/// （真机实测头像掉回字母占位）。
///
/// 带指纹之后旧文件会成为孤儿，所以 [`purge_stale_avatar_files`] 在写入新文件后删掉同一
/// uid 的其它版本。这与生成式头像（initials/九宫格）早就在用的"带指纹文件名"是同一套做法。
///
/// 扩展名固定 `.img`（解码器按内容嗅探，无需真实后缀；iOS 已验证可渲染）。
pub(crate) fn avatar_cache_path(user_root: &Path, target_uid: u64, source_url: &str) -> PathBuf {
    user_root
        .join("avatars")
        .join("users")
        .join(format!("{target_uid}-{}.img", url_tag(source_url)))
}

/// 源 URL 的短指纹（8 位十六进制）。只用来区分版本，不做安全用途。
fn url_tag(source_url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source_url.hash(&mut h);
    format!("{:08x}", (h.finish() & 0xffff_ffff) as u32)
}

/// 删掉同一 uid 的其它版本头像文件，只保留 `keep`。
///
/// 带指纹的文件名意味着换一次头像多一个文件；不清理的话，一个常换头像的账号会在
/// 目录里堆满历史版本。失败只当没清理过——留着孤儿文件远好过让换头像这件事失败。
/// 删掉某个 uid 的**全部**头像缓存文件（头像被清空时用）。
pub(crate) fn remove_cached_avatar_files(user_root: &Path, target_uid: u64) {
    let dir = user_root.join("avatars").join("users");
    // keep 指向一个不存在的名字 ⇒ 同 uid 的都会被删。
    purge_stale_avatar_files(&dir, target_uid, &dir.join(""));
}

fn purge_stale_avatar_files(dir: &Path, target_uid: u64, keep: &Path) {
    let prefix = format!("{target_uid}-");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        // 旧布局 `{uid}.img`（带指纹之前装机的）在这里一并回收：写入新版本后它已经是孤儿，
        // 留着只会让老用户的目录里永远躺一张废图。生成式头像 `{uid}.gen-*.img` 不匹配，安全。
        let legacy = format!("{target_uid}.img");
        if (name.starts_with(&prefix) || name == legacy) && name.ends_with(".img") {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 下载 URL 到 dest：先写 `.part` 临时文件再 rename（原子换入）。
/// 头像是 PUBLIC 类匿名可读文件，不带鉴权头；明文落盘（无附件加密信封）。
pub(crate) async fn download_to_file(
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
///
/// 🔴 这个数字镜像在三处，改一处就要三处一起改：这里、Web 端 canvas 实现、
/// server `ImagePolicy`。spec §8 的表头写明了这条。
pub(crate) const AVATAR_UPLOAD_EDGE: u32 = 720;

/// JPEG 质量。720x720 q85 约 60~100KB —— 比原先 480 PNG（约 500KB）还小而清晰度翻倍。
const AVATAR_JPEG_QUALITY: u8 = 85;

/// 用户在裁剪界面选定的区域，**归一化**到 0..1，相对 **EXIF 方向校正之后**的图像。
///
/// 为什么用归一化而不是像素：像素坐标要求 UI 先知道源图尺寸，而那又要求 UI 自己读
/// EXIF 判断宽高是否交换——源图带旋转时，UI 显示的是校正后的样子，拿原始像素尺寸
/// 换算会算到完全不相干的区域。归一化把这件事整个留在 Rust：UI 只说"我框的是这张图
/// 的哪一块比例"，方向校正后的实际像素由 decode 之后的尺寸决定。
#[derive(Debug, Clone, Copy)]
pub struct AvatarCrop {
    /// 裁剪区左上角 x，占图像宽度的比例。
    pub x: f32,
    /// 裁剪区左上角 y，占图像高度的比例。
    pub y: f32,
    /// 裁剪区边长，占图像**短边**的比例（正方形，所以只要一个值）。
    pub size: f32,
}

/// AVATAR_CACHE_SPEC §8: 头像上传前客户端预处理。
///
/// 1. decode（image crate 仅编译 jpeg/png/webp 特性，gif/损坏格式天然解码失败
///    即拒，上传前报错不消耗流量）；EXIF orientation 已应用；
/// 2. 按 `crop` 裁出正方形；`None` 时退回中心裁剪（兼容没有裁剪界面的调用方）；
/// 3. 缩放到 720x720（小于 720 也放大——产出规格恒定，下游不必处理"任意边长"）；
/// 4. 合成白底后编码 JPEG 写 `out_dir`，返回处理后文件路径（交上传管道）。
///
/// `out_dir` 不得用 `std::env::temp_dir()`：Android 上它是 `/data/local/tmp`，
/// 普通 app 进程无写权限（EACCES, os error 13）——真机头像上传曾因此 100% 失败。
/// app 沙箱内唯一保证可写的是宿主传入的 data_dir。
pub(crate) fn prepare_avatar_image_sync(
    src_path: &Path,
    out_dir: &Path,
    crop: Option<AvatarCrop>,
) -> crate::Result<PathBuf> {
    // 复用消息缩略图同一 decode（EXIF orientation 已应用）；actor `State` 上的
    // 无状态 helper，直接静态调用。
    let img = crate::State::decode_image_oriented(src_path)?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(crate::Error::Storage(
            "prepare avatar: empty image".to_string(),
        ));
    }

    let (x, y, side) = match crop {
        // 越界**钳制**而不是报错：UI 的浮点换算与 decode 后的整数像素之间总会差一两个
        // 像素，为此让用户重新裁一次是荒唐的（spec §8.1）。
        Some(c) => {
            let short = w.min(h) as f32;
            let side = ((c.size.clamp(0.0, 1.0) * short).round() as u32).clamp(1, w.min(h));
            let x = ((c.x.max(0.0) * w as f32).round() as u32).min(w.saturating_sub(side));
            let y = ((c.y.max(0.0) * h as f32).round() as u32).min(h.saturating_sub(side));
            (x, y, side)
        }
        None => {
            let side = w.min(h);
            ((w - side) / 2, (h - side) / 2, side)
        }
    };

    let square = img.crop_imm(x, y, side, side).resize_exact(
        AVATAR_UPLOAD_EDGE,
        AVATAR_UPLOAD_EDGE,
        image::imageops::FilterType::Lanczos3,
    );

    // 🔴 JPEG 没有透明通道：带 alpha 的源图必须先合成到白底。少了这一步，透明区域
    // 会被编码成黑色——一张透明背景的 PNG 头像上传完变成黑块。
    let rgba = square.to_rgba8();
    let mut rgb = image::RgbImage::new(AVATAR_UPLOAD_EDGE, AVATAR_UPLOAD_EDGE);
    for (x, y, px) in rgba.enumerate_pixels() {
        let [r, g, b, a] = px.0;
        let a = a as u32;
        // 源色按 alpha 与白底做直线混合。
        let blend = |c: u8| -> u8 { (((c as u32) * a + 255 * (255 - a)) / 255) as u8 };
        rgb.put_pixel(x, y, image::Rgb([blend(r), blend(g), blend(b)]));
    }

    std::fs::create_dir_all(out_dir).map_err(|e| {
        crate::Error::Storage(format!(
            "prepare avatar: create out dir {} failed: {e}",
            out_dir.display()
        ))
    })?;
    let out = out_dir.join(format!(
        "privchat-avatar-{}-{}.jpg",
        std::process::id(),
        chrono::Utc::now()
            .timestamp_nanos_opt()
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
    ));
    let mut file = std::fs::File::create(&out).map_err(|e| {
        crate::Error::Storage(format!("prepare avatar: create {} failed: {e}", out.display()))
    })?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut file, AVATAR_JPEG_QUALITY)
        .encode_image(&image::DynamicImage::ImageRgb8(rgb))
        .map_err(|e| crate::Error::Storage(format!("prepare avatar: encode jpeg failed: {e}")))?;
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

const MAX_CONCURRENT_AVATAR_CACHE_JOBS: usize = 3;

/// 头像缓存管理器（挂在 actor `State` 上；Clone 共享同一份去重状态）。
#[derive(Clone)]
pub(crate) struct AvatarCacheManager {
    inner: Arc<StdMutex<CacheState>>,
    limiter: Arc<Semaphore>,
}

impl Default for AvatarCacheManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(CacheState::default())),
            limiter: Arc::new(Semaphore::new(MAX_CONCURRENT_AVATAR_CACHE_JOBS)),
        }
    }
}

impl AvatarCacheManager {
    #[cfg(test)]
    pub(crate) fn inflight_len(&self) -> usize {
        self.inner
            .lock()
            .map(|state| state.inflight.len())
            .unwrap_or(0)
    }

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
        let limiter = self.limiter.clone();
        let owner_uid = self_uid.to_string();
        let url = url.to_string();
        tokio::spawn(async move {
            let permit = match limiter.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    if let Ok(mut st) = mgr.inner.lock() {
                        st.inflight.remove(&key);
                    }
                    return;
                }
            };
            let ok = run_ensure(&storage, &sinks, &owner_uid, user_id, &url).await;
            drop(permit);
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
    owner_uid: &str,
    user_id: u64,
    url: &str,
) -> bool {
    // 读 user 行当前缓存态。行不存在（upsert 被 version 门控拒绝等）直接放弃。
    let row = match storage
        .get_user_avatar_cache_scoped(owner_uid.to_string(), user_id)
        .await
    {
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
    let paths = match storage
        .get_storage_paths_for_uid(owner_uid.to_string())
        .await
    {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[SDK.avatar] get storage paths failed user_id={user_id}: {e}");
            return false;
        }
    };
    let dest = avatar_cache_path(&paths.user_root, user_id, url);
    // 走到这里说明本地缺失或已过期（换头像 ⇒ cached_url != url）：下载到带指纹的新
    // 文件名，成功后再删掉同 uid 的旧版本。
    if let Err(e) = download_to_file(url, &dest).await {
        eprintln!("[SDK.avatar] download failed user_id={user_id} url={url}: {e}");
        return false;
    }
    let dest_str = dest.to_string_lossy().to_string();
    match storage
        .set_user_avatar_cache_scoped(
            owner_uid.to_string(),
            user_id,
            url.to_string(),
            dest_str.clone(),
        )
        .await
    {
        Ok(true) => {
            // 文件名带指纹 ⇒ 换头像会留下旧文件，写成功后清掉同 uid 的其它版本
            // （也顺带清掉历史 uid-only / hash 布局的残留）。
            if let Some(dir) = dest.parent() {
                purge_stale_avatar_files(dir, user_id, &dest);
            }
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

/// 显式 re-cache（CLIENT_GLOBAL_STATE §4.3 P2）：把 `user_id` 的头像从 `url` 下载到本地并强制落库。
///
/// 与 [`AvatarCacheManager::ensure`] 的差别：ensure 是 sync 循环里的 fire-and-forget + URL 门控；
/// 本函数是**显式命令**（调用方已确认 url 是当前新头像，如自己上传后），await 完成并返回结果。
///
/// 流程：`get_storage_paths` → `{user_root}/avatars/users/{uid}.img` → 下载 → `force_set`。
/// **失败不污染**：url 非法 / 下载失败 / 写文件失败 都在 force_set 之前 return Err，旧
/// `avatar_local_path` / `avatar_cached_url` 保持不变。返回 `(local_path, cached_url)`。
pub(crate) async fn recache_user_avatar(
    storage: &StorageHandle,
    user_id: u64,
    url: &str,
) -> crate::Result<(String, String)> {
    let url = url.trim();
    if url.is_empty() || !url.starts_with("http") {
        return Err(crate::Error::Storage(format!(
            "recache: invalid avatar url: {url:?}"
        )));
    }
    let paths = storage.get_storage_paths().await?;
    let dest = avatar_cache_path(&paths.user_root, user_id, url);
    download_to_file(url, &dest)
        .await
        .map_err(|e| crate::Error::Storage(format!("recache download failed: {e}")))?;
    if let Some(dir) = dest.parent() {
        purge_stale_avatar_files(dir, user_id, &dest);
    }
    let dest_str = dest.to_string_lossy().to_string();
    // 只有下载 + 落盘成功才 force-set；上面任一步 Err 都不会走到这里 ⇒ 旧缓存不被覆盖。
    storage
        .force_set_user_avatar_cache(user_id, url.to_string(), dest_str.clone())
        .await?;
    Ok((dest_str, url.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn recache_rejects_invalid_url_without_pollution() {
        // 失败（url 非法/非 http）在任何 storage 写之前 return Err ⇒ 旧本地缓存不被污染。
        use crate::storage_actor::StorageHandle;
        let mut rand = [0u8; 6];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut rand);
        let dir = std::env::temp_dir().join(format!(
            "privchat-recache-test-{}-{}",
            std::process::id(),
            hex::encode(rand)
        ));
        let storage = StorageHandle::start_at(dir).expect("start storage");
        assert!(recache_user_avatar(&storage, 42, "").await.is_err());
        assert!(recache_user_avatar(&storage, 42, "   ").await.is_err());
        assert!(recache_user_avatar(&storage, 42, "ftp://x/a.png")
            .await
            .is_err());
    }

    #[test]
    fn cache_path_keyed_by_uid_and_url() {
        let root = Path::new("/data/users/1001");
        let a = avatar_cache_path(root, 42, "https://cdn/a.jpg");
        assert!(a.starts_with("/data/users/1001/avatars/users"));
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("42-"));
        // 同 uid 同 url ⇒ 同一文件（命中缓存，不重复下载）。
        assert_eq!(a, avatar_cache_path(root, 42, "https://cdn/a.jpg"));
        // 🔴 换头像必须换文件名：文件名不变，图片加载器就一直给旧位图，界面要杀进程才刷新。
        assert_ne!(a, avatar_cache_path(root, 42, "https://cdn/b.jpg"));
        assert_ne!(a, avatar_cache_path(root, 43, "https://cdn/a.jpg"));
    }

    #[test]
    fn purge_removes_other_versions_of_the_same_uid_only() {
        let dir = std::env::temp_dir().join(format!(
            "privchat-avatar-purge-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let keep = dir.join("42-aaaaaaaa.img");
        let stale = dir.join("42-bbbbbbbb.img");
        let other_uid = dir.join("43-aaaaaaaa.img");
        let unrelated = dir.join("42.jpg");
        let legacy = dir.join("42.img");
        let generated = dir.join("42.gen-abcd.img");
        for f in [&keep, &stale, &other_uid, &unrelated, &legacy, &generated] {
            std::fs::write(f, b"x").unwrap();
        }

        purge_stale_avatar_files(&dir, 42, &keep);

        assert!(keep.exists(), "当前版本被误删");
        assert!(!stale.exists(), "旧版本没被清掉，换几次头像目录就堆满了");
        assert!(other_uid.exists(), "误删了别人的头像");
        assert!(unrelated.exists(), "误删了非 .img 文件");
        assert!(!legacy.exists(), "旧布局 42.img 没被回收");
        assert!(generated.exists(), "误删了生成的字母头像");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_avatar_center_crops_and_always_outputs_720() {
        let dir = std::env::temp_dir();
        // 大图：800x600 → 中心裁 600x600 → 缩到 480x480
        let src = dir.join(format!(
            "privchat-avatar-test-big-{}.png",
            std::process::id()
        ));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            800,
            600,
            image::Rgba([1u8, 2, 3, 255]),
        ))
        .save_with_format(&src, image::ImageFormat::Png)
        .unwrap();
        let out = prepare_avatar_image_sync(&src, &dir, None).unwrap();
        let processed = image::open(&out).unwrap();
        assert_eq!((processed.width(), processed.height()), (720, 720));
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);

        // 小图同样产出 720x720：规格恒定，下游不必处理"头像可能是任意边长"。
        let src2 = dir.join(format!(
            "privchat-avatar-test-small-{}.png",
            std::process::id()
        ));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            100,
            50,
            image::Rgba([9u8, 9, 9, 255]),
        ))
        .save_with_format(&src2, image::ImageFormat::Png)
        .unwrap();
        let out2 = prepare_avatar_image_sync(&src2, &dir, None).unwrap();
        let processed2 = image::open(&out2).unwrap();
        assert_eq!((processed2.width(), processed2.height()), (720, 720));
        let _ = std::fs::remove_file(&src2);
        let _ = std::fs::remove_file(&out2);

        // 非法格式（非图片字节）直接 Err
        let bad = dir.join(format!(
            "privchat-avatar-test-bad-{}.bin",
            std::process::id()
        ));
        std::fs::write(&bad, b"definitely not an image").unwrap();
        assert!(prepare_avatar_image_sync(&bad, &dir, None).is_err());
        let _ = std::fs::remove_file(&bad);
    }

    /// 裁剪矩形要真的被采用，而不是被忽略后仍走中心裁剪。
    ///
    /// 造一张左右分色的图：左半红、右半蓝。框住最左边那一块，产出应当整幅是红的；
    /// 若裁剪参数被忽略，中心裁剪会同时取到红蓝交界，中心像素就不是纯红。
    #[test]
    fn crop_rect_selects_the_requested_region() {
        let dir = std::env::current_dir().unwrap().join("target/avatar-crop-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join(format!("crop-src-{}.png", std::process::id()));
        let mut img = image::RgbaImage::new(400, 200);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = if x < 200 {
                image::Rgba([255u8, 0, 0, 255])
            } else {
                image::Rgba([0u8, 0, 255, 255])
            };
        }
        image::DynamicImage::ImageRgba8(img)
            .save_with_format(&src, image::ImageFormat::Png)
            .unwrap();

        let out = prepare_avatar_image_sync(
            &src,
            &dir,
            Some(AvatarCrop { x: 0.0, y: 0.0, size: 1.0 }),
        )
        .unwrap();
        let processed = image::open(&out).unwrap().to_rgb8();
        assert_eq!((processed.width(), processed.height()), (720, 720));
        let c = processed.get_pixel(360, 360).0;
        assert!(
            c[0] > 200 && c[2] < 60,
            "裁剪矩形被忽略了，中心像素不是纯红: {c:?}"
        );
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }

    /// 越界的裁剪矩形按钳制处理，不报错。
    ///
    /// UI 的浮点换算与 decode 后的整数像素之间总会差一两个像素，为此让用户重裁一次
    /// 是荒唐的（spec §8.1）。
    #[test]
    fn an_out_of_bounds_crop_is_clamped_not_rejected() {
        let dir = std::env::current_dir().unwrap().join("target/avatar-crop-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join(format!("clamp-src-{}.png", std::process::id()));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            100,
            100,
            image::Rgba([7u8, 7, 7, 255]),
        ))
        .save_with_format(&src, image::ImageFormat::Png)
        .unwrap();

        // 整个矩形都在图外，且边长超过图像。
        let out = prepare_avatar_image_sync(
            &src,
            &dir,
            Some(AvatarCrop { x: 9.0, y: 9.0, size: 9.0 }),
        )
        .expect("越界矩形应当被钳制而不是报错");
        assert_eq!(image::open(&out).unwrap().width(), 720);
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }

    /// 透明像素合成到白底，而不是变成黑色。
    ///
    /// JPEG 没有透明通道。少了白底合成，一张透明背景的 PNG 头像上传完就是个黑块。
    #[test]
    fn transparent_pixels_become_white_not_black() {
        let dir = std::env::current_dir().unwrap().join("target/avatar-crop-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join(format!("alpha-src-{}.png", std::process::id()));
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            120,
            120,
            image::Rgba([0u8, 0, 0, 0]), // 全透明
        ))
        .save_with_format(&src, image::ImageFormat::Png)
        .unwrap();

        let out = prepare_avatar_image_sync(&src, &dir, None).unwrap();
        let c = image::open(&out).unwrap().to_rgb8().get_pixel(360, 360).0;
        assert!(
            c[0] > 240 && c[1] > 240 && c[2] > 240,
            "透明像素没有合成到白底，编码成了深色: {c:?}"
        );
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&out);
    }
}
