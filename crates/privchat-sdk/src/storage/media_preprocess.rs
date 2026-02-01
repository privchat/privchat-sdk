//! 媒体发送前处理（PrepareMedia）
//!
//! 与设计文档 MEDIA_SEND_PREPROCESSING 对应：
//! - meta.json 结构（source / target / thumbnail / processing）
//! - 消息文件目录与临时目录路径
//! - 1x1 全透明 PNG 占位（无缩略图时上传用）
//! - 图片缩略图生成（image 0.25）
//! - 视频处理钩子类型

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use chrono::TimeZone;
use crate::error::{PrivchatSDKError, Result};

// ========== meta.json 结构 ==========

/// meta.json 根结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaMeta {
    pub source: SourceMeta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<ThumbnailMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing: Option<ProcessingMeta>,
}

/// 源文件信息（必存）：根据客户端调用 SDK 传入文件的原尺寸生成
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceMeta {
    pub original_filename: String,
    pub mime: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// 目标（实际上传）信息；非原图发送时在压缩/裁剪完成后更新
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_mode: Option<String>, // "image" | "video" | "document" 等
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<u8>,
}

/// 缩略图信息（如有）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThumbnailMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

/// 处理策略与时间
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>, // e.g. "client_preprocess"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

impl MediaMeta {
    pub fn new(original_filename: String, mime: String) -> Self {
        Self {
            source: SourceMeta {
                original_filename,
                mime,
                width: None,
                height: None,
                file_size: None,
                duration_ms: None,
            },
            target: None,
            thumbnail: None,
            processing: Some(ProcessingMeta {
                strategy: Some("client_preprocess".to_string()),
                created_at: Some(chrono::Utc::now().timestamp()),
            }),
        }
    }

    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| PrivchatSDKError::Serialization(e.to_string()))
    }

    pub fn from_json_str(s: &str) -> Result<Self> {
        serde_json::from_str(s).map_err(|e| PrivchatSDKError::Serialization(e.to_string()))
    }
}

// ========== 路径助手 ==========

/// 消息文件目录：`{data_dir}/users/{uid}/files/{yyyymm}/{id}`
/// 仅持久存源文件 + meta.json
pub fn message_files_dir(data_dir: &Path, uid: &str, yyyymm: &str, message_id: i64) -> PathBuf {
    data_dir
        .join("users")
        .join(uid)
        .join("files")
        .join(yyyymm)
        .join(message_id.to_string())
}

/// 临时文件目录：`{data_dir}/users/{uid}/files/tmp/{yyyymmdd}`
/// 按日清理，只保留当日
pub fn tmp_files_dir(data_dir: &Path, uid: &str, yyyymmdd: &str) -> PathBuf {
    data_dir
        .join("users")
        .join(uid)
        .join("files")
        .join("tmp")
        .join(yyyymmdd)
}

/// 从时间戳（毫秒）得到 yyyymm（年月）
pub fn yyyymm_from_timestamp_ms(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let nsecs = ((ts_ms % 1000).abs() as u32) * 1_000_000;
    chrono::Utc
        .timestamp_opt(secs, nsecs)
        .single()
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y%m")
        .to_string()
}

/// 从时间戳（毫秒）得到 yyyymmdd（年月日，用于 tmp 按日清理）
pub fn yyyymmdd_from_timestamp_ms(ts_ms: i64) -> String {
    let secs = ts_ms / 1000;
    let nsecs = ((ts_ms % 1000).abs() as u32) * 1_000_000;
    chrono::Utc
        .timestamp_opt(secs, nsecs)
        .single()
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y%m%d")
        .to_string()
}

/// 今日 yyyymmdd（用于 tmp 目录与清理判断）
pub fn today_yyyymmdd() -> String {
    chrono::Utc::now().format("%Y%m%d").to_string()
}

// ========== 1x1 全透明 PNG ==========

/// 1x1 全透明 PNG 的字节（占位缩略图，未设置视频钩子时上传用）
pub const TRANSPARENT_PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// 将 1x1 全透明 PNG 写入指定路径（用于无缩略图时占位文件）
pub async fn write_transparent_png_1x1(path: &Path) -> Result<()> {
    tokio::fs::write(path, TRANSPARENT_PNG_1X1)
        .await
        .map_err(|e| PrivchatSDKError::IO(e.to_string()))
}

// ========== 图片尺寸与缩略图（SDK 内置，image 0.25） ==========

/// 媒体尺寸约束（Media Resize Constraint Rule）：最长边不超过 constraint，另一边等比缩放；不放大、不拉伸、不裁剪。
/// 横图（宽≥高）以宽对标 max_edge；竖图（高>宽）以高对标 max_edge。
/// 若最长边已 ≤ max_edge，本函数仍会返回 (min(w,max_edge), 等比)，调用方应保证「不放大」：仅在 max(orig) > constraint 时做 resize。
/// 返回 (new_width, new_height)，至少为 (1, 1)。
pub fn scale_to_max_edge(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_edge == 0 {
        return (width.max(1), height.max(1));
    }
    let (new_w, new_h) = if width >= height {
        let new_w = width.min(max_edge);
        let new_h = ((height as u64) * (new_w as u64) / (width as u64)) as u32;
        (new_w, new_h.max(1))
    } else {
        let new_h = height.min(max_edge);
        let new_w = ((width as u64) * (new_h as u64) / (height as u64)) as u32;
        (new_w.max(1), new_h)
    };
    (new_w, new_h)
}

/// 默认缩略图最长边（会话列表预览等）
pub const THUMBNAIL_MAX_EDGE: u32 = 90;

/// 生成图片缩略图并写入 output_path（JPEG）。
/// 返回 (width, height, file_size) 用于更新 meta.thumbnail。
/// 设计：最长边不超过 max_edge（如 90），横图以宽为基准、竖图以高为基准，另一边等比缩放。
pub fn generate_image_thumbnail_sync(
    source_path: &Path,
    output_path: &Path,
    max_edge: u32,
    quality: u8,
) -> Result<(u32, u32, u64)> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageReader;
    use std::io::BufWriter;

    let reader = ImageReader::open(source_path)
        .map_err(|e| PrivchatSDKError::IO(format!("打开图片失败: {}", e)))?;
    let img = reader
        .decode()
        .map_err(|e| PrivchatSDKError::IO(format!("解码图片失败: {}", e)))?;

    let (w, h) = (img.width(), img.height());
    let (target_w, target_h) = scale_to_max_edge(w, h, max_edge);
    let thumb = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
    let (w, h) = (thumb.width(), thumb.height());
    let rgba = thumb.to_rgba8();

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    }
    let file = std::fs::File::create(output_path).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    let mut writer = BufWriter::new(file);

    let mut encoder = JpegEncoder::new_with_quality(&mut writer, quality);
    encoder
        .encode(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| PrivchatSDKError::IO(format!("写入缩略图失败: {}", e)))?;
    writer.flush().map_err(|e: std::io::Error| PrivchatSDKError::IO(e.to_string()))?;
    let file_size = output_path.metadata().map(|m| m.len()).unwrap_or(0);

    Ok((w, h, file_size))
}

/// 按最长边限制缩放图片并写入 output_path（JPEG）。
/// 横图以宽对标 max_edge，竖图以高对标 max_edge，另一边等比缩放。
/// 返回 (new_width, new_height, file_size)，用于更新 meta.target。
pub fn resize_image_to_max_edge_sync(
    source_path: &Path,
    output_path: &Path,
    max_edge: u32,
    quality: u8,
) -> Result<(u32, u32, u64)> {
    use image::codecs::jpeg::JpegEncoder;
    use image::ImageReader;
    use std::io::BufWriter;

    let reader = ImageReader::open(source_path)
        .map_err(|e| PrivchatSDKError::IO(format!("打开图片失败: {}", e)))?;
    let img = reader
        .decode()
        .map_err(|e| PrivchatSDKError::IO(format!("解码图片失败: {}", e)))?;

    let (w, h) = (img.width(), img.height());
    let (target_w, target_h) = scale_to_max_edge(w, h, max_edge);
    let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Triangle);
    let (w, h) = (resized.width(), resized.height());
    let rgba = resized.to_rgba8();

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    }
    let file = std::fs::File::create(output_path).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    let mut writer = BufWriter::new(file);

    let mut encoder = JpegEncoder::new_with_quality(&mut writer, quality);
    encoder
        .encode(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .map_err(|e| PrivchatSDKError::IO(format!("写入失败: {}", e)))?;
    writer.flush().map_err(|e: std::io::Error| PrivchatSDKError::IO(e.to_string()))?;
    let file_size = output_path.metadata().map(|m| m.len()).unwrap_or(0);

    Ok((w, h, file_size))
}

/// 媒体处理钩子：上层实现缩略图/压缩，SDK 只调回调。
/// 入参：op_type, source_path, meta_path, output_path；返回 true 表示成功。
pub type VideoProcessHook = Arc<dyn Fn(MediaProcessOp, &Path, &Path, &Path) -> std::result::Result<bool, String> + Send + Sync>;

/// 媒体处理操作类型（Rust 底层固定，Swift/Kotlin 上层遵循此命名）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProcessOp {
    Thumbnail,
    Compress,
}

/// 发送形态：image / video / document（决定是否生成缩略图、是否调视频钩子）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    Image,
    Video,
    Document,
}

// ========== PrepareMedia 流程（步骤 3：复制源文件、写 meta、缩略图入 tmp） ==========

/// 同步将 1x1 全透明 PNG 写入路径（用于视频无钩子时占位）
#[allow(dead_code)]
fn write_transparent_png_1x1_sync(path: &Path) -> Result<()> {
    std::fs::write(path, TRANSPARENT_PNG_1X1).map_err(|e| PrivchatSDKError::IO(e.to_string()))
}

/// 读取图片宽高（用于 source meta），失败返回 None
fn read_image_dimensions(path: &Path) -> Option<(u32, u32)> {
    let reader = image::ImageReader::open(path).ok()?;
    let img = reader.decode().ok()?;
    Some((img.width(), img.height()))
}

/// PrepareMedia 步骤 3：复制源文件到消息目录、写 meta.json、生成缩略图到 tmp。
/// 失败时调用方应回滚事务、消息作废，不入队。
///
/// # 参数
/// - `data_dir`: 用户数据根目录
/// - `uid`: 用户 ID
/// - `message_id`: 本地消息 ID（用于目录名与 tmp 文件名）
/// - `timestamp_ms`: 消息插入时间（毫秒），用于 yyyymm/yyyymmdd
/// - `source_path`: 用户选择的源文件路径
/// - `original_filename`: 原始文件名（存进 meta 与目录内文件名）
/// - `mime`: MIME 类型
/// - `send_mode`: 发送形态（image 生成缩略图，video 调钩子或占位，document 不处理）
/// - `video_hook`: 视频处理钩子（仅 send_mode == Video 时使用）
///
/// # 目录与文件
/// - 持久：`files/{yyyymm}/{id}/` 下原文件 + meta.json
/// - 临时：`files/tmp/{yyyymmdd}/{id}_thumb.jpg` 或 `_thumb.png`（占位）
pub fn prepare_media_sync(
    data_dir: &Path,
    uid: &str,
    message_id: i64,
    timestamp_ms: i64,
    source_path: &Path,
    original_filename: &str,
    mime: &str,
    send_mode: SendMode,
    video_hook: Option<&VideoProcessHook>,
) -> Result<()> {
    let yyyymm = yyyymm_from_timestamp_ms(timestamp_ms);
    let yyyymmdd = yyyymmdd_from_timestamp_ms(timestamp_ms);
    let files_dir = message_files_dir(data_dir, uid, &yyyymm, message_id);
    let tmp_dir = tmp_files_dir(data_dir, uid, &yyyymmdd);

    std::fs::create_dir_all(&files_dir).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    std::fs::create_dir_all(&tmp_dir).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;

    let dest_file = files_dir.join(original_filename);
    std::fs::copy(source_path, &dest_file).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    let file_size = std::fs::metadata(&dest_file)
        .map_err(|e| PrivchatSDKError::IO(e.to_string()))?
        .len();

    let (width, height) = if send_mode == SendMode::Image {
        read_image_dimensions(&dest_file).unwrap_or((0, 0))
    } else {
        (0, 0)
    };

    let mut meta = MediaMeta::new(original_filename.to_string(), mime.to_string());
    meta.source.width = if width > 0 { Some(width) } else { None };
    meta.source.height = if height > 0 { Some(height) } else { None };
    meta.source.file_size = Some(file_size);

    let thumb_path_jpg = tmp_dir.join(format!("{}_thumb.jpg", message_id));
    let _thumb_path_png = tmp_dir.join(format!("{}_thumb.png", message_id));
    let meta_path = files_dir.join("meta.json");

    match send_mode {
        SendMode::Image => {
            let (w, h, thumb_size) = generate_image_thumbnail_sync(
                &dest_file,
                &thumb_path_jpg,
                THUMBNAIL_MAX_EDGE,
                85,
            )?;
            meta.thumbnail = Some(ThumbnailMeta {
                width: Some(w),
                height: Some(h),
                file_size: Some(thumb_size),
                mime: Some("image/jpeg".to_string()),
            });
        }
        SendMode::Video => {
            if let Some(hook) = video_hook {
                let ok = hook(
                    MediaProcessOp::Thumbnail,
                    &dest_file,
                    &meta_path,
                    &thumb_path_jpg,
                )
                .map_err(|e| PrivchatSDKError::IO(format!("视频缩略图钩子失败: {}", e)))?;
                if ok {
                    if let Ok(m) = std::fs::metadata(&thumb_path_jpg) {
                        meta.thumbnail = Some(ThumbnailMeta {
                            width: None,
                            height: None,
                            file_size: Some(m.len()),
                            mime: Some("image/jpeg".to_string()),
                        });
                    }
                }
            }
            // 无回调时不在此处生成/写占位图；由调用方在上传前直接上传 1x1 PNG 常量拿到 thumbnail_file_id 并传入任务，消费者直接使用
        }
        SendMode::Document => {}
    }

    let json = meta.to_json_string()?;
    std::fs::write(&meta_path, json).map_err(|e| PrivchatSDKError::IO(e.to_string()))?;
    Ok(())
}
