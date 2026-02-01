//! 媒体文件索引模块 - 文件管理和索引
//! 
//! 本模块提供：
//! - 文件索引和元数据管理
//! - 文件类型检测和分类
//! - 文件清理和垃圾回收
//! - 文件预览和缩略图管理
//! - 文件上传下载状态跟踪

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::fs;
use serde::{Serialize, Deserialize};
use sha2::{Digest, Sha256};
use crate::error::{PrivchatSDKError, Result};
use crate::storage::MediaStats;

/// 媒体文件索引组件
#[derive(Debug)]
pub struct MediaIndex {
    base_path: PathBuf,
    /// 用户媒体索引
    user_indices: Arc<RwLock<HashMap<String, UserMediaIndex>>>,
    /// 当前用户ID
    current_user: Arc<RwLock<Option<String>>>,
}

/// 用户媒体索引
#[derive(Debug)]
struct UserMediaIndex {
    /// 用户媒体目录
    media_dir: PathBuf,
    /// 文件索引（文件ID -> 文件信息）
    file_index: Arc<RwLock<HashMap<String, FileRecord>>>,
    /// 文件哈希索引（哈希 -> 文件ID）
    hash_index: Arc<RwLock<HashMap<String, String>>>,
}

/// 文件记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    /// 文件ID
    pub file_id: String,
    /// 文件名
    pub filename: String,
    /// 文件路径（相对于用户媒体目录）
    pub relative_path: String,
    /// 文件大小
    pub size: u64,
    /// 文件类型
    pub file_type: FileType,
    /// 媒体类型
    pub media_type: MediaType,
    /// 文件哈希
    pub hash: String,
    /// 创建时间
    pub created_at: u64,
    /// 最后访问时间
    pub last_accessed: u64,
    /// 上传/下载状态
    pub status: FileStatus,
    /// 元数据
    pub metadata: FileMetadata,
}

/// 文件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileType {
    /// 图片
    Image,
    /// 视频
    Video,
    /// 音频
    Audio,
    /// 文档
    Document,
    /// 其他
    Other,
}

/// 媒体类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaType {
    /// 图片类型
    Image {
        width: u32,
        height: u32,
        format: String,
    },
    /// 视频类型
    Video {
        width: u32,
        height: u32,
        duration: u32,
        format: String,
    },
    /// 音频类型
    Audio {
        duration: u32,
        format: String,
        bitrate: u32,
    },
    /// 文档类型
    Document {
        format: String,
        pages: Option<u32>,
    },
    /// 其他类型
    Other {
        format: String,
    },
}

/// 文件状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileStatus {
    /// 本地文件
    Local,
    /// 上传中
    Uploading { progress: f32 },
    /// 已上传
    Uploaded { url: String },
    /// 下载中
    Downloading { progress: f32 },
    /// 下载完成
    Downloaded,
    /// 上传失败
    UploadFailed { error: String },
    /// 下载失败
    DownloadFailed { error: String },
    /// 已删除
    Deleted,
}

/// 文件元数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// 缩略图路径
    pub thumbnail_path: Option<String>,
    /// 预览图路径
    pub preview_path: Option<String>,
    /// 文件标签
    pub tags: Vec<String>,
    /// 文件描述
    pub description: Option<String>,
    /// 扩展属性
    pub extra: HashMap<String, String>,
}

/// 文件查询条件
#[derive(Debug, Clone, Default)]
pub struct FileQuery {
    /// 文件类型筛选
    pub file_type: Option<FileType>,
    /// 文件名模糊匹配
    pub filename_pattern: Option<String>,
    /// 大小范围
    pub size_range: Option<(u64, u64)>,
    /// 时间范围
    pub time_range: Option<(u64, u64)>,
    /// 状态筛选
    pub status: Option<FileStatus>,
    /// 标签筛选
    pub tags: Option<Vec<String>>,
    /// 排序方式
    pub sort_by: SortBy,
    /// 限制数量
    pub limit: Option<usize>,
}

/// 排序方式
#[derive(Debug, Clone)]
pub enum SortBy {
    /// 按创建时间排序
    CreatedAt(bool), // true: 升序, false: 降序
    /// 按文件大小排序
    Size(bool),
    /// 按最后访问时间排序
    LastAccessed(bool),
    /// 按文件名排序
    Filename(bool),
}

impl Default for SortBy {
    fn default() -> Self {
        SortBy::CreatedAt(false) // 默认按创建时间降序
    }
}

impl MediaIndex {
    /// 创建新的媒体索引实例
    pub async fn new(base_path: &Path) -> Result<Self> {
        let base_path = base_path.to_path_buf();
        
        Ok(Self {
            base_path,
            user_indices: Arc::new(RwLock::new(HashMap::new())),
            current_user: Arc::new(RwLock::new(None)),
        })
    }
    
    /// 初始化用户媒体索引
    pub async fn init_user_index(&self, uid: &str) -> Result<()> {
        let user_dir = self.base_path.join("users").join(uid);
        let media_dir = user_dir.join("media");
        
        // 创建媒体目录
        fs::create_dir_all(&media_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("创建媒体目录失败: {}", e)))?;
        
        // 创建子目录
        let subdirs = ["images", "videos", "audios", "documents", "others", "thumbnails", "previews"];
        for subdir in subdirs {
            let subdir_path = media_dir.join(subdir);
            fs::create_dir_all(&subdir_path).await
                .map_err(|e| PrivchatSDKError::IO(format!("创建媒体子目录失败: {}", e)))?;
        }
        
        // 扫描现有文件并建立索引
        let file_index = Arc::new(RwLock::new(HashMap::new()));
        let hash_index = Arc::new(RwLock::new(HashMap::new()));
        
        self.scan_and_index_files(&media_dir, &file_index, &hash_index).await?;
        
        let user_index = UserMediaIndex {
            media_dir,
            file_index,
            hash_index,
        };
        
        let mut user_indices = self.user_indices.write().await;
        user_indices.insert(uid.to_string(), user_index);
        
        tracing::info!("用户媒体索引初始化完成: {}", uid);
        
        Ok(())
    }
    
    /// 切换用户
    pub async fn switch_user(&self, uid: &str) -> Result<()> {
        // 如果用户索引不存在，先初始化
        let user_indices = self.user_indices.read().await;
        if !user_indices.contains_key(uid) {
            drop(user_indices);
            self.init_user_index(uid).await?;
        }
        
        // 更新当前用户
        let mut current_user = self.current_user.write().await;
        *current_user = Some(uid.to_string());
        
        Ok(())
    }
    
    /// 清理用户数据
    pub async fn cleanup_user_data(&self, uid: &str) -> Result<()> {
        let mut user_indices = self.user_indices.write().await;
        user_indices.remove(uid);
        
        // 删除用户媒体目录
        let user_dir = self.base_path.join("users").join(uid);
        let media_dir = user_dir.join("media");
        
        if media_dir.exists() {
            fs::remove_dir_all(&media_dir).await
                .map_err(|e| PrivchatSDKError::IO(format!("删除用户媒体目录失败: {}", e)))?;
        }
        
        Ok(())
    }
    
    /// 获取当前用户索引
    async fn get_current_user_index(&self) -> Result<UserMediaIndex> {
        let current_user = self.current_user.read().await;
        let uid = current_user.as_ref()
            .ok_or_else(|| PrivchatSDKError::NotConnected)?;
        
        let user_indices = self.user_indices.read().await;
        let user_index = user_indices.get(uid)
            .ok_or_else(|| PrivchatSDKError::KvStore("用户媒体索引不存在".to_string()))?;
        
        Ok(UserMediaIndex {
            media_dir: user_index.media_dir.clone(),
            file_index: user_index.file_index.clone(),
            hash_index: user_index.hash_index.clone(),
        })
    }
    
    /// 添加文件到索引
    pub async fn add_file(&self, file_path: &Path, file_id: Option<String>) -> Result<FileRecord> {
        let user_index = self.get_current_user_index().await?;
        
        // 检查文件是否存在
        if !file_path.exists() {
            return Err(PrivchatSDKError::IO("文件不存在".to_string()));
        }
        
        // 获取文件信息
        let metadata = fs::metadata(file_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("获取文件元数据失败: {}", e)))?;
        
        let size = metadata.len();
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // 计算文件哈希
        let hash = self.calculate_file_hash(file_path).await?;
        
        // 检查文件是否已存在（通过哈希）
        let hash_index = user_index.hash_index.read().await;
        if let Some(existing_file_id) = hash_index.get(&hash) {
            let file_index = user_index.file_index.read().await;
            if let Some(existing_record) = file_index.get(existing_file_id) {
                return Ok(existing_record.clone());
            }
        }
        drop(hash_index);
        
        // 生成文件ID
        let file_id = file_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        
        // 确定文件类型和媒体类型
        let file_type = self.detect_file_type(file_path).await?;
        let media_type = self.detect_media_type(file_path, &file_type).await?;
        
        // 确定目标目录
        let target_subdir = match file_type {
            FileType::Image => "images",
            FileType::Video => "videos",
            FileType::Audio => "audios",
            FileType::Document => "documents",
            FileType::Other => "others",
        };
        
        let target_dir = user_index.media_dir.join(target_subdir);
        let filename = file_path.file_name()
            .ok_or_else(|| PrivchatSDKError::IO("无法获取文件名".to_string()))?
            .to_string_lossy()
            .to_string();
        
        let target_path = target_dir.join(&filename);
        let relative_path = format!("{}/{}", target_subdir, filename);
        
        // 如果文件不在目标位置，则复制
        if file_path != target_path {
            fs::copy(file_path, &target_path).await
                .map_err(|e| PrivchatSDKError::IO(format!("复制文件失败: {}", e)))?;
        }
        
        // 创建文件记录
        let file_record = FileRecord {
            file_id: file_id.clone(),
            filename,
            relative_path,
            size,
            file_type,
            media_type,
            hash: hash.clone(),
            created_at,
            last_accessed: created_at,
            status: FileStatus::Local,
            metadata: FileMetadata {
                thumbnail_path: None,
                preview_path: None,
                tags: Vec::new(),
                description: None,
                extra: HashMap::new(),
            },
        };
        
        // 更新索引
        let mut file_index = user_index.file_index.write().await;
        file_index.insert(file_id.clone(), file_record.clone());
        
        let mut hash_index = user_index.hash_index.write().await;
        hash_index.insert(hash, file_id);
        
        Ok(file_record)
    }
    
    /// 获取文件记录
    pub async fn get_file(&self, file_id: &str) -> Result<Option<FileRecord>> {
        let user_index = self.get_current_user_index().await?;
        let file_index = user_index.file_index.read().await;
        
        if let Some(mut file_record) = file_index.get(file_id).cloned() {
            // 更新最后访问时间
            file_record.last_accessed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            
            drop(file_index);
            
            // 异步更新索引
            let mut file_index = user_index.file_index.write().await;
            file_index.insert(file_id.to_string(), file_record.clone());
            
            Ok(Some(file_record))
        } else {
            Ok(None)
        }
    }
    
    /// 获取文件完整路径
    pub async fn get_file_path(&self, file_id: &str) -> Result<Option<PathBuf>> {
        let user_index = self.get_current_user_index().await?;
        let file_index = user_index.file_index.read().await;
        
        if let Some(file_record) = file_index.get(file_id) {
            let full_path = user_index.media_dir.join(&file_record.relative_path);
            Ok(Some(full_path))
        } else {
            Ok(None)
        }
    }
    
    /// 查询文件
    pub async fn query_files(&self, query: &FileQuery) -> Result<Vec<FileRecord>> {
        let user_index = self.get_current_user_index().await?;
        let file_index = user_index.file_index.read().await;
        
        let mut results: Vec<FileRecord> = file_index.values().cloned().collect();
        
        // 应用筛选条件
        if let Some(file_type) = &query.file_type {
            results.retain(|r| r.file_type == *file_type);
        }
        
        if let Some(pattern) = &query.filename_pattern {
            results.retain(|r| r.filename.contains(pattern));
        }
        
        if let Some((min_size, max_size)) = query.size_range {
            results.retain(|r| r.size >= min_size && r.size <= max_size);
        }
        
        if let Some((start_time, end_time)) = query.time_range {
            results.retain(|r| r.created_at >= start_time && r.created_at <= end_time);
        }
        
        if let Some(tags) = &query.tags {
            results.retain(|r| tags.iter().any(|tag| r.metadata.tags.contains(tag)));
        }
        
        // 排序
        match &query.sort_by {
            SortBy::CreatedAt(ascending) => {
                results.sort_by(|a, b| {
                    if *ascending {
                        a.created_at.cmp(&b.created_at)
                    } else {
                        b.created_at.cmp(&a.created_at)
                    }
                });
            }
            SortBy::Size(ascending) => {
                results.sort_by(|a, b| {
                    if *ascending {
                        a.size.cmp(&b.size)
                    } else {
                        b.size.cmp(&a.size)
                    }
                });
            }
            SortBy::LastAccessed(ascending) => {
                results.sort_by(|a, b| {
                    if *ascending {
                        a.last_accessed.cmp(&b.last_accessed)
                    } else {
                        b.last_accessed.cmp(&a.last_accessed)
                    }
                });
            }
            SortBy::Filename(ascending) => {
                results.sort_by(|a, b| {
                    if *ascending {
                        a.filename.cmp(&b.filename)
                    } else {
                        b.filename.cmp(&a.filename)
                    }
                });
            }
        }
        
        // 限制数量
        if let Some(limit) = query.limit {
            results.truncate(limit);
        }
        
        Ok(results)
    }
    
    /// 删除文件
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let user_index = self.get_current_user_index().await?;
        let mut file_index = user_index.file_index.write().await;
        
        if let Some(file_record) = file_index.remove(file_id) {
            // 从哈希索引中删除
            let mut hash_index = user_index.hash_index.write().await;
            hash_index.remove(&file_record.hash);
            
            // 删除实际文件
            let file_path = user_index.media_dir.join(&file_record.relative_path);
            if file_path.exists() {
                fs::remove_file(&file_path).await
                    .map_err(|e| PrivchatSDKError::IO(format!("删除文件失败: {}", e)))?;
            }
            
            // 删除缩略图和预览图
            if let Some(thumbnail_path) = &file_record.metadata.thumbnail_path {
                let thumbnail_full_path = user_index.media_dir.join(thumbnail_path);
                if thumbnail_full_path.exists() {
                    let _ = fs::remove_file(&thumbnail_full_path).await;
                }
            }
            
            if let Some(preview_path) = &file_record.metadata.preview_path {
                let preview_full_path = user_index.media_dir.join(preview_path);
                if preview_full_path.exists() {
                    let _ = fs::remove_file(&preview_full_path).await;
                }
            }
        }
        
        Ok(())
    }
    
    /// 清理过期文件
    pub async fn cleanup_expired_files(&self, max_age_days: u32) -> Result<u64> {
        let user_index = self.get_current_user_index().await?;
        let mut file_index = user_index.file_index.write().await;
        
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let max_age_seconds = max_age_days as u64 * 24 * 60 * 60;
        let mut removed_count = 0u64;
        
        let mut files_to_remove = Vec::new();
        
        for (file_id, file_record) in file_index.iter() {
            if current_time > file_record.last_accessed + max_age_seconds {
                files_to_remove.push(file_id.clone());
            }
        }
        
        for file_id in files_to_remove {
            if let Some(file_record) = file_index.remove(&file_id) {
                // 删除实际文件
                let file_path = user_index.media_dir.join(&file_record.relative_path);
                if file_path.exists() {
                    let _ = fs::remove_file(&file_path).await;
                }
                
                removed_count += 1;
            }
        }
        
        Ok(removed_count)
    }
    
    /// 获取媒体统计信息
    pub async fn get_stats(&self) -> Result<MediaStats> {
        let user_index = self.get_current_user_index().await?;
        let file_index = user_index.file_index.read().await;
        
        let mut total_files = 0u64;
        let mut total_size = 0u64;
        let mut image_count = 0u64;
        let mut video_count = 0u64;
        let mut audio_count = 0u64;
        let mut document_count = 0u64;
        
        for file_record in file_index.values() {
            total_files += 1;
            total_size += file_record.size;
            
            match file_record.file_type {
                FileType::Image => image_count += 1,
                FileType::Video => video_count += 1,
                FileType::Audio => audio_count += 1,
                FileType::Document => document_count += 1,
                FileType::Other => {}
            }
        }
        
        Ok(MediaStats {
            total_files,
            total_size,
            image_count,
            video_count,
            audio_count,
            document_count,
        })
    }
    
    /// 扫描并索引现有文件
    async fn scan_and_index_files(
        &self,
        media_dir: &Path,
        file_index: &Arc<RwLock<HashMap<String, FileRecord>>>,
        hash_index: &Arc<RwLock<HashMap<String, String>>>,
    ) -> Result<()> {
        let mut entries = fs::read_dir(media_dir).await
            .map_err(|e| PrivchatSDKError::IO(format!("读取媒体目录失败: {}", e)))?;
        
        while let Some(entry) = entries.next_entry().await
            .map_err(|e| PrivchatSDKError::IO(format!("遍历媒体目录失败: {}", e)))? {
            
            let path = entry.path();
            if path.is_file() {
                // 为现有文件创建索引
                let file_id = uuid::Uuid::new_v4().to_string();
                
                if let Ok(file_record) = self.create_file_record_from_path(&path, &file_id, media_dir).await {
                    let mut file_index = file_index.write().await;
                    file_index.insert(file_id.clone(), file_record.clone());
                    
                    let mut hash_index = hash_index.write().await;
                    hash_index.insert(file_record.hash, file_id);
                }
            } else if path.is_dir() {
                // 递归扫描子目录
                Box::pin(self.scan_and_index_files(&path, file_index, hash_index)).await?;
            }
        }
        
        Ok(())
    }
    
    /// 从文件路径创建文件记录
    async fn create_file_record_from_path(
        &self,
        file_path: &Path,
        file_id: &str,
        media_dir: &Path,
    ) -> Result<FileRecord> {
        let metadata = fs::metadata(file_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("获取文件元数据失败: {}", e)))?;
        
        let size = metadata.len();
        let created_at = metadata.created()
            .unwrap_or(SystemTime::now())
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        let hash = self.calculate_file_hash(file_path).await?;
        let file_type = self.detect_file_type(file_path).await?;
        let media_type = self.detect_media_type(file_path, &file_type).await?;
        
        let filename = file_path.file_name()
            .ok_or_else(|| PrivchatSDKError::IO("无法获取文件名".to_string()))?
            .to_string_lossy()
            .to_string();
        
        let relative_path = file_path.strip_prefix(media_dir)
            .map_err(|e| PrivchatSDKError::IO(format!("获取相对路径失败: {}", e)))?
            .to_string_lossy()
            .to_string();
        
        Ok(FileRecord {
            file_id: file_id.to_string(),
            filename,
            relative_path,
            size,
            file_type,
            media_type,
            hash,
            created_at,
            last_accessed: created_at,
            status: FileStatus::Local,
            metadata: FileMetadata {
                thumbnail_path: None,
                preview_path: None,
                tags: Vec::new(),
                description: None,
                extra: HashMap::new(),
            },
        })
    }
    
    /// 计算文件哈希
    async fn calculate_file_hash(&self, file_path: &Path) -> Result<String> {
        let content = fs::read(file_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("读取文件内容失败: {}", e)))?;
        
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let result = hasher.finalize();
        
        Ok(hex::encode(result))
    }
    
    /// 检测文件类型
    async fn detect_file_type(&self, file_path: &Path) -> Result<FileType> {
        let extension = file_path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        
        match extension.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "svg" => Ok(FileType::Image),
            "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" | "webm" => Ok(FileType::Video),
            "mp3" | "wav" | "flac" | "aac" | "ogg" | "wma" | "m4a" => Ok(FileType::Audio),
            "pdf" | "doc" | "docx" | "txt" | "rtf" | "ppt" | "pptx" | "xls" | "xlsx" => Ok(FileType::Document),
            _ => Ok(FileType::Other),
        }
    }
    
    /// 检测媒体类型
    async fn detect_media_type(&self, file_path: &Path, file_type: &FileType) -> Result<MediaType> {
        let extension = file_path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        
        match file_type {
            FileType::Image => Ok(MediaType::Image {
                width: 0,  // 实际应用中可以使用图像处理库获取尺寸
                height: 0,
                format: extension,
            }),
            FileType::Video => Ok(MediaType::Video {
                width: 0,  // 实际应用中可以使用视频处理库获取信息
                height: 0,
                duration: 0,
                format: extension,
            }),
            FileType::Audio => Ok(MediaType::Audio {
                duration: 0,  // 实际应用中可以使用音频处理库获取信息
                format: extension,
                bitrate: 0,
            }),
            FileType::Document => Ok(MediaType::Document {
                format: extension,
                pages: None,
            }),
            FileType::Other => Ok(MediaType::Other {
                format: extension,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::io::Write;
    
    #[tokio::test]
    async fn test_media_index_init() {
        let temp_dir = TempDir::new().unwrap();
        let media_index = MediaIndex::new(temp_dir.path()).await.unwrap();
        
        // 初始化用户索引
        media_index.init_user_index("test_user").await.unwrap();
        media_index.switch_user("test_user").await.unwrap();
        
        // 验证用户媒体目录已创建
        let user_media_dir = temp_dir.path().join("users").join("test_user").join("media");
        assert!(user_media_dir.exists());
        assert!(user_media_dir.join("images").exists());
        assert!(user_media_dir.join("videos").exists());
        assert!(user_media_dir.join("audios").exists());
    }
    
    #[tokio::test]
    async fn test_file_operations() {
        let temp_dir = TempDir::new().unwrap();
        let media_index = MediaIndex::new(temp_dir.path()).await.unwrap();
        
        media_index.init_user_index("test_user").await.unwrap();
        media_index.switch_user("test_user").await.unwrap();
        
        // 创建测试文件
        let test_file_path = temp_dir.path().join("test.txt");
        let mut file = std::fs::File::create(&test_file_path).unwrap();
        file.write_all(b"This is a test file").unwrap();
        
        // 添加文件到索引
        let file_record = media_index.add_file(&test_file_path, None).await.unwrap();
        
        assert_eq!(file_record.filename, "test.txt");
        assert_eq!(file_record.size, 19);
        assert_eq!(file_record.file_type, FileType::Document);
        
        // 获取文件记录
        let retrieved_record = media_index.get_file(&file_record.file_id).await.unwrap().unwrap();
        assert_eq!(retrieved_record.file_id, file_record.file_id);
        
        // 获取文件路径
        let file_path = media_index.get_file_path(&file_record.file_id).await.unwrap().unwrap();
        assert!(file_path.exists());
        
        // 查询文件
        let query = FileQuery {
            file_type: Some(FileType::Document),
            ..Default::default()
        };
        
        let results = media_index.query_files(&query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_id, file_record.file_id);
        
        // 删除文件
        media_index.delete_file(&file_record.file_id).await.unwrap();
        
        // 验证文件已删除
        let deleted_record = media_index.get_file(&file_record.file_id).await.unwrap();
        assert!(deleted_record.is_none());
    }
    
    #[tokio::test]
    async fn test_media_stats() {
        let temp_dir = TempDir::new().unwrap();
        let media_index = MediaIndex::new(temp_dir.path()).await.unwrap();
        
        media_index.init_user_index("test_user").await.unwrap();
        media_index.switch_user("test_user").await.unwrap();
        
        // 创建不同类型的测试文件
        let test_files: Vec<(&str, &[u8], FileType)> = vec![
            ("test.txt", b"document content", FileType::Document),
            ("image.jpg", b"image content", FileType::Image),
            ("video.mp4", b"video content", FileType::Video),
        ];
        
        for (filename, content, _) in test_files {
            let test_file_path = temp_dir.path().join(filename);
            let mut file = std::fs::File::create(&test_file_path).unwrap();
            file.write_all(content).unwrap();
            
            media_index.add_file(&test_file_path, None).await.unwrap();
        }
        
        // 获取统计信息
        let stats = media_index.get_stats().await.unwrap();
        
        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.document_count, 1);
        assert_eq!(stats.image_count, 1);
        assert_eq!(stats.video_count, 1);
        assert!(stats.total_size > 0);
    }
} 