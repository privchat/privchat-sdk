//! HTTP 客户端模块 - 用于文件上传/下载
//! 
//! 本模块提供文件上传和下载功能，使用 reqwest 作为底层 HTTP 客户端。
//! 支持进度回调、错误处理和重试机制。

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use reqwest::{Client, multipart};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tracing::{error, info};

use crate::error::{PrivchatSDKError, Result};
use crate::sdk::HttpClientConfig;

/// HTTP 文件上传响应
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FileUploadResponse {
    pub file_id: String,
    pub file_url: String,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    pub file_size: u64,
    #[serde(default)]
    pub original_size: Option<u64>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    pub mime_type: String,
    /// 存储源 ID（0=本地，1=S3 等），写入消息 content 便于未来多存储源
    #[serde(default)]
    pub storage_source_id: Option<u32>,
}

/// 获取文件 URL 的响应（GET /api/app/files/{file_id}/url?user_id=xxx）
#[derive(Debug, Clone, serde::Deserialize)]
pub struct FileUrlResponse {
    pub file_url: String,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub file_size: Option<u64>,
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 存储源 ID（0=本地，1=S3 等），便于客户端按源区分
    #[serde(default)]
    pub storage_source_id: Option<u32>,
}

/// HTTP 客户端（用于文件上传/下载）
#[allow(dead_code)]
pub struct FileHttpClient {
    client: Client,
    base_url: Option<String>,
}

impl FileHttpClient {
    /// 创建新的 HTTP 客户端
    pub fn new(config: &HttpClientConfig, base_url: Option<String>) -> Result<Self> {
        let mut builder = Client::builder();
        
        if let Some(timeout) = config.connect_timeout_secs {
            builder = builder.connect_timeout(Duration::from_secs(timeout));
        }
        
        if let Some(timeout) = config.request_timeout_secs {
            builder = builder.timeout(Duration::from_secs(timeout));
        }
        
        let client = builder
            .build()
            .map_err(|e| PrivchatSDKError::Other(format!("创建 HTTP 客户端失败: {}", e)))?;
        
        info!("✅ HTTP 客户端已创建 (base_url: {:?})", base_url);
        
        Ok(Self { client, base_url })
    }
    
    /// 上传文件（带进度回调）
    /// 
    /// # 参数
    /// 
    /// * `upload_url` - 上传 URL（从 RPC 响应获取）
    /// * `upload_token` - 上传 token（从 RPC 响应获取）
    /// * `file_path` - 要上传的文件路径
    /// * `progress_callback` - 进度回调（可选）
    pub async fn upload_file(
        &self,
        upload_url: &str,
        upload_token: &str,
        file_path: &Path,
        progress_callback: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<FileUploadResponse> {
        // 1. 读取文件元数据
        let file_metadata = tokio::fs::metadata(file_path).await
            .map_err(|e| PrivchatSDKError::Other(format!("读取文件元数据失败: {}", e)))?;
        let file_size = file_metadata.len();
        
        info!("📤 开始上传文件: {} ({} bytes)", file_path.display(), file_size);
        
        // 2. 获取文件名
        let filename = file_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        
        // 3. 读取文件内容
        let file_data = tokio::fs::read(file_path).await
            .map_err(|e| PrivchatSDKError::Other(format!("读取文件失败: {}", e)))?;
        
        // 4. 检测 MIME 类型（简单实现，可以后续扩展）
        let mime_type = detect_mime_type(file_path);
        
        // 5. 创建 multipart form
        let part = multipart::Part::bytes(file_data)
            .file_name(filename.clone())
            .mime_str(&mime_type)
            .map_err(|e| PrivchatSDKError::Other(format!("创建 multipart part 失败: {}", e)))?;
        
        let form = multipart::Form::new()
            .part("file", part);
        
        // 6. 报告进度（开始上传）
        if let Some(ref callback) = progress_callback {
            callback(0, Some(file_size));
        }
        
        // 7. 发送请求
        let response = self.client
            .post(upload_url)
            .header("X-Upload-Token", upload_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("上传文件失败: {}", e)))?;
        
        // 8. 报告进度（上传完成）
        if let Some(ref callback) = progress_callback {
            callback(file_size, Some(file_size));
        }
        
        // 9. 检查响应状态
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "无法读取错误信息".to_string());
            error!("❌ 上传失败，HTTP 状态码: {}, 错误: {}", status, error_text);
            return Err(PrivchatSDKError::Transport(format!(
                "上传失败，HTTP 状态码: {} ({})", status, error_text
            )));
        }
        
        // 10. 解析响应
        let result: FileUploadResponse = response.json().await
            .map_err(|e| PrivchatSDKError::Serialization(format!("解析上传响应失败: {}", e)))?;
        
        info!("✅ 文件上传成功: file_id={}, file_url={}", result.file_id, result.file_url);
        
        Ok(result)
    }
    
    /// 从内存上传文件（用于 send_attachment_bytes）
    pub async fn upload_file_bytes(
        &self,
        upload_url: &str,
        upload_token: &str,
        filename: String,
        mime_type: String,
        file_data: Vec<u8>,
        progress_callback: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<FileUploadResponse> {
        let file_size = file_data.len() as u64;
        
        info!("📤 开始上传文件（内存）: {} ({} bytes)", filename, file_size);
        
        // 1. 创建 multipart form
        let part = multipart::Part::bytes(file_data)
            .file_name(filename.clone())
            .mime_str(&mime_type)
            .map_err(|e| PrivchatSDKError::Other(format!("创建 multipart part 失败: {}", e)))?;
        
        let form = multipart::Form::new()
            .part("file", part);
        
        // 2. 报告进度（开始上传）
        if let Some(ref callback) = progress_callback {
            callback(0, Some(file_size));
        }
        
        // 3. 发送请求
        let response = self.client
            .post(upload_url)
            .header("X-Upload-Token", upload_token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("上传文件失败: {}", e)))?;
        
        // 4. 报告进度（上传完成）
        if let Some(ref callback) = progress_callback {
            callback(file_size, Some(file_size));
        }
        
        // 5. 检查响应状态
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "无法读取错误信息".to_string());
            error!("❌ 上传失败，HTTP 状态码: {}, 错误: {}", status, error_text);
            return Err(PrivchatSDKError::Transport(format!(
                "上传失败，HTTP 状态码: {} ({})", status, error_text
            )));
        }
        
        // 6. 解析响应
        let result: FileUploadResponse = response.json().await
            .map_err(|e| PrivchatSDKError::Serialization(format!("解析上传响应失败: {}", e)))?;
        
        info!("✅ 文件上传成功: file_id={}, file_url={}", result.file_id, result.file_url);
        
        Ok(result)
    }
    
    /// 获取文件访问 URL（GET /api/app/files/{file_id}/url?user_id=xxx）
    /// 
    /// * `base_url` - 文件 API 基础 URL（如 http://localhost:9083）
    /// * `file_id` - 文件 ID
    /// * `user_id` - 当前用户 ID（用于鉴权）
    pub async fn get_file_url(
        &self,
        base_url: &str,
        file_id: u64,
        user_id: u64,
    ) -> Result<FileUrlResponse> {
        let base = base_url.trim_end_matches('/');
        let url = format!("{}/api/app/files/{}/url?user_id={}", base, file_id, user_id);
        info!("🔗 获取文件 URL: file_id={}, user_id={}", file_id, user_id);
        let response = self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("获取文件 URL 失败: {}", e)))?;
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "无法读取错误信息".to_string());
            error!("❌ 获取文件 URL 失败，HTTP 状态码: {}, 错误: {}", status, error_text);
            return Err(PrivchatSDKError::Transport(format!(
                "获取文件 URL 失败，HTTP 状态码: {} ({})", status, error_text
            )));
        }
        let result: FileUrlResponse = response.json().await
            .map_err(|e| PrivchatSDKError::Serialization(format!("解析文件 URL 响应失败: {}", e)))?;
        Ok(result)
    }

    /// 下载文件（带进度回调）
    /// 
    /// # 参数
    /// 
    /// * `file_url` - 文件下载 URL
    /// * `output_path` - 输出文件路径
    /// * `progress_callback` - 进度回调（可选）
    pub async fn download_file(
        &self,
        file_url: &str,
        output_path: &Path,
        progress_callback: Option<Arc<dyn Fn(u64, Option<u64>) + Send + Sync>>,
    ) -> Result<()> {
        info!("📥 开始下载文件: {} -> {}", file_url, output_path.display());
        
        // 1. 发送请求
        let response = self.client
            .get(file_url)
            .send()
            .await
            .map_err(|e| PrivchatSDKError::Transport(format!("下载文件失败: {}", e)))?;
        
        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_else(|_| "无法读取错误信息".to_string());
            error!("❌ 下载失败，HTTP 状态码: {}, 错误: {}", status, error_text);
            return Err(PrivchatSDKError::Transport(format!(
                "下载失败，HTTP 状态码: {} ({})", status, error_text
            )));
        }
        
        let total_size = response.content_length();
        
        // 2. 创建输出文件
        let mut file = tokio::fs::File::create(output_path).await
            .map_err(|e| PrivchatSDKError::Other(format!("创建输出文件失败: {}", e)))?;
        
        // 3. 流式下载（支持进度回调）
        let mut stream = response.bytes_stream();
        let mut downloaded = 0u64;
        
        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result
                .map_err(|e| PrivchatSDKError::Transport(format!("读取数据块失败: {}", e)))?;
            
            file.write_all(&chunk).await
                .map_err(|e| PrivchatSDKError::Other(format!("写入文件失败: {}", e)))?;
            
            downloaded += chunk.len() as u64;
            
            // 报告进度
            if let Some(ref callback) = progress_callback {
                callback(downloaded, total_size);
            }
        }
        
        // 4. 同步文件到磁盘
        file.sync_all().await
            .map_err(|e| PrivchatSDKError::Other(format!("同步文件失败: {}", e)))?;
        
        info!("✅ 文件下载成功: {} ({} bytes)", output_path.display(), downloaded);
        
        Ok(())
    }
}

/// 检测文件的 MIME 类型（简单实现）
fn detect_mime_type(path: &Path) -> String {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "png" => "image/png",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "mp4" => "video/mp4",
            "mov" => "video/quicktime",
            "mp3" => "audio/mpeg",
            "wav" => "audio/wav",
            "pdf" => "application/pdf",
            "zip" => "application/zip",
            "txt" => "text/plain",
            _ => "application/octet-stream",
        }
    } else {
        "application/octet-stream"
    }.to_string()
}
