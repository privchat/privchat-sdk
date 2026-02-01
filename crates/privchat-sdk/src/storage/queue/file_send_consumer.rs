//! 文件发送消费者 - 步骤 5～8：压缩（可选）→ 请求 token → 上传 → 发消息，发送成功统一回调。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::client::PrivchatClient;
use crate::error::{PrivchatSDKError, Result};
use crate::rpc_client::RpcClientExt;
use crate::events::{EventManager, SDKEvent, SendStatusState};
use crate::storage::entities::MessageStatus;
use crate::storage::media_preprocess::{
    message_files_dir, resize_image_to_max_edge_sync, tmp_files_dir,
    yyyymm_from_timestamp_ms, yyyymmdd_from_timestamp_ms, MediaMeta,
};
use crate::storage::queue::file_send_queue::FileSendQueue;
use crate::storage::queue::file_send_task::FileSendTask;
use crate::storage::StorageManager;
use privchat_protocol::rpc::file::upload::FileRequestUploadTokenRequest;

/// 文件发送消费者配置
#[derive(Debug, Clone)]
pub struct FileConsumerConfig {
    /// 工作协程数量（2～3）
    pub worker_count: usize,
    /// 无任务时轮询间隔（毫秒）
    pub poll_interval_ms: u64,
    /// 发送图片时最长边上限（如 720、1080）；None 表示发原图
    pub image_send_max_edge: Option<u32>,
}

impl Default for FileConsumerConfig {
    fn default() -> Self {
        Self {
            worker_count: 3,
            poll_interval_ms: 200,
            image_send_max_edge: Some(1080),
        }
    }
}

/// 文件发送消费者运行器
pub struct FileConsumerRunner {
    config: FileConsumerConfig,
    file_queue: Arc<FileSendQueue>,
    data_dir: PathBuf,
    storage: Arc<StorageManager>,
    client: Arc<RwLock<Option<PrivchatClient>>>,
    http_client: Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
    event_manager: Arc<EventManager>,
    shutdown: Arc<tokio::sync::Notify>,
    is_running: Arc<RwLock<bool>>,
}

impl FileConsumerRunner {
    pub fn new(
        config: FileConsumerConfig,
        file_queue: Arc<FileSendQueue>,
        data_dir: PathBuf,
        storage: Arc<StorageManager>,
        client: Arc<RwLock<Option<PrivchatClient>>>,
        http_client: Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
        event_manager: Arc<EventManager>,
    ) -> Self {
        Self {
            config,
            file_queue,
            data_dir,
            storage,
            client,
            http_client,
            event_manager,
            shutdown: Arc::new(tokio::sync::Notify::new()),
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    pub async fn start(&self) -> Result<()> {
        {
            let mut r = self.is_running.write().await;
            if *r {
                return Err(PrivchatSDKError::Other("FileConsumer already running".to_string()));
            }
            *r = true;
        }
        info!("Starting FileConsumer with {} workers", self.config.worker_count);
        for worker_id in 0..self.config.worker_count {
            self.spawn_worker(worker_id);
        }
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        {
            let mut r = self.is_running.write().await;
            *r = false;
        }
        self.shutdown.notify_waiters();
        sleep(std::time::Duration::from_millis(300)).await;
        info!("FileConsumer stopped");
        Ok(())
    }

    pub async fn is_running(&self) -> bool {
        *self.is_running.read().await
    }

    fn spawn_worker(&self, worker_id: usize) {
        let queue = self.file_queue.clone();
        let data_dir = self.data_dir.clone();
        let storage = self.storage.clone();
        let client = self.client.clone();
        let http_client = self.http_client.clone();
        let event_manager = self.event_manager.clone();
        let shutdown = self.shutdown.clone();
        let is_running = self.is_running.clone();
        let config = self.config.clone();
        let poll_ms = config.poll_interval_ms;

        tokio::spawn(async move {
            info!("FileConsumer worker {} started", worker_id);
            loop {
                tokio::select! {
                    _ = shutdown.notified() => break,
                    _ = sleep(std::time::Duration::from_millis(poll_ms)) => {
                        if !*is_running.read().await { break; }
                        if let Ok(Some(task)) = queue.pop().await {
                            if let Err(e) = Self::process_task(
                                worker_id,
                                &task,
                                &data_dir,
                                &*storage,
                                &client,
                                &http_client,
                                &*event_manager,
                                &config,
                            ).await {
                                error!("FileConsumer worker {} process task {} error: {}", worker_id, task.id, e);
                            }
                        }
                    }
                }
            }
            info!("FileConsumer worker {} stopped", worker_id);
        });
    }

    async fn process_task(
        worker_id: usize,
        task: &FileSendTask,
        data_dir: &std::path::Path,
        storage: &StorageManager,
        client: &Arc<RwLock<Option<PrivchatClient>>>,
        http_client: &Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
        event_manager: &EventManager,
        config: &FileConsumerConfig,
    ) -> Result<()> {
        debug!("FileConsumer worker {} processing task {}", worker_id, task.id);

        storage.update_message_status(task.id, MessageStatus::Sending as i32).await?;
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        event_manager.emit(SDKEvent::SendStatusUpdate {
            channel_id: task.channel_id,
            id: task.id as u64,
            state: SendStatusState::Sending,
            attempts: 0,
            error: None,
            timestamp: ts,
        }).await;

        let result = Self::do_upload_and_send(task, data_dir, storage, client, http_client, config).await;
        let server_message_id = match result {
            Err(e) => {
                warn!("FileConsumer worker {} task {} failed: {}", worker_id, task.id, e);
                let _ = storage.update_message_status(task.id, MessageStatus::Failed as i32).await;
                let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                let _ = event_manager.emit(SDKEvent::SendStatusUpdate {
                    channel_id: task.channel_id,
                    id: task.id as u64,
                    state: SendStatusState::Failed,
                    attempts: 0,
                    error: Some(e.to_string()),
                    timestamp: ts,
                }).await;
                return Err(e);
            }
            Ok(sid) => sid,
        };
        storage.update_message_status(task.id, MessageStatus::Sent as i32).await?;
        storage.update_message_server_id(task.id, server_message_id).await?;
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        event_manager.emit(SDKEvent::SendStatusUpdate {
            channel_id: task.channel_id,
            id: task.id as u64,
            state: SendStatusState::Sent,
            attempts: 0,
            error: None,
            timestamp: ts,
        }).await;
        info!("FileConsumer worker {} sent message {} -> server_message_id={}", worker_id, task.id, server_message_id);
        Ok(())
    }

    /// 步骤 5（图片压缩）～8：按配置最长边缩放图片（可选）→ 请求 token → 上传 → 发消息；成功返回 server_message_id，失败返回 Err
    async fn do_upload_and_send(
        task: &FileSendTask,
        data_dir: &std::path::Path,
        _storage: &StorageManager,
        client: &Arc<RwLock<Option<PrivchatClient>>>,
        http_client: &Arc<RwLock<Option<Arc<crate::http_client::FileHttpClient>>>>,
        config: &FileConsumerConfig,
    ) -> Result<u64> {
        let yyyymm = yyyymm_from_timestamp_ms(task.timestamp_ms);
        let yyyymmdd = yyyymmdd_from_timestamp_ms(task.timestamp_ms);
        let files_dir = message_files_dir(data_dir, &task.uid, &yyyymm, task.id);
        let tmp_dir = tmp_files_dir(data_dir, &task.uid, &yyyymmdd);
        let meta_path = files_dir.join("meta.json");
        let meta_str = tokio::fs::read_to_string(&meta_path).await
            .map_err(|e| PrivchatSDKError::IO(format!("读取 meta.json 失败: {}", e)))?;
        let meta: MediaMeta = MediaMeta::from_json_str(&meta_str)?;
        let original_filename = meta.source.original_filename.clone();
        let mime = meta.source.mime.clone();
        let body_path = files_dir.join(&original_filename);

        let user_id = task.from_uid;

        // 步骤 5（图片）：Media Resize Constraint — 仅当原图最长边 > constraint 时等比缩放；不放大原则
        let (body_upload_path, body_size, body_mime) = if task.message_type == "image"
            && config.image_send_max_edge.is_some()
        {
            let max_edge = config.image_send_max_edge.unwrap();
            let (w, h) = (
                meta.source.width.unwrap_or(0),
                meta.source.height.unwrap_or(0),
            );
            if w > 0 && h > 0 && w.max(h) > max_edge {
                let body_tmp = tmp_dir.join(format!("{}_body.jpg", task.id));
                let (_nw, _nh, file_size) = tokio::task::spawn_blocking({
                    let src = body_path.clone();
                    let out = body_tmp.clone();
                    move || resize_image_to_max_edge_sync(&src, &out, max_edge, 88)
                })
                .await
                .map_err(|e| PrivchatSDKError::Other(format!("spawn_blocking: {}", e)))??;
                (
                    body_tmp,
                    file_size as i64,
                    "image/jpeg".to_string(),
                )
            } else {
                let size = tokio::fs::metadata(&body_path).await.map(|m| m.len()).unwrap_or(0) as i64;
                (body_path.clone(), size, mime.clone())
            }
        } else {
            let size = tokio::fs::metadata(&body_path).await.map(|m| m.len()).unwrap_or(0) as i64;
            (body_path.clone(), size, mime.clone())
        };

        let (thumbnail_file_id, file_id, storage_source_id) = if task.needs_thumbnail() {
            // 视频无回调时缩略图已在发送前上传，直接使用预上传的 thumbnail_file_id
            let thumbnail_file_id = if let Some(ref pre_id) = task.pre_uploaded_thumbnail_file_id {
                pre_id.clone()
            } else {
                let thumb_mime = meta.thumbnail.as_ref().and_then(|t| t.mime.as_deref()).unwrap_or("image/jpeg");
                let thumb_ext = if thumb_mime.contains("png") { "png" } else { "jpg" };
                let thumb_path = tmp_dir.join(format!("{}_thumb.{}", task.id, thumb_ext));
                if !thumb_path.exists() {
                    return Err(PrivchatSDKError::IO(format!("缩略图不存在: {}", thumb_path.display())));
                }
                let thumb_size = tokio::fs::metadata(&thumb_path).await.map(|m| m.len()).unwrap_or(0) as i64;

                let thumb_token_req = FileRequestUploadTokenRequest {
                    user_id,
                    filename: Some(format!("{}_thumb.{}", task.id, thumb_ext)),
                    file_size: thumb_size,
                    mime_type: thumb_mime.to_string(),
                    file_type: "image".to_string(),
                    business_type: "message".to_string(),
                };
                let thumb_token = {
                    let mut guard = client.write().await;
                    let c = guard.as_mut().ok_or(PrivchatSDKError::NotConnected)?;
                    c.file_request_upload_token(thumb_token_req).await?
                };
                let http_guard = http_client.read().await;
                let http = http_guard.as_ref().ok_or_else(|| PrivchatSDKError::Other("HTTP 客户端未初始化".to_string()))?;
                let thumb_resp = http.upload_file(&thumb_token.upload_url, &thumb_token.token, &thumb_path, None).await?;
                thumb_resp.file_id
            };

            let file_type = if task.message_type == "image" { "image" } else if task.message_type == "video" { "video" } else { "file" };
            let body_token_req = FileRequestUploadTokenRequest {
                user_id,
                filename: Some(original_filename.clone()),
                file_size: body_size,
                mime_type: body_mime.clone(),
                file_type: file_type.to_string(),
                business_type: "message".to_string(),
            };
            let body_token = {
                let mut guard = client.write().await;
                let c = guard.as_mut().ok_or(PrivchatSDKError::NotConnected)?;
                c.file_request_upload_token(body_token_req).await?
            };
            let http_guard = http_client.read().await;
            let http = http_guard.as_ref().ok_or_else(|| PrivchatSDKError::Other("HTTP 客户端未初始化".to_string()))?;
            let body_resp = http.upload_file(&body_token.upload_url, &body_token.token, &body_upload_path, None).await?;
            let sid = body_resp.storage_source_id.unwrap_or(0);
            (Some(thumbnail_file_id), body_resp.file_id, sid)
        } else {
            let body_token_req = FileRequestUploadTokenRequest {
                user_id,
                filename: Some(original_filename.clone()),
                file_size: body_size,
                mime_type: body_mime.clone(),
                file_type: "file".to_string(),
                business_type: "message".to_string(),
            };
            let body_token = {
                let mut guard = client.write().await;
                let c = guard.as_mut().ok_or(PrivchatSDKError::NotConnected)?;
                c.file_request_upload_token(body_token_req).await?
            };
            let http_guard = http_client.read().await;
            let http = http_guard.as_ref().ok_or_else(|| PrivchatSDKError::Other("HTTP 客户端未初始化".to_string()))?;
            let body_resp = http.upload_file(&body_token.upload_url, &body_token.token, &body_upload_path, None).await?;
            let sid = body_resp.storage_source_id.unwrap_or(0);
            (None, body_resp.file_id, sid)
        };

        // storage_source_id 来自上传响应，便于接收端与未来多存储源；缺省为 0（本地）
        let content = if let Some(thumb_id) = thumbnail_file_id {
            serde_json::json!({
                "file_id": file_id,
                "thumbnail_file_id": thumb_id,
                "mime_type": body_mime,
                "filename": original_filename,
                "storage_source_id": storage_source_id,
            })
        } else {
            serde_json::json!({
                "file_id": file_id,
                "mime_type": body_mime,
                "filename": original_filename,
                "storage_source_id": storage_source_id,
            })
        };
        let content_str = content.to_string();

        let (_, server_message_id) = {
            let mut guard = client.write().await;
            let c = guard.as_mut().ok_or(PrivchatSDKError::NotConnected)?;
            c.send_message_internal(
                task.channel_id,
                &content_str,
                &task.message_type,
                None,
                task.local_message_id,
            ).await?
        };
        Ok(server_message_id)
    }
}
