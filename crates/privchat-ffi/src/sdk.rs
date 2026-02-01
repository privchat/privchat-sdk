//! Main SDK interface for FFI

use std::sync::Arc;
use tokio::sync::{RwLock, Mutex};
use tracing::{debug, error, info, warn};

use crate::{
    config::PrivchatConfig,
    error::PrivchatError,
    events::{ConnectionState, SDKEvent, PrivchatDelegate, SendObserver, SendUpdate, SendState, TimelineObserver, TimelineDiffKind, ChannelListObserver, ChannelListEntry, LatestChannelEvent, MessageEntry, FriendEntry, UserEntry, FriendPendingEntry, GroupEntry, GroupMemberEntry, BlacklistEntry, SyncStateEntry, PresenceEntry, AuthResult, GroupCreateResult, GetOrCreateDirectChannelResult, TypingObserver, TypingIndicatorEvent, ReceiptsObserver, ReadReceiptEvent, UnreadStats, LastReadPosition, GroupQRCodeJoinResult, SyncObserver, SearchPage, SearchHit, NotificationMode, ChannelTags, DeviceSummary, ReactionChip, SeenByEntry, MediaProcessOp, VideoProcessHook, SendMessageOptions, AttachmentInfo, AttachmentSendResult, ProgressObserver},
    helpers::get_runtime,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// Main SDK handle for FFI consumers
#[derive(uniffi::Object)]
pub struct PrivchatSDK {
    inner: Arc<RwLock<Option<Arc<privchat_sdk::PrivchatSDK>>>>,
    config: PrivchatConfig,
    /// Event queue for polling (FIFO)
    event_queue: Arc<Mutex<Vec<SDKEvent>>>,
    /// Event listener task handle
    event_listener_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Network status listener task handle
    network_listener_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    /// Callback delegate for real-time event notifications
    delegate: Arc<RwLock<Option<Box<dyn PrivchatDelegate>>>>,
    /// Send observers for tracking message send status
    send_observers: Arc<Mutex<HashMap<u64, Arc<dyn SendObserver>>>>,
    /// Counter for generating observer tokens
    send_observer_counter: Arc<AtomicU64>,
    /// Timeline observers for tracking message timeline updates
    /// token -> (channel_id, observer)
    timeline_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn TimelineObserver>)>>>,
    /// Counter for generating timeline observer tokens
    timeline_observer_counter: Arc<AtomicU64>,
    /// Channel list observers for tracking channel list updates
    channel_list_observers: Arc<Mutex<HashMap<u64, Arc<dyn ChannelListObserver>>>>,
    /// Counter for generating channel list observer tokens
    channel_list_observer_counter: Arc<AtomicU64>,
    /// Typing observers for tracking typing indicator updates
    /// token -> (channel_id, observer)
    typing_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn TypingObserver>)>>>,
    /// Counter for generating typing observer tokens
    typing_observer_counter: Arc<AtomicU64>,
    /// Receipts observers for tracking read receipt updates
    /// token -> (channel_id, observer)
    receipts_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn ReceiptsObserver>)>>>,
    /// Counter for generating receipts observer tokens
    receipts_observer_counter: Arc<AtomicU64>,
    /// Sync observer for tracking sync status updates（预留，暂未接入）
    #[allow(dead_code)]
    sync_observer: Arc<Mutex<Option<Arc<dyn SyncObserver>>>>,
}

/// 所有 FFI 入口调用时打 debug 日志，便于排查是否进入 SDK（仅 RUST_LOG=debug 时输出，避免污染 TUI）
macro_rules! sdk_ffi_log {
    ($name:expr) => {
        debug!("privchat sdk->{}()", $name);
    };
}

#[uniffi::export]
impl PrivchatSDK {
    /// Initialize the SDK with configuration
    #[uniffi::constructor]
    pub fn new(config: PrivchatConfig) -> Result<Self, PrivchatError> {
        sdk_ffi_log!("new");
        info!("Initializing Privchat SDK");
        
        Ok(Self {
            inner: Arc::new(RwLock::new(None)),
            config,
            event_queue: Arc::new(Mutex::new(Vec::with_capacity(100))),
            event_listener_handle: Arc::new(RwLock::new(None)),
            network_listener_handle: Arc::new(RwLock::new(None)),
            delegate: Arc::new(RwLock::new(None)),
            send_observers: Arc::new(Mutex::new(HashMap::new())),
            send_observer_counter: Arc::new(AtomicU64::new(0)),
            timeline_observers: Arc::new(Mutex::new(HashMap::new())),
            timeline_observer_counter: Arc::new(AtomicU64::new(0)),
            channel_list_observers: Arc::new(Mutex::new(HashMap::new())),
            channel_list_observer_counter: Arc::new(AtomicU64::new(0)),
            typing_observers: Arc::new(Mutex::new(HashMap::new())),
            typing_observer_counter: Arc::new(AtomicU64::new(0)),
            receipts_observers: Arc::new(Mutex::new(HashMap::new())),
            receipts_observer_counter: Arc::new(AtomicU64::new(0)),
            sync_observer: Arc::new(Mutex::new(None)),
        })
    }
    
    /// Connect to server (establish network connection)
    /// Must be called before register/login/authenticate
    ///
    /// Runs inside our static Tokio runtime via block_on so that SDK init (tokio::fs etc.)
    /// and event listener spawn always execute in a runtime context. Prevents "no reactor
    /// running" when called from UniFFI/Kotlin threads.
    pub fn connect(self: Arc<Self>) -> Result<(), PrivchatError> {
        sdk_ffi_log!("connect");
        let this = self.clone();
        get_runtime().block_on(async move {
            this.connect_async().await
        })
    }

    async fn connect_async(self: Arc<Self>) -> Result<(), PrivchatError> {
        info!("Connecting to server");
        
        // 1. 确保 SDK 已初始化
        {
            let guard = self.inner.read().await;
            if guard.is_none() {
                drop(guard);
                let config = self.config.clone();
                Self::do_initialize(self.inner.clone(), config).await?;
                info!("SDK initialized successfully");
            }
        }
        
        // 2. 启动事件监听器（如果还没有启动）
        {
            let guard = self.inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let mut handle_guard = self.event_listener_handle.write().await;
                if handle_guard.is_none() {
                    info!("Starting event listener");
                    let listener_handle = Self::start_event_listener(
                        sdk.clone(), 
                        self.event_queue.clone(),
                        self.delegate.clone(),
                        self.send_observers.clone(),
                        self.timeline_observers.clone(),
                        self.channel_list_observers.clone(),
                        self.typing_observers.clone(),
                        self.receipts_observers.clone(),
                    );
                    *handle_guard = Some(listener_handle);
                }
                drop(handle_guard);
                let mut net_guard = self.network_listener_handle.write().await;
                if net_guard.is_none() {
                    info!("Starting network status listener");
                    let net_handle = Self::start_network_status_listener(
                        self.inner.clone(),
                        self.delegate.clone(),
                    );
                    *net_guard = Some(net_handle);
                }
                drop(net_guard);
                
                // 3. 调用真实的连接逻辑
                sdk.connect().await?;
                info!("✅ Connected to server successfully");
                // 连接成功后启动「transport 断开 → SDK 状态」桥监听，被动断开会同步到 SDK 并发出 ConnectionStateChanged
                sdk.clone().start_transport_disconnect_listener().await;
                // 连接成功后后台执行启动同步（Friend → Group → Channel → UserSettings，由 Cursor 决定全量/增量）
                privchat_sdk::PrivchatSDK::run_bootstrap_sync_in_background(sdk.clone());
                Ok(())
            } else {
                Err(PrivchatError::NotInitialized)
            }
        }
    }
    
    /// Disconnect from server.
    /// Runs on our Tokio runtime via block_on.
    pub fn disconnect(self: Arc<Self>) -> Result<(), PrivchatError> {
        sdk_ffi_log!("disconnect");
        let this = self.clone();
        get_runtime().block_on(async move {
            let guard = this.inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.disconnect().await?;
                info!("✅ Disconnected from server successfully");
                Ok(())
            } else {
                debug!("SDK not initialized, nothing to disconnect");
                Ok(())
            }
        })
    }
    
    /// Send a text message
    /// 
    /// Returns the message ID on success.
    /// 
    /// UniFFI 0.31: Direct async/await support - no TaskHandle needed
    /// Send a message
    /// 
    /// **同步 API**：立即返回入队结果（local_message_id），实际发送在后台队列异步进行。
    /// 发送状态更新通过 `SendObserver` 通知。
    /// 
    /// Returns the `local_message_id` (transport layer identifier) for tracking send status.
    /// The actual network transmission happens asynchronously in the background queue.
    pub fn send_message(
        self: Arc<Self>,
        content: String,
        channel_id: u64,
        _channel_type: i32,
    ) -> Result<u64, PrivchatError> {
        sdk_ffi_log!("send_message");
        // 使用默认选项发送消息
        self.send_message_with_options(content, channel_id, SendMessageOptions::default())
    }
    
    /// Send message with options
    /// 
    /// **类型化返回**：返回 `u64` (local_message_id) 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `content`: 消息内容
    /// - `channel_id`: 会话ID
    /// - `options`: 发送选项（回复、提及等）
    /// 
    /// # 返回
    /// - `Ok(u64)`: 返回 local_message_id（用于跟踪发送状态）
    pub fn send_message_with_options(
        self: Arc<Self>,
        content: String,
        channel_id: u64,
        options: SendMessageOptions,
    ) -> Result<u64, PrivchatError> {
        sdk_ffi_log!("send_message_with_options");
        debug!("Sending message to channel {}: {}", channel_id, content);
        
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 转换 FFI 层的 SendMessageOptions 到 SDK 层
                let extra_value = options.extra_json.as_ref().and_then(|json_str| {
                    serde_json::from_str(json_str).ok()
                });
                
                let sdk_options = privchat_sdk::events::SendMessageOptions {
                    in_reply_to_message_id: options.in_reply_to_message_id,
                    mentions: options.mentions,
                    silent: options.silent,
                    extra: extra_value,
                };
                
                let id = sdk.send_message_with_options(channel_id, &content, sdk_options).await?;
                info!("✅ Message enqueued successfully, id: {}", id);
                Ok(id)
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// Mark message as read
    /// 
    /// **同步 API**：立即返回，实际标记操作在后台异步进行。
    /// 
    /// UniFFI 0.31: Synchronous API that blocks on async operations internally.
    pub fn mark_as_read(
        self: Arc<Self>,
        channel_id: u64,
        message_id: u64,
    ) -> Result<(), PrivchatError> {
        sdk_ffi_log!("mark_as_read");
        debug!("Marking message {} as read in channel {}", message_id, channel_id);
        
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.mark_as_read(channel_id, message_id).await?;
                info!("✅ Message marked as read successfully");
                Ok(())
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    // ========== 文件上传/下载 ==========
    
    /// 从文件路径发送附件
    /// 
    /// 流程：
    /// 1. RPC: file/request_upload_token → 获取 { upload_token, upload_url }
    /// 2. HTTP: POST upload_url (multipart, header: X-Upload-Token) → 返回 { file_id, file_url }
    /// 3. 发送消息: content 包含 file_id 和 file_url
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `file_path`: 文件路径
    /// - `options`: 发送选项（回复、提及等）
    /// - `progress`: 进度回调（可选）
    /// 
    /// # 返回
    /// - `Ok(AttachmentSendResult)`: 返回包含 local_message_id 和附件信息的结果
    pub fn send_attachment_from_path(
        self: Arc<Self>,
        channel_id: u64,
        file_path: String,
        options: SendMessageOptions,
        progress: Option<Box<dyn ProgressObserver>>,
    ) -> Result<AttachmentSendResult, PrivchatError> {
        sdk_ffi_log!("send_attachment_from_path");
        debug!("Sending attachment from path: channel_id={}, file_path={}", channel_id, file_path);
        
        let inner = self.inner.clone();
        let path = std::path::PathBuf::from(file_path);
        
        // 转换 ProgressObserver 到闭包
        let progress_callback: Option<std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>> = 
            progress.map(|obs| {
                let obs_arc = std::sync::Arc::new(obs);
                std::sync::Arc::new(move |current: u64, total: Option<u64>| {
                    obs_arc.on_progress(current, total);
                }) as std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>
            });
        
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 转换 FFI 层的 SendMessageOptions 到 SDK 层
                let extra_value = options.extra_json.as_ref().and_then(|json_str| {
                    serde_json::from_str(json_str).ok()
                });
                
                let sdk_options = privchat_sdk::events::SendMessageOptions {
                    in_reply_to_message_id: options.in_reply_to_message_id,
                    mentions: options.mentions,
                    silent: options.silent,
                    extra: extra_value,
                };
                
                // 调用 SDK 层方法
                let (id, sdk_attachment) = sdk.send_attachment_from_path(
                    channel_id,
                    &path,
                    sdk_options,
                    progress_callback,
                ).await?;
                
                // 转换 SDK 层的 AttachmentInfo 到 FFI 层
                let ffi_attachment = AttachmentInfo {
                    url: sdk_attachment.url,
                    mime_type: sdk_attachment.mime_type,
                    size: sdk_attachment.size,
                    thumbnail_url: sdk_attachment.thumbnail_url,
                    filename: sdk_attachment.filename,
                    file_id: sdk_attachment.file_id,
                    width: sdk_attachment.width,
                    height: sdk_attachment.height,
                    duration: sdk_attachment.duration,
                };
                
                info!("✅ Attachment sent successfully, id: {}, file_id: {:?}", id, ffi_attachment.file_id);
                Ok(AttachmentSendResult {
                    id,
                    attachment: ffi_attachment,
                })
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// 从内存发送附件
    /// 
    /// 流程与 `send_attachment_from_path` 相同，但文件数据来自内存。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `filename`: 文件名
    /// - `mime_type`: MIME 类型
    /// - `data`: 文件数据（字节）
    /// - `options`: 发送选项（回复、提及等）
    /// - `progress`: 进度回调（可选）
    /// 
    /// # 返回
    /// - `Ok(AttachmentSendResult)`: 返回包含 local_message_id 和附件信息的结果
    pub fn send_attachment_bytes(
        self: Arc<Self>,
        channel_id: u64,
        filename: String,
        mime_type: String,
        data: Vec<u8>,
        options: SendMessageOptions,
        progress: Option<Box<dyn ProgressObserver>>,
    ) -> Result<AttachmentSendResult, PrivchatError> {
        debug!("Sending attachment from bytes: channel_id={}, filename={}, size={}", 
            channel_id, filename, data.len());
        
        let inner = self.inner.clone();
        
        // 转换 ProgressObserver 到闭包
        let progress_callback: Option<std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>> = 
            progress.map(|obs| {
                let obs_arc = std::sync::Arc::new(obs);
                std::sync::Arc::new(move |current: u64, total: Option<u64>| {
                    obs_arc.on_progress(current, total);
                }) as std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>
            });
        
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 转换 FFI 层的 SendMessageOptions 到 SDK 层
                let extra_value = options.extra_json.as_ref().and_then(|json_str| {
                    serde_json::from_str(json_str).ok()
                });
                
                let sdk_options = privchat_sdk::events::SendMessageOptions {
                    in_reply_to_message_id: options.in_reply_to_message_id,
                    mentions: options.mentions,
                    silent: options.silent,
                    extra: extra_value,
                };
                
                // 调用 SDK 层方法
                let (id, sdk_attachment) = sdk.send_attachment_bytes(
                    channel_id,
                    filename,
                    mime_type,
                    data,
                    sdk_options,
                    progress_callback,
                ).await?;
                
                // 转换 SDK 层的 AttachmentInfo 到 FFI 层
                let ffi_attachment = AttachmentInfo {
                    url: sdk_attachment.url,
                    mime_type: sdk_attachment.mime_type,
                    size: sdk_attachment.size,
                    thumbnail_url: sdk_attachment.thumbnail_url,
                    filename: sdk_attachment.filename,
                    file_id: sdk_attachment.file_id,
                    width: sdk_attachment.width,
                    height: sdk_attachment.height,
                    duration: sdk_attachment.duration,
                };
                
                info!("✅ Attachment sent successfully, id: {}, file_id: {:?}", id, ffi_attachment.file_id);
                Ok(AttachmentSendResult {
                    id,
                    attachment: ffi_attachment,
                })
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// 下载附件到缓存目录
    /// 
    /// 下载文件到 SDK 管理的缓存目录，并通过 MediaIndex 进行索引管理。
    /// 如果文件已存在，直接返回缓存路径。
    /// 
    /// # 参数
    /// - `file_id`: 文件ID（服务端分配的唯一标识）
    /// - `file_url`: 文件下载 URL
    /// - `progress`: 进度回调（可选）
    /// 
    /// # 返回
    /// - `Ok(String)`: 返回缓存文件路径（字符串形式）
    pub fn download_attachment_to_cache(
        self: Arc<Self>,
        file_id: String,
        file_url: String,
        progress: Option<Box<dyn ProgressObserver>>,
    ) -> Result<String, PrivchatError> {
        debug!("Downloading attachment to cache: file_id={}, file_url={}", file_id, file_url);
        
        let inner = self.inner.clone();
        
        // 转换 ProgressObserver 到闭包
        let progress_callback: Option<std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>> = 
            progress.map(|obs| {
                let obs_arc = std::sync::Arc::new(obs);
                std::sync::Arc::new(move |current: u64, total: Option<u64>| {
                    obs_arc.on_progress(current, total);
                }) as std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>
            });
        
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let cache_path = sdk.download_attachment_to_cache(
                    &file_id,
                    &file_url,
                    progress_callback,
                ).await?;
                
                let path_str = cache_path.to_string_lossy().to_string();
                info!("✅ Attachment downloaded to cache: {}", path_str);
                Ok(path_str)
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// 下载附件到指定路径
    /// 
    /// 下载文件到用户指定的路径，不经过缓存系统。
    /// 
    /// # 参数
    /// - `file_url`: 文件下载 URL
    /// - `output_path`: 输出文件路径
    /// - `progress`: 进度回调（可选）
    /// 
    /// # 返回
    /// - `Ok(())`: 下载成功
    pub fn download_attachment_to_path(
        self: Arc<Self>,
        file_url: String,
        output_path: String,
        progress: Option<Box<dyn ProgressObserver>>,
    ) -> Result<(), PrivchatError> {
        debug!("Downloading attachment to path: file_url={}, output_path={}", file_url, output_path);
        
        let inner = self.inner.clone();
        let path = std::path::PathBuf::from(output_path);
        
        // 转换 ProgressObserver 到闭包
        let progress_callback: Option<std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>> = 
            progress.map(|obs| {
                let obs_arc = std::sync::Arc::new(obs);
                std::sync::Arc::new(move |current: u64, total: Option<u64>| {
                    obs_arc.on_progress(current, total);
                }) as std::sync::Arc<dyn Fn(u64, Option<u64>) + Send + Sync>
            });
        
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.download_attachment_to_path(
                    &file_url,
                    &path,
                    progress_callback,
                ).await?;
                
                info!("✅ Attachment downloaded to path successfully");
                Ok(())
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// Get current connection state
    pub fn connection_state(&self) -> ConnectionState {
        sdk_ffi_log!("connection_state");
        // Block on async call using the runtime
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let state = sdk.get_connection_state().await;
                // Convert privchat_sdk::ConnectionStatus to FFI ConnectionState
                use privchat_sdk::connection_state::ConnectionStatus as SdkStatus;
                match state.status {
                    SdkStatus::Disconnected => ConnectionState::Disconnected,
                    SdkStatus::Connecting => ConnectionState::Connecting,
                    SdkStatus::Connected | SdkStatus::Authenticated => ConnectionState::Connected,
                    SdkStatus::Authenticating => ConnectionState::Connecting,
                    SdkStatus::Reconnecting => ConnectionState::Reconnecting,
                    SdkStatus::Failed => ConnectionState::Failed,
                }
            } else {
                ConnectionState::Disconnected
            }
        })
    }
    
    /// Get current user ID (if connected)
    pub fn current_user_id(&self) -> Option<u64> {
        sdk_ffi_log!("current_user_id");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let state = sdk.get_connection_state().await;
                // Try to parse user_id from string to u64
                state.user.as_ref().and_then(|info| {
                    info.user_id.parse::<u64>().ok()
                })
            } else {
                None
            }
        })
    }
    
    /// Shutdown the SDK gracefully
    pub fn shutdown(self: Arc<Self>) -> Result<(), PrivchatError> {
        info!("Shutting down SDK gracefully");
        
        let inner = self.inner.clone();
        
        get_runtime().block_on(async move {
            let mut guard = inner.write().await;
            if let Some(sdk) = guard.take() {
                match sdk.shutdown().await {
                    Ok(_) => {
                        info!("✅ SDK shutdown successfully");
                        Ok(())
                    }
                    Err(e) => {
                        error!("❌ SDK shutdown failed: {:?}", e);
                        Err(PrivchatError::Generic { msg: "SDK shutdown failed".to_string() })
                    }
                }
            } else {
                debug!("SDK already shutdown");
                Ok(())
            }
        })
    }
    
    /// Poll for pending events (non-blocking)
    /// Returns up to max_events events from the queue
    pub fn poll_events(&self, max_events: u32) -> Vec<SDKEvent> {
        let event_queue = self.event_queue.clone();
        get_runtime().block_on(async move {
            let mut queue: tokio::sync::MutexGuard<'_, Vec<SDKEvent>> = event_queue.lock().await;
            let count = std::cmp::min(max_events as usize, queue.len());
            queue.drain(0..count).collect()
        })
    }
    
    /// Get the number of pending events in the queue
    pub fn pending_events_count(&self) -> u32 {
        let event_queue = self.event_queue.clone();
        get_runtime().block_on(async move {
            let queue: tokio::sync::MutexGuard<'_, Vec<SDKEvent>> = event_queue.lock().await;
            queue.len() as u32
        })
    }
    
    /// Clear all pending events from the queue
    pub fn clear_events(&self) {
        let event_queue = self.event_queue.clone();
        get_runtime().block_on(async move {
            let mut queue: tokio::sync::MutexGuard<'_, Vec<SDKEvent>> = event_queue.lock().await;
            queue.clear();
            info!("Event queue cleared");
        })
    }
    
    /// Set the callback delegate for real-time event notifications
    /// 
    /// UniFFI 0.31: Type-safe callback interface
    /// The delegate will be called on a background thread whenever events occur.
    /// This is the preferred way to handle events for production applications.
    pub fn set_delegate(&self, delegate: Box<dyn PrivchatDelegate>) {
        let delegate_arc = self.delegate.clone();
        get_runtime().block_on(async move {
            let mut guard = delegate_arc.write().await;
            *guard = Some(delegate);
            info!("✅ Delegate set successfully");
        });
    }
    
    /// Remove the callback delegate
    pub fn remove_delegate(&self) {
        let delegate_arc = self.delegate.clone();
        get_runtime().block_on(async move {
            let mut guard = delegate_arc.write().await;
            *guard = None;
            info!("Delegate removed");
        });
    }

    /// 设置视频处理钩子（Contract v1.1）
    /// 
    /// 未设置时，视频消息的缩略图将使用 1x1 全透明 PNG 占位上传。
    pub fn set_video_process_hook(&self, hook: Option<Box<dyn VideoProcessHook>>) {
        let inner = self.inner.clone();
        let was_set = hook.is_some();
        get_runtime().block_on(async move {
            let sdk_hook = hook.map(|h| {
                let h: std::sync::Arc<dyn VideoProcessHook> = std::sync::Arc::from(h);
                let closure: privchat_sdk::storage::media_preprocess::VideoProcessHook =
                    std::sync::Arc::new(move |op, p1, p2, p3| {
                        let op_ffi = match op {
                            privchat_sdk::storage::media_preprocess::MediaProcessOp::Thumbnail => MediaProcessOp::Thumbnail,
                            privchat_sdk::storage::media_preprocess::MediaProcessOp::Compress => MediaProcessOp::Compress,
                        };
                        h.process(op_ffi, p1.to_string_lossy().to_string(), p2.to_string_lossy().to_string(), p3.to_string_lossy().to_string())
                            .map_err(|e| e.to_string())
                    });
                closure
            });
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.set_video_process_hook(sdk_hook).await;
                info!("Video process hook {}", if was_set { "set" } else { "removed" });
            }
        });
    }

    /// 移除视频处理钩子
    pub fn remove_video_process_hook(&self) {
        self.set_video_process_hook(None);
    }
    
    // ========== Phase 4: Advanced Features ==========
    
    // ---------- Account Management ----------
    
    /// Register a new user account
    /// 
    /// **类型化返回**：返回 `AuthResult` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(AuthResult)`: 包含 user_id 和 token
    /// - `Err(PrivchatError)`: 注册失败
    pub fn register(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<AuthResult, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let device_info = privchat_protocol::message::DeviceInfo {
                    device_id: device_id.clone(),
                    device_type: privchat_protocol::message::DeviceType::Web,
                    app_id: "privchat_ffi".to_string(),
                    push_token: None,
                    push_channel: None,
                    device_name: "Privchat FFI".to_string(),
                    device_model: None,
                    os_version: Some(std::env::consts::OS.to_string()),
                    app_version: Some("1.0.0".to_string()),
                    manufacturer: None,
                    device_fingerprint: None,
                };
                let (user_id, token) = sdk.register(username, password, device_id, Some(device_info)).await?;
                Ok(AuthResult {
                    user_id,
                    token,
                })
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// Login with existing account
    /// 
    /// **类型化返回**：返回 `AuthResult` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(AuthResult)`: 包含 user_id 和 token
    /// - `Err(PrivchatError)`: 登录失败
    pub fn login(
        &self,
        username: String,
        password: String,
        device_id: String,
    ) -> Result<AuthResult, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let (user_id, token) = sdk.login(username, password, device_id, None).await?;
                Ok(AuthResult {
                    user_id,
                    token,
                })
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// Authenticate after login/register.
    /// Runs on our Tokio runtime via block_on.
    pub fn authenticate(&self, user_id: u64, token: String, device_id: String) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let device_info = privchat_protocol::message::DeviceInfo {
                    device_id,
                    device_type: privchat_protocol::message::DeviceType::Web,
                    app_id: "privchat_ffi".to_string(),
                    push_token: None,
                    push_channel: None,
                    device_name: "Privchat FFI".to_string(),
                    device_model: None,
                    os_version: Some(std::env::consts::OS.to_string()),
                    app_version: Some("1.0.0".to_string()),
                    manufacturer: None,
                    device_fingerprint: None,
                };
                sdk.authenticate(user_id, &token, device_info).await?;
                Ok(())
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    // ---------- Friend Management ----------
    
    /// Get friends list (from local database, Entity Model V1: Friend + User)
    /// 
    /// **类型化返回**：返回 `Vec<FriendEntry>` 而不是 JSON 字符串。
    pub fn get_friends(&self, limit: Option<u32>, offset: Option<u32>) -> Result<Vec<FriendEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let limit = limit.unwrap_or(100);
                let offset = offset.unwrap_or(0);
                let pairs = sdk.get_friends(limit, offset).await?;
                let ffi_friends: Vec<FriendEntry> = pairs
                    .into_iter()
                    .map(|(f, u)| FriendEntry {
                        user_id: f.user_id,
                        username: u.username.unwrap_or_default(),
                        nickname: u.nickname,
                        avatar_url: if u.avatar.is_empty() { None } else { Some(u.avatar) },
                        user_type: u.user_type as i16,
                        status: "accepted".to_string(),
                        added_at: f.created_at,
                        remark: u.alias,
                    })
                    .collect();
                Ok(ffi_friends)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 统一实体同步（ENTITY_SYNC_V1）
    /// entity_type: "friend" | "group" | "channel" | "group_member" | "user" | "user_settings" | "user_block"
    /// scope: 可选，如 group_member 时为 Some(group_id.to_string())
    /// 返回本轮同步并落库的条数
    pub fn sync_entities(&self, entity_type: String, scope: Option<String>) -> Result<u32, PrivchatError> {
        use std::str::FromStr;
        let entity_type_parsed = privchat_sdk::EntityType::from_str(entity_type.as_str())
            .map_err(|_| PrivchatError::Generic { msg: format!("invalid entity_type: {}", entity_type) })?;
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let count = sdk.sync_entities(entity_type_parsed, scope.as_deref()).await?;
                Ok(count as u32)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }

    /// 在后台执行实体同步（不阻塞）
    /// entity_type/scope 同上
    pub fn sync_entities_in_background(&self, entity_type: String, scope: Option<String>) {
        use std::str::FromStr;
        if let Ok(entity_type_parsed) = privchat_sdk::EntityType::from_str(entity_type.as_str()) {
            let inner = self.inner.clone();
            tokio::spawn(async move {
                let guard = inner.read().await;
                if let Some(sdk) = guard.as_ref() {
                    privchat_sdk::PrivchatSDK::sync_entities_in_background(
                        sdk.clone(),
                        entity_type_parsed,
                        scope,
                    );
                }
            });
        }
    }
    
    /// 是否已完成过首次 Bootstrap（本地曾完整跑完 Friend→Group→Channel→UserSettings）。
    /// 用于首次登录设备必须强制全量初始化：若返回 false，应阻塞直到 run_bootstrap_sync() 成功。
    pub fn is_bootstrap_completed(&self) -> Result<bool, PrivchatError> {
        sdk_ffi_log!("is_bootstrap_completed");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let r = sdk.is_bootstrap_completed().await.map_err(Into::into);
                if r.is_err() {
                    warn!("[FFI] ⚠️ is_bootstrap_completed: SDK 返回错误 (inner 有值)");
                }
                r
            } else {
                warn!("[FFI] ⚠️ is_bootstrap_completed: inner is None (SDK 未初始化或已 shutdown)");
                Err(PrivchatError::Disconnected)
            }
        })
    }

    /// 执行启动同步（Bootstrap）：Friend → Group → Channel → UserSettings，串行、遇错即返（阻塞）。
    /// 用于首次初始化：is_bootstrap_completed 为 false 时必须调用并等待完成。
    pub fn run_bootstrap_sync(&self) -> Result<(), PrivchatError> {
        sdk_ffi_log!("run_bootstrap_sync");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let r = sdk.run_bootstrap_sync().await.map_err(Into::into);
                if r.is_err() {
                    warn!("[FFI] ⚠️ run_bootstrap_sync: SDK 返回错误 (inner 有值，多半是 check_connected/is_connected 为 false)");
                }
                r
            } else {
                warn!("[FFI] ⚠️ run_bootstrap_sync: inner is None (SDK 未初始化或已 shutdown)");
                Err(PrivchatError::Disconnected)
            }
        })
    }

    /// 在后台执行启动同步（不阻塞）。用于已初始化时的增量补齐：is_bootstrap_completed 为 true 时调用。
    pub fn run_bootstrap_sync_in_background(&self) {
        sdk_ffi_log!("run_bootstrap_sync_in_background");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                privchat_sdk::PrivchatSDK::run_bootstrap_sync_in_background(sdk.clone());
            }
        });
    }
    
    /// 从本地数据库获取单条用户设置（ENTITY_SYNC_V1 user_settings，只读 DB）
    /// key 如 "theme", "notification_enabled"；返回值为 JSON 字符串，不存在返回 None
    pub fn get_user_setting(&self, key: String) -> Result<Option<String>, PrivchatError> {
        sdk_ffi_log!("get_user_setting");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let opt = sdk.get_user_setting(&key).await?;
                Ok(opt.map(|v| v.to_string()))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 从本地数据库获取当前用户全部设置（JSON 对象字符串，如 {"theme":"dark","notification_enabled":true}）
    pub fn get_all_user_settings(&self) -> Result<String, PrivchatError> {
        sdk_ffi_log!("get_all_user_settings");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let map = sdk.get_all_user_settings().await?;
                serde_json::to_string(&map).map_err(|e| PrivchatError::Generic { msg: e.to_string() })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Search users by query
    /// 
    /// **类型化返回**：返回 `Vec<UserEntry>` 而不是 JSON 字符串。
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    pub fn search_users(&self, query: String) -> Result<Vec<UserEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.search_users(&query).await?;
                
                // 解析 JSON 结果并转换为 UserEntry
                // 注意：search_users 返回的是 JSON，需要解析
                // 理想情况下，SDK 层应该返回 Vec<UserEntry>，但当前返回 JSON
                if let Some(users_array) = result.get("users").and_then(|v| v.as_array()) {
                    let mut entries = Vec::new();
                    for user_value in users_array {
                        if let (Some(user_id), Some(username)) = (
                            user_value.get("user_id").and_then(|v| v.as_u64()),
                            user_value.get("username").and_then(|v| v.as_str()),
                        ) {
                            entries.push(UserEntry {
                                user_id,
                                username: username.to_string(),
                                nickname: user_value.get("nickname").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                avatar_url: user_value.get("avatar_url").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                user_type: user_value.get("user_type").and_then(|v| v.as_i64()).unwrap_or(0) as i16,
                                is_friend: user_value.get("is_friend").and_then(|v| v.as_bool()).unwrap_or(false),
                                can_send_message: user_value.get("can_send_message").and_then(|v| v.as_bool()).unwrap_or(false),
                                search_session_id: user_value.get("search_session_id").and_then(|v| v.as_u64()),
                                is_online: user_value.get("is_online").and_then(|v| v.as_bool()),
                            });
                        }
                    }
                    Ok(entries)
                } else {
                    Ok(Vec::new())
                }
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取待处理好友申请列表（别人申请我为好友的请求）
    pub fn list_friend_pending_requests(&self) -> Result<Vec<FriendPendingEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let response = sdk.get_friend_pending_requests().await?;
                let entries = response.requests.into_iter().map(|r| FriendPendingEntry {
                    from_user_id: r.from_user_id,
                    message: r.message,
                    created_at: r.created_at,
                }).collect();
                Ok(entries)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Send friend request
    ///
    /// 当从「搜索用户」添加好友时，必须传 `search_session_id`（来源 id），服务端要求 source + source_id。
    pub fn send_friend_request(
        &self,
        to_user_id: u64,
        remark: Option<String>,
        search_session_id: Option<String>,
    ) -> Result<String, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.send_friend_request(to_user_id, remark.as_deref(), search_session_id).await?;
                Ok(serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string()))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Accept friend request
    /// 
    /// **类型化返回**：返回 `u64` (channel_id) 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(u64)`: 创建的会话 ID (channel_id)
    /// - `Err(PrivchatError)`: 操作失败
    pub fn accept_friend_request(&self, from_user_id: u64) -> Result<u64, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.accept_friend_request(from_user_id).await?;
                // SDK 层返回 JSON，解析 channel_id
                let channel_id = result.get("channel_id")
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| PrivchatError::Generic { msg: "Missing channel_id in response".to_string() })?;
                Ok(channel_id)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Reject friend request
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn reject_friend_request(&self, from_user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.reject_friend_request(from_user_id).await?;
                // SDK 层返回 JSON，解析 bool
                let success = result.as_bool().unwrap_or(false);
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Delete a friend
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn delete_friend(&self, user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.delete_friend(user_id).await?;
                // SDK 层返回 JSON，解析 bool
                let success = result.as_bool().unwrap_or(false);
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Group Management ----------
    
    /// Create a new group
    /// 
    /// **类型化返回**：返回 `GroupCreateResult` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(GroupCreateResult)`: 创建的群组信息
    /// - `Err(PrivchatError)`: 操作失败
    pub fn create_group(&self, name: String, member_ids: Vec<u64>) -> Result<GroupCreateResult, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // SDK 层已经返回 GroupCreateResponse，直接转换
                let response = sdk.create_group(&name, member_ids).await?;
                Ok(GroupCreateResult {
                    group_id: response.group_id,
                    name: response.name,
                    description: response.description,
                    member_count: response.member_count,
                    created_at: response.created_at,
                    creator_id: response.creator_id,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Invite users to group
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功（至少成功邀请一个用户）
    /// - `Err(PrivchatError)`: 操作失败
    pub fn invite_to_group(&self, group_id: u64, user_ids: Vec<u64>) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.invite_to_group(group_id, user_ids).await?;
                // SDK 层返回 JSON，解析 success 字段
                let success = result.get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Join a group by QR code
    /// 
    /// **类型化返回**：返回 `GroupQRCodeJoinResult` 而不是 JSON 字符串。
    /// 
    /// 扫描群组二维码后，使用此方法加入群组。
    /// 加入群组后，服务端会自动将用户加入到对应的 channel 中。
    /// 
    /// # 参数
    /// - `qr_key`: 二维码 key（从二维码 URL 中提取）
    /// - `token`: 二维码 token（可选，从二维码 URL 中提取）
    /// - `message`: 申请理由（可选，如果需要审批）
    /// 
    /// # 返回
    /// - `Ok(GroupQRCodeJoinResult)`: 加入结果（可能返回 pending 状态，需要审批）
    /// - `Err(PrivchatError)`: 操作失败
    pub fn join_group_by_qrcode(
        &self,
        qr_key: String,
        token: Option<String>,
        message: Option<String>,
    ) -> Result<GroupQRCodeJoinResult, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let response = sdk.join_group_by_qrcode(qr_key, token, message).await?;
                Ok(GroupQRCodeJoinResult {
                    status: response.status,
                    group_id: response.group_id,
                    request_id: response.request_id,
                    message: response.message,
                    expires_at: response.expires_at,
                    user_id: response.user_id,
                    joined_at: response.joined_at,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Leave a group
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn leave_group(&self, group_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // SDK 层已返回 bool
                let success = sdk.leave_group(group_id).await?;
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 从本地数据库获取群列表（分页）
    pub fn get_groups(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<GroupEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let limit = limit.unwrap_or(100);
                let offset = offset.unwrap_or(0);
                let groups = sdk.get_groups(limit, offset).await?;
                let entries: Vec<GroupEntry> = groups
                    .into_iter()
                    .map(|g| GroupEntry {
                        group_id: g.group_id,
                        name: g.name,
                        avatar: g.avatar,
                        owner_id: g.owner_id,
                        is_dismissed: g.is_dismissed,
                        created_at: g.created_at,
                        updated_at: g.updated_at,
                    })
                    .collect();
                Ok(entries)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get group members list (from local database, 按 group_id 关联)
    /// 
    /// **类型化返回**：返回 `Vec<GroupMemberEntry>`。
    pub fn get_group_members(
        &self,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<GroupMemberEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_members = sdk.get_group_members(group_id, limit, offset).await?;
                
                // 转换为 FFI 类型
                let ffi_members: Vec<GroupMemberEntry> = sdk_members
                    .iter()
                    .map(|m| GroupMemberEntry {
                        user_id: m.member_uid,
                        channel_id: m.channel_id,
                        channel_type: m.channel_type,
                        name: m.member_name.clone(),
                        remark: m.member_remark.clone(),
                        avatar: m.member_avatar.clone(),
                        role: m.role,
                        status: m.status,
                        invite_user_id: m.member_invite_uid,
                    })
                    .collect();
                
                Ok(ffi_members)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Remove member from group
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn remove_group_member(&self, group_id: u64, user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // SDK 层已返回 bool
                let success = sdk.remove_group_member(group_id, user_id).await?;
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Message Management ----------
    
    /// Get message history for a channel (from local database)
    /// 
    /// **类型化返回**：返回 `Vec<MessageEntry>` 而不是 JSON 字符串，提供类型安全和更好的性能。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `limit`: 获取数量
    /// - `before_message_id`: 在此消息 ID 之前的消息（用于分页，可选）
    /// 
    /// # 返回
    /// - `Ok(Vec<MessageEntry>)`: 消息列表（类型化对象）
    pub fn get_message_history(
        &self,
        channel_id: u64,
        limit: u32,
        before_message_id: Option<u64>,
    ) -> Result<Vec<MessageEntry>, PrivchatError> {
        sdk_ffi_log!("get_message_history");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 调用 SDK 的公开方法获取本地消息历史；游标为 message.id
                let sdk_messages = sdk.get_messages(channel_id, limit, before_message_id).await?;
                
                // 转换为 FFI 层：客户端统一使用 message.id 作为 id
                let ffi_messages: Vec<MessageEntry> = sdk_messages
                    .iter()
                    .map(|msg| {
                        let client_id = msg.id.map(|i| i as u64).unwrap_or(0);
                        Self::convert_sdk_message_to_ffi(msg, client_id)
                    })
                    .collect();
                
                Ok(ffi_messages)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 根据 message.id 获取单条消息（用于发送成功后刷新时间线）
    ///
    /// 合约 v1：入参为本地主键 message.id（send_message 返回值）。
    pub fn get_message_by_id(&self, message_id: u64) -> Result<Option<MessageEntry>, PrivchatError> {
        sdk_ffi_log!("get_message_by_id");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let msg = sdk.get_message_by_id(message_id).await?;
                Ok(msg.map(|m| Self::convert_sdk_message_to_ffi(&m, message_id)))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Paginate back (load older messages)
    /// 
    /// Load older messages before the specified message ID, for scrolling up to load history.
    /// 
    /// **类型化返回**：返回 `Vec<MessageEntry>` 而不是 JSON 字符串。
    /// 
    /// **Timeline Observer 集成**：加载的消息会通过 Timeline Observer 的 `Append` 事件通知。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `before_id`: 在此 message.id 之前的消息（当前显示的最早一条的 id，用于分页游标）
    /// - `count`: 加载数量
    /// 
    /// # 返回
    /// - `Ok(Vec<MessageEntry>)`: 加载的消息列表（按时间倒序，最新的在前）
    pub fn paginate_back(
        &self,
        channel_id: u64,
        before_id: u64,
        count: u32,
    ) -> Result<Vec<MessageEntry>, PrivchatError> {
        sdk_ffi_log!("paginate_back");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_messages = sdk.paginate_back(channel_id, before_id, count).await?;
                
                // 转换为 FFI 层：客户端统一使用 message.id 作为 id
                let ffi_messages: Vec<MessageEntry> = sdk_messages
                    .iter()
                    .map(|msg| {
                        let client_id = msg.id.map(|i| i as u64).unwrap_or(0);
                        Self::convert_sdk_message_to_ffi(msg, client_id)
                    })
                    .collect();
                
                Ok(ffi_messages)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取频道当前最小的 message.id（用于「加载更早」分页游标）
    pub fn get_earliest_id(&self, channel_id: u64) -> Result<Option<u64>, PrivchatError> {
        sdk_ffi_log!("get_earliest_id");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.get_earliest_id(channel_id).await.map_err(Into::into)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Paginate forward (load newer messages)
    /// 
    /// Load newer messages after the specified message ID, for scrolling down to load new messages.
    /// 
    /// **类型化返回**：返回 `Vec<MessageEntry>` 而不是 JSON 字符串。
    /// 
    /// **Timeline Observer 集成**：加载的消息会通过 Timeline Observer 的 `Append` 事件通知。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `after_message_id`: 在此消息 ID 之后的消息（通常是当前显示的最新消息 ID）
    /// - `count`: 加载数量
    /// 
    /// # 返回
    /// - `Ok(Vec<MessageEntry>)`: 加载的消息列表（按时间正序，最早的在前）
    pub fn paginate_forward(
        &self,
        channel_id: u64,
        after_id: u64,
        count: u32,
    ) -> Result<Vec<MessageEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_messages = sdk.paginate_forward(channel_id, after_id, count).await?;
                
                let ffi_messages: Vec<MessageEntry> = sdk_messages
                    .iter()
                    .map(|msg| {
                        let client_id = msg.id.map(|i| i as u64).unwrap_or(0);
                        Self::convert_sdk_message_to_ffi(msg, client_id)
                    })
                    .collect();
                
                Ok(ffi_messages)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Sync message history from server to local database
    /// Returns the number of synced messages
    pub fn sync_messages(
        &self,
        channel_id: u64,
        limit: Option<u32>,
    ) -> Result<u32, PrivchatError> {
        sdk_ffi_log!("sync_messages");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let count = sdk.sync_messages(channel_id, limit).await?;
                Ok(count as u32)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Search messages by keyword
    /// 
    /// **类型化返回**：返回 `Vec<MessageEntry>` 而不是 JSON 字符串。
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    /// 
    /// # 参数
    /// - `query`: 搜索关键词
    /// - `channel_id`: 可选的频道 ID（限定搜索范围）
    /// 
    /// # 返回
    /// - `Ok(Vec<MessageEntry>)`: 搜索结果（类型化对象）
    pub fn search_messages(
        &self,
        query: String,
        channel_id: Option<String>,
    ) -> Result<Vec<MessageEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let ch_id = channel_id.as_deref();
                let result = sdk.search_messages(&query, ch_id).await?;
                
                // 解析 JSON 结果并转换为 Message 类型
                // 注意：search_messages 返回的是 JSON，需要解析
                // 理想情况下，SDK 层应该返回 Vec<Message>，但当前返回 JSON
                // 这里先解析 JSON，后续可以优化 SDK 层
                if let Some(messages_array) = result.get("messages").and_then(|v| v.as_array()) {
                    let mut ffi_messages = Vec::new();
                    for msg_value in messages_array {
                        if let (Some(msg_id), Some(ch_id), Some(from_uid), Some(content), Some(ts)) = (
                            msg_value.get("message_id").and_then(|v| v.as_u64()),
                            msg_value.get("channel_id").and_then(|v| v.as_u64()),
                            msg_value.get("from_uid").and_then(|v| v.as_u64()),
                            msg_value.get("content").and_then(|v| v.as_str()),
                            msg_value.get("timestamp").and_then(|v| v.as_u64()),
                        ) {
                            use crate::events::MessageStatus;
                            // 搜索结果可能无客户端 id，用 message_id 作为 id
                            ffi_messages.push(MessageEntry {
                                id: msg_id,
                                server_message_id: Some(msg_id),
                                channel_id: ch_id,
                                channel_type: msg_value.get("channel_type").and_then(|v| v.as_i64()).unwrap_or(1) as i32,
                                from_uid,
                                content: content.to_string(),
                                status: MessageStatus::Sent,
                                timestamp: ts,
                            });
                        }
                    }
                    Ok(ffi_messages)
                } else {
                    Ok(Vec::new())
                }
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Channel Management ----------
    
    /// Get channel list (from local database)
    /// 
    /// **类型化返回**：返回 `Vec<ChannelListEntry>` 而不是 JSON 字符串，提供类型安全和更好的性能。
    /// 
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    /// 业务逻辑（获取 Channel 信息、成员数量等）在 SDK 层完成。
    /// 
    /// # 参数
    /// - `limit`: 获取数量限制（可选）
    /// - `offset`: 偏移量（可选）
    /// 
    /// # 返回
    /// - `Ok(Vec<ChannelListEntry>)`: 会话列表（类型化对象，包含完整信息）
    pub fn get_channels(
        &self,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<ChannelListEntry>, PrivchatError> {
        sdk_ffi_log!("get_channels");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 创建查询条件
                let query = privchat_sdk::storage::entities::ChannelQuery {
                    limit,
                    offset,
                    ..Default::default()
                };
                
                // 调用 SDK 层的方法，直接获取 ChannelListEntry（业务逻辑在 SDK 层）
                let sdk_entries = sdk.get_channel_list_entries(&query).await?;
                
                // 转换为 FFI 类型（简单类型转换）
                let ffi_entries: Vec<ChannelListEntry> = sdk_entries
                    .iter()
                    .map(|e| Self::convert_sdk_channel_list_entry_to_ffi(e))
                    .collect();
                
                Ok(ffi_entries)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Mark channel as read
    pub fn mark_channel_read(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 调用 SDK 的公开方法标记会话已读
                sdk.mark_channel_read(channel_id, channel_type).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Presence/Online Status ----------
    
    /// Subscribe to user presence status
    /// 
    /// **类型化返回**：返回 `Vec<PresenceEntry>` 而不是 JSON 字符串。
    pub fn subscribe_presence(&self, user_ids: Vec<u64>) -> Result<Vec<PresenceEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let statuses = sdk.subscribe_presence(user_ids).await?;
                Ok(statuses
                    .into_iter()
                    .map(|(user_id, status)| Self::convert_online_status_to_presence_info(user_id, &status))
                    .collect())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Unsubscribe from user presence status
    pub fn unsubscribe_presence(&self, user_ids: Vec<u64>) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.unsubscribe_presence(user_ids).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get user presence status from cache
    /// 
    /// **类型化返回**：返回 `Option<PresenceEntry>` 而不是 JSON 字符串。
    pub fn get_presence(&self, user_id: u64) -> Option<PresenceEntry> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.get_presence(user_id).await
                    .map(|status| Self::convert_online_status_to_presence_info(user_id, &status))
            } else {
                None
            }
        })
    }
    
    /// Batch get user presence statuses from cache
    /// 
    /// **类型化返回**：返回 `Vec<PresenceEntry>` 而不是 JSON 字符串。
    pub fn batch_get_presence(&self, user_ids: Vec<u64>) -> Vec<PresenceEntry> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let statuses = sdk.batch_get_presence(&user_ids).await;
                statuses
                    .into_iter()
                    .map(|(user_id, status)| Self::convert_online_status_to_presence_info(user_id, &status))
                    .collect()
            } else {
                Vec::new()
            }
        })
    }
    
    /// Fetch user presence statuses from server
    /// 
    /// **类型化返回**：返回 `Vec<PresenceEntry>` 而不是 JSON 字符串。
    pub fn fetch_presence(&self, user_ids: Vec<u64>) -> Result<Vec<PresenceEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let statuses = sdk.fetch_presence(user_ids).await?;
                Ok(statuses
                    .into_iter()
                    .map(|(user_id, status)| Self::convert_online_status_to_presence_info(user_id, &status))
                    .collect())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    
    // ---------- Typing Indicators ----------
    
    /// Send typing indicator (start typing)
    pub fn send_typing(&self, channel_id: u64) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.send_typing(channel_id, None).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Stop typing indicator
    pub fn stop_typing(&self, channel_id: u64) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.stop_typing(channel_id).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Advanced Message Operations ----------
    
    /// Revoke/recall a message
    pub fn revoke_message(&self, message_id: u64) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.recall_message(message_id).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Edit a message
    pub fn edit_message(&self, message_id: u64, new_content: String) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.edit_message(message_id, &new_content).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Add reaction to a message
    pub fn add_reaction(&self, message_id: u64, emoji: String) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.add_reaction(message_id, &emoji).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Remove reaction from a message
    pub fn remove_reaction(&self, message_id: u64, emoji: String) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.remove_reaction(message_id, &emoji).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Blacklist Management ----------
    
    /// Add user to blacklist
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn add_to_blacklist(&self, user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.add_to_blacklist(user_id).await?;
                // SDK 层返回 JSON，解析 bool
                let success = result.as_bool().unwrap_or(false);
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Remove user from blacklist
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn remove_from_blacklist(&self, user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.remove_from_blacklist(user_id).await?;
                // SDK 层返回 JSON，解析 bool
                let success = result.as_bool().unwrap_or(false);
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get blacklist
    /// 
    /// **类型化返回**：返回 `Vec<BlacklistEntry>` 而不是 JSON 字符串。
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    pub fn get_blacklist(&self) -> Result<Vec<BlacklistEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let result = sdk.get_blacklist().await?;
                
                // 解析 JSON 结果并转换为 BlacklistEntry
                // 注意：get_blacklist 返回的是 JSON，需要解析
                // 理想情况下，SDK 层应该返回 Vec<BlacklistEntry>，但当前返回 JSON
                if let Some(users_array) = result.get("users").and_then(|v| v.as_array()) {
                    let mut entries = Vec::new();
                    for user_value in users_array {
                        if let (Some(user_id), Some(username), Some(blocked_at)) = (
                            user_value.get("user_id").and_then(|v| v.as_u64()),
                            user_value.get("username").and_then(|v| v.as_str()),
                            user_value.get("blocked_at").and_then(|v| v.as_i64()),
                        ) {
                            entries.push(BlacklistEntry {
                                user_id,
                                username: username.to_string(),
                                nickname: user_value.get("nickname").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                avatar_url: user_value.get("avatar_url").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                blocked_at,
                            });
                        }
                    }
                    Ok(entries)
                } else {
                    Ok(Vec::new())
                }
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Channel Advanced Management ----------
    
    /// Pin or unpin a channel
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn pin_channel(&self, channel_id: u64, pin: bool) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // SDK 层已返回 bool
                let success = sdk.pin_channel(channel_id, pin).await?;
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Hide a channel
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// 隐藏频道不会删除频道，只是不在用户的会话列表中显示。
    /// 好友关系和群组关系仍然保留。
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn hide_channel(&self, channel_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // SDK 层已返回 bool
                let success = sdk.hide_channel(channel_id).await?;
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Mute a channel
    /// 
    /// **类型化返回**：返回 `bool` 而不是 JSON 字符串。
    /// 
    /// 设置频道静音后，该频道的新消息将不会推送通知。
    /// 这是用户个人的偏好设置，适用于私聊和群聊。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `muted`: true 表示静音，false 表示取消静音
    /// 
    /// # 返回
    /// - `Ok(bool)`: 操作是否成功
    /// - `Err(PrivchatError)`: 操作失败
    pub fn mute_channel(&self, channel_id: u64, muted: bool) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let success = sdk.mute_channel(channel_id, muted).await?;
                Ok(success)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get unread statistics for a channel
    /// 
    /// **类型化返回**：返回 `UnreadStats` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// 
    /// # 返回
    /// - `Ok(UnreadStats)`: 未读统计（messages, notifications, mentions）
    pub fn channel_unread_stats(&self, channel_id: u64) -> Result<UnreadStats, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let stats = sdk.channel_unread_stats(channel_id).await?;
                Ok(UnreadStats {
                    messages: stats.messages,
                    notifications: stats.notifications,
                    mentions: stats.mentions,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get own last read position for a channel
    /// 
    /// **类型化返回**：返回 `LastReadPosition` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// 
    /// # 返回
    /// - `Ok(LastReadPosition)`: 最后已读位置（message_id, timestamp）
    pub fn own_last_read(&self, channel_id: u64) -> Result<LastReadPosition, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let (msg_id, ts) = sdk.own_last_read(channel_id).await?;
                Ok(LastReadPosition {
                    server_message_id: msg_id,
                    timestamp: ts,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Mark channel as fully read at a specific message
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 频道 ID
    /// - `message_id`: 要标记为已读的消息 ID
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn mark_fully_read_at(&self, channel_id: u64, message_id: u64) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.mark_fully_read_at(channel_id, message_id).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Lifecycle Methods ----------
    
    /// App 切换到后台
    /// 
    /// **业务层必须调用**：在 App 切换到后台时调用此方法。
    /// 
    /// 这是 SDK 的一级生命周期事件，会自动触发所有已注册的 Hook：
    /// - Push Hook：更新设备推送状态（push_armed = true）
    /// - Transport Hook：降级实时连接策略（降低心跳频率）
    /// - Presence Hook：Flush presence 状态
    /// - Message Hook：停止非必要操作，Flush 关键状态
    /// 
    /// **注意**：Push Hook 会在 SDK 初始化时自动注册，无需用户手动注册。
    /// 
    /// # 平台调用示例
    /// 
    /// **iOS (Swift):**
    /// ```swift
    /// func applicationDidEnterBackground(_ application: UIApplication) {
    ///     sdk.onAppBackground()
    /// }
    /// ```
    /// 
    /// **Android (Kotlin):**
    /// ```kotlin
    /// override fun onPause() {
    ///     super.onPause()
    ///     sdk.onAppBackground()
    /// }
    /// ```
    pub fn on_app_background(self: Arc<Self>) -> Result<(), PrivchatError> {
        let this = self.clone();
        get_runtime().block_on(async move {
            let guard = this.inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.on_app_background().await
                    .map_err(|e| PrivchatError::from(e))
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    /// App 切换到前台
    /// 
    /// **业务层必须调用**：在 App 切换到前台时调用此方法。
    /// 
    /// 这是 SDK 的一级生命周期事件，会自动触发所有已注册的 Hook：
    /// - Push Hook：更新设备推送状态（push_armed = false）
    /// - Transport Hook：恢复实时连接策略（正常心跳频率）
    /// - Presence Hook：更新 presence 状态
    /// - Message Hook：同步离线消息，恢复后台暂停的任务
    /// 
    /// # 平台调用示例
    /// 
    /// **iOS (Swift):**
    /// ```swift
    /// func applicationWillEnterForeground(_ application: UIApplication) {
    ///     sdk.onAppForeground()
    /// }
    /// ```
    /// 
    /// **Android (Kotlin):**
    /// ```kotlin
    /// override fun onResume() {
    ///     super.onResume()
    ///     sdk.onAppForeground()
    /// }
    /// ```
    pub fn on_app_foreground(self: Arc<Self>) -> Result<(), PrivchatError> {
        let this = self.clone();
        get_runtime().block_on(async move {
            let guard = this.inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.on_app_foreground().await
                    .map_err(|e| PrivchatError::from(e))?;
                // 前台时后台同步好友列表（local-first：创建群组等可立即用最新好友）
                privchat_sdk::PrivchatSDK::sync_entities_in_background(sdk.clone(), privchat_sdk::EntityType::Friend, None);
                Ok(())
            } else {
                Err(PrivchatError::NotInitialized)
            }
        })
    }
    
    pub fn is_connected(&self) -> bool {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.is_connected().await
            } else {
                false
            }
        })
    }
    
    // ---------- Channel Management ----------
    
    /// Leave a channel
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn leave_channel(
        &self,
        channel_id: u64,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.leave_channel(channel_id).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取群成员列表（按 group_id 关联）
    /// 
    /// **类型化返回**：返回 `Vec<GroupMemberEntry>`。
    /// 
    /// # 参数
    /// - `group_id`: 群组 ID
    /// - `limit`: 每页数量（可选）
    /// - `offset`: 偏移量（可选）
    pub fn list_members(
        &self,
        group_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<GroupMemberEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let members = sdk.get_group_members(group_id, limit, offset).await?;
                let ffi_members: Vec<GroupMemberEntry> = members.iter()
                    .map(|m| GroupMemberEntry {
                        user_id: m.member_uid,
                        channel_id: group_id,
                        channel_type: 2,
                        name: m.member_name.clone(),
                        remark: m.member_remark.clone(),
                        avatar: m.member_avatar.clone(),
                        role: m.role,
                        status: m.status,
                        invite_user_id: m.member_invite_uid,
                    })
                    .collect();
                Ok(ffi_members)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Invite user to channel
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `channel_type`: 会话类型（1: 私聊, 2: 群聊）
    /// - `user_ids`: 要邀请的用户ID列表
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn invite_user(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_ids: Vec<u64>,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.add_channel_members(channel_id, channel_type, user_ids).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Kick user from channel
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `channel_type`: 会话类型（1: 私聊, 2: 群聊）
    /// - `user_id`: 要移除的用户ID
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn kick_user(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.remove_channel_member(channel_id, channel_type, user_id).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get channel profile
    /// 
    /// **类型化返回**：返回 `ChannelListEntry` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// 
    /// # 返回
    /// - `Ok(ChannelListEntry)`: 会话信息
    pub fn channel_profile(
        &self,
        channel_id: u64,
    ) -> Result<ChannelListEntry, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let query = privchat_sdk::storage::entities::ChannelQuery {
                    channel_id: Some(channel_id),
                    ..Default::default()
                };
                
                let entries = sdk.get_channel_list_entries(&query).await?;
                let entry = entries.first()
                    .ok_or_else(|| PrivchatError::Generic { msg: "会话不存在".to_string() })?;
                
                Ok(Self::convert_sdk_channel_list_entry_to_ffi(entry))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 根据对端用户 ID 查找本地私聊会话的 channel_id（非好友发消息流程：先查本地）
    ///
    /// 若本地已有与该用户的私聊会话则返回 Some(channel_id)，否则返回 None。
    pub fn get_direct_channel_id_by_peer_user_id(&self, peer_user_id: u64) -> Result<Option<u64>, PrivchatError> {
        sdk_ffi_log!("get_direct_channel_id_by_peer_user_id");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.storage().find_channel_id_by_user(peer_user_id).await
                    .map_err(PrivchatError::from)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取或创建与某用户的私聊会话（非好友发消息：新对话首条发送时调用）
    ///
    /// source/source_id 与添加好友规范一致，可选。
    pub fn get_or_create_direct_channel(
        &self,
        peer_user_id: u64,
        source: Option<String>,
        source_id: Option<String>,
    ) -> Result<GetOrCreateDirectChannelResult, PrivchatError> {
        sdk_ffi_log!("get_or_create_direct_channel");
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let (channel_id, created) = sdk.get_or_create_direct_channel(peer_user_id, source, source_id).await
                    .map_err(PrivchatError::from)?;
                Ok(GetOrCreateDirectChannelResult { channel_id, created })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    // ---------- Phase 8: PTS-Based Synchronization ----------
    
    /// Synchronize a single channel (fetch missing messages based on PTS)
    /// 
    /// **类型化返回**：返回 `SyncStateEntry` 而不是 JSON 字符串。
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    pub fn sync_channel(&self, channel_id: u64, channel_type: u8) -> Result<SyncStateEntry, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sync_state = sdk.sync_channel(channel_id, channel_type).await?;
                
                // 转换为 SyncStateEntry
                use privchat_sdk::sync::SyncState;
                let needs_sync = matches!(sync_state.state, SyncState::Syncing | SyncState::HasGap { .. });
                Ok(SyncStateEntry {
                    channel_id: sync_state.channel_id,
                    channel_type: sync_state.channel_type as i32,
                    local_pts: sync_state.local_pts,
                    server_pts: sync_state.server_pts,
                    needs_sync,
                    last_sync_at: Some(sync_state.last_sync_at),
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Batch synchronize all channels
    /// 
    /// **类型化返回**：返回 `Vec<SyncStateEntry>` 而不是 JSON 字符串。
    pub fn sync_all_channels(&self) -> Result<Vec<SyncStateEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sync_states = sdk.sync_all_channels().await?;
                
                // 转换为 Vec<SyncStateEntry>
                use privchat_sdk::sync::SyncState;
                Ok(sync_states
                    .into_iter()
                    .map(|state| {
                        let needs_sync = matches!(state.state, SyncState::Syncing | SyncState::HasGap { .. });
                        SyncStateEntry {
                            channel_id: state.channel_id,
                            channel_type: state.channel_type as i32,
                            local_pts: state.local_pts,
                            server_pts: state.server_pts,
                            needs_sync,
                            last_sync_at: Some(state.last_sync_at),
                        }
                    })
                    .collect())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get channel synchronization state (local PTS vs server PTS)
    /// 
    /// **类型化返回**：返回 `SyncStateEntry` 而不是 JSON 字符串。
    /// **FFI 层实现**：简单调用 SDK 层的方法，只做类型转换。
    pub fn get_channel_sync_state(&self, channel_id: u64, channel_type: u8) -> Result<SyncStateEntry, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let (local_pts, server_pts) = sdk.get_channel_sync_state(channel_id, channel_type).await?;
                
                // 检查是否需要同步
                let needs_sync = sdk.needs_sync(channel_id, channel_type).await.unwrap_or(false);
                
                // 转换为 SyncStateEntry
                Ok(SyncStateEntry {
                    channel_id,
                    channel_type: channel_type as i32,
                    local_pts,
                    server_pts,
                    needs_sync,
                    last_sync_at: None, // TODO: 从 SDK 层获取最后同步时间
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Check if channel needs synchronization (has PTS gap)
    /// Returns true if local_pts < server_pts
    pub fn needs_sync(&self, channel_id: u64, channel_type: u8) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let (local_pts, server_pts) = sdk.get_channel_sync_state(channel_id, channel_type).await?;
                Ok(local_pts < server_pts)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Start supervised synchronization
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `observer`: 同步状态观察者回调
    /// 
    /// # 返回
    /// - `Ok(())`: 启动成功
    pub fn start_supervised_sync(
        self: Arc<Self>,
        observer: Box<dyn SyncObserver>,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        let observer_arc: Arc<dyn SyncObserver> = Arc::from(observer);
        
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                // 将 FFI 层的 SyncObserver 转换为 SDK 层需要的回调函数
                let observer_clone = observer_arc.clone();
                let callback: Arc<dyn Fn(privchat_sdk::events::SyncStatus) + Send + Sync> = Arc::new(move |sdk_status| {
                    use privchat_sdk::events::SyncPhase as SdkPhase;
                    use crate::events::SyncPhase as FfiPhase;
                    
                    // 转换 SyncPhase
                    let ffi_phase = match sdk_status.phase {
                        SdkPhase::Idle => FfiPhase::Idle,
                        SdkPhase::Running => FfiPhase::Running,
                        SdkPhase::BackingOff => FfiPhase::BackingOff,
                        SdkPhase::Error => FfiPhase::Error,
                    };
                    
                    // 转换为 FFI 层的 SyncStatus
                    let ffi_status = crate::events::SyncStatus {
                        phase: ffi_phase,
                        message: sdk_status.message.clone(),
                    };
                    
                    // 调用观察者
                    observer_clone.on_state(ffi_status);
                });
                
                sdk.start_supervised_sync(callback).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Stop supervised synchronization
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 返回
    /// - `Ok(())`: 停止成功
    pub fn stop_supervised_sync(&self) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.stop_supervised_sync().await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Search messages in a channel
    /// 
    /// **类型化返回**：返回 `SearchPage` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `query`: 搜索关键词
    /// - `limit`: 每页数量
    /// - `offset`: 偏移量（可选）
    /// 
    /// # 返回
    /// - `Ok(SearchPage)`: 搜索结果页面
    pub fn search_channel(
        &self,
        channel_id: u64,
        query: String,
        limit: u32,
        offset: Option<u32>,
    ) -> Result<SearchPage, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let page = sdk.search_channel(channel_id, &query, limit, offset).await?;
                Ok(SearchPage {
                    hits: page.hits.iter().map(|hit| SearchHit {
                        channel_id: hit.channel_id,
                        server_message_id: hit.server_message_id,
                        sender: hit.sender,
                        body: hit.body.clone(),
                        timestamp_ms: hit.timestamp_ms,
                    }).collect(),
                    next_offset: page.next_offset,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Set send queue enabled state
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `enabled`: true 表示启用，false 表示禁用
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn send_queue_set_enabled(&self, enabled: bool) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.send_queue_set_enabled(enabled).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Set channel send queue enabled state
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `enabled`: true 表示启用，false 表示禁用
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn channel_send_queue_set_enabled(
        &self,
        channel_id: u64,
        enabled: bool,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.channel_send_queue_set_enabled(channel_id, enabled).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Enqueue text message with transaction ID
    /// 
    /// **类型化返回**：返回 `String` (事务ID) 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `body`: 消息内容
    /// - `txn_id`: 可选的事务ID（如果不提供，将自动生成）
    /// 
    /// # 返回
    /// - `Ok(String)`: 事务ID（可用于重试）
    pub fn enqueue_text(
        &self,
        channel_id: u64,
        body: String,
        txn_id: Option<String>,
    ) -> Result<String, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let txn_id_result = sdk.enqueue_text(channel_id, body, txn_id).await?;
                Ok(txn_id_result)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Retry message by transaction ID
    /// 通过 message.id 重试消息
    ///
    /// 按本地消息主键（message.id）重试失败的消息。
    ///
    /// # 参数
    /// - `message_id`: 本地消息主键（message.id）
    ///
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn retry_message(&self, message_id: u64) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.retry_message(message_id as i64).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }

    /// Logout
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// Clear current user session, disconnect, and clean up related state.
    /// 
    /// # 返回
    /// - `Ok(())`: 登出成功
    pub fn logout(&self) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.logout().await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Enter foreground
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// Called when the app enters foreground, used to restore connection and sync.
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn enter_foreground(&self) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.enter_foreground().await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Enter background
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// Called when the app enters background, used to pause non-critical operations.
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn enter_background(&self) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.enter_background().await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Set channel favourite state
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `favourite`: true 表示收藏，false 表示取消收藏
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn set_channel_favourite(
        &self,
        channel_id: u64,
        favourite: bool,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.set_channel_favourite(channel_id, favourite).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Set channel low priority state
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `low_priority`: true 表示低优先级，false 表示正常优先级
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn set_channel_low_priority(
        &self,
        channel_id: u64,
        low_priority: bool,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.set_channel_low_priority(channel_id, low_priority).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get channel tags (favourite and low_priority)
    /// 
    /// **类型化返回**：返回 `ChannelTags` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// 
    /// # 返回
    /// - `Ok(ChannelTags)`: 会话标签
    pub fn channel_tags(
        &self,
        channel_id: u64,
    ) -> Result<ChannelTags, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let tags = sdk.channel_tags(channel_id).await?;
                Ok(ChannelTags {
                    favourite: tags.favourite,
                    low_priority: tags.low_priority,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Get channel notification mode
    /// 
    /// **类型化返回**：返回 `NotificationMode` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// 
    /// # 返回
    /// - `Ok(NotificationMode)`: 通知模式
    pub fn channel_notification_mode(
        &self,
        channel_id: u64,
    ) -> Result<NotificationMode, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let mode = sdk.channel_notification_mode(channel_id).await?;
                Ok(match mode {
                    privchat_sdk::events::NotificationMode::All => NotificationMode::All,
                    privchat_sdk::events::NotificationMode::Mentions => NotificationMode::Mentions,
                    privchat_sdk::events::NotificationMode::None => NotificationMode::None,
                })
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// Set channel notification mode
    /// 
    /// **类型化返回**：返回 `()` 而不是 JSON 字符串。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// - `mode`: 通知模式
    /// 
    /// # 返回
    /// - `Ok(())`: 操作成功
    pub fn set_channel_notification_mode(
        &self,
        channel_id: u64,
        mode: NotificationMode,
    ) -> Result<(), PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_mode = match mode {
                    NotificationMode::All => privchat_sdk::events::NotificationMode::All,
                    NotificationMode::Mentions => privchat_sdk::events::NotificationMode::Mentions,
                    NotificationMode::None => privchat_sdk::events::NotificationMode::None,
                };
                sdk.set_channel_notification_mode(channel_id, sdk_mode).await?;
                Ok(())
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取我的设备列表
    /// 
    /// 获取当前用户的所有设备列表，包括当前设备和其他已登录设备。
    /// 
    /// # 返回
    /// - `Ok(Vec<DeviceSummary>)`: 设备列表
    /// 
    /// # 示例
    /// ```kotlin
    /// val devices = sdk.listMyDevices()
    /// for (device in devices) {
    ///     println("设备: ${device.deviceId}, 名称: ${device.deviceName}, 当前设备: ${device.isCurrent}")
    /// }
    /// ```
    pub fn list_my_devices(&self) -> Result<Vec<DeviceSummary>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_devices = sdk.list_my_devices().await?;
                
                // 将 SDK 的 DeviceSummary 转换为 FFI 的 DeviceSummary
                let ffi_devices: Vec<DeviceSummary> = sdk_devices
                    .into_iter()
                    .map(|sdk_device| {
                        DeviceSummary {
                            device_id: sdk_device.device_id,
                            device_name: sdk_device.device_name,
                            device_model: sdk_device.device_model,
                            app_id: sdk_device.app_id,
                            device_type: sdk_device.device_type,
                            last_active_at: sdk_device.last_active_at,
                            created_at: sdk_device.created_at,
                            ip_address: sdk_device.ip_address,
                            is_current: sdk_device.is_current,
                        }
                    })
                    .collect();
                
                Ok(ffi_devices)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取私聊会话的对等用户ID
    /// 
    /// 对于私聊会话（channel_type = 1），返回另一个用户的 user_id。
    /// 对于群聊或其他类型的会话，返回 `None`。
    /// 
    /// # 参数
    /// - `channel_id`: 会话ID
    /// 
    /// # 返回
    /// - `Ok(Option<u64>)`: 对等用户ID（如果是私聊），否则返回 `None`
    /// 
    /// # 示例
    /// ```kotlin
    /// val peerUserId = sdk.dmPeerUserId(channelId)
    /// if (peerUserId != null) {
    ///     println("私聊对等用户ID: $peerUserId")
    /// } else {
    ///     println("这不是私聊会话")
    /// }
    /// ```
    pub fn dm_peer_user_id(&self, channel_id: u64) -> Result<Option<u64>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.dm_peer_user_id(channel_id).await
                    .map_err(|e| PrivchatError::from(e))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取消息的反应列表
    /// 
    /// 获取指定消息的所有反应（表情和用户列表）。
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_id`: 消息ID
    /// 
    /// # 返回
    /// - `Ok(Vec<ReactionChip>)`: 反应列表
    /// 
    /// # 示例
    /// ```kotlin
    /// val reactions = sdk.reactions(channelId, messageId)
    /// for (chip in reactions) {
    ///     println("${chip.emoji}: ${chip.count} 个用户")
    /// }
    /// ```
    pub fn reactions(&self, channel_id: u64, message_id: u64) -> Result<Vec<ReactionChip>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_chips = sdk.reactions(channel_id, message_id).await?;
                
                // 将 SDK 的 ReactionChip 转换为 FFI 的 ReactionChip
                let ffi_chips: Vec<ReactionChip> = sdk_chips
                    .into_iter()
                    .map(|sdk_chip| {
                        ReactionChip {
                            emoji: sdk_chip.emoji,
                            user_ids: sdk_chip.user_ids,
                            count: sdk_chip.count as u64,
                        }
                    })
                    .collect();
                
                Ok(ffi_chips)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 批量获取消息的反应列表
    /// 
    /// 一次性获取多个消息的反应列表，提高效率。
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_ids`: 消息ID列表
    /// 
    /// # 返回
    /// - `Ok(HashMap<u64, Vec<ReactionChip>>)`: 消息ID到反应列表的映射
    /// 
    /// # 示例
    /// ```kotlin
    /// val messageIds = listOf(123u, 456u, 789u)
    /// val reactionsMap = sdk.reactionsBatch(channelId, messageIds)
    /// for ((msgId, chips) in reactionsMap) {
    ///     println("消息 $msgId 有 ${chips.size} 个反应")
    /// }
    /// ```
    pub fn reactions_batch(
        &self,
        channel_id: u64,
        message_ids: Vec<u64>,
    ) -> Result<std::collections::HashMap<u64, Vec<ReactionChip>>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_results = sdk.reactions_batch(channel_id, message_ids).await?;
                
                // 将 SDK 的结果转换为 FFI 的结果
                let ffi_results: std::collections::HashMap<u64, Vec<ReactionChip>> = sdk_results
                    .into_iter()
                    .map(|(msg_id, sdk_chips)| {
                        let ffi_chips: Vec<ReactionChip> = sdk_chips
                            .into_iter()
                            .map(|sdk_chip| {
                                ReactionChip {
                                    emoji: sdk_chip.emoji,
                                    user_ids: sdk_chip.user_ids,
                                    count: sdk_chip.count as u64,
                                }
                            })
                            .collect();
                        (msg_id, ffi_chips)
                    })
                    .collect();
                
                Ok(ffi_results)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 检查指定用户是否已读指定消息
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_id`: 消息ID
    /// - `user_id`: 用户ID
    /// 
    /// # 返回
    /// - `Ok(bool)`: true 表示已读，false 表示未读
    /// 
    /// # 示例
    /// ```kotlin
    /// val isRead = sdk.isEventReadBy(channelId, messageId, userId)
    /// if (isRead) {
    ///     println("用户 $userId 已读消息 $messageId")
    /// }
    /// ```
    pub fn is_event_read_by(&self, channel_id: u64, message_id: u64, user_id: u64) -> Result<bool, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                sdk.is_event_read_by(channel_id, message_id, user_id).await
                    .map_err(|e| PrivchatError::from(e))
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
    
    /// 获取已读指定消息的用户列表
    /// 
    /// # 参数
    /// - `channel_id`: 频道ID
    /// - `message_id`: 消息ID
    /// - `limit`: 返回的最大用户数量（可选，默认返回所有）
    /// 
    /// # 返回
    /// - `Ok(Vec<SeenByEntry>)`: 已读用户列表，按已读时间排序
    /// 
    /// # 示例
    /// ```kotlin
    /// val seenBy = sdk.seenByForEvent(channelId, messageId, limit = 10)
    /// for (entry in seenBy) {
    ///     println("用户 ${entry.userId} 在 ${entry.readAt} 已读")
    /// }
    /// ```
    pub fn seen_by_for_event(
        &self,
        channel_id: u64,
        message_id: u64,
        limit: Option<u32>,
    ) -> Result<Vec<SeenByEntry>, PrivchatError> {
        let inner = self.inner.clone();
        get_runtime().block_on(async move {
            let guard = inner.read().await;
            if let Some(sdk) = guard.as_ref() {
                let sdk_entries = sdk.seen_by_for_event(channel_id, message_id, limit).await?;
                
                // 将 SDK 的 SeenByEntry 转换为 FFI 的 SeenByEntry
                let ffi_entries: Vec<SeenByEntry> = sdk_entries
                    .into_iter()
                    .map(|sdk_entry| {
                        SeenByEntry {
                            user_id: sdk_entry.user_id,
                            read_at: sdk_entry.read_at,
                        }
                    })
                    .collect();
                
                Ok(ffi_entries)
            } else {
                Err(PrivchatError::Disconnected)
            }
        })
    }
}

// Private helper methods
impl PrivchatSDK {
    /// 辅助方法：转换 OnlineStatusInfo 为 PresenceEntry
    fn convert_online_status_to_presence_info(
        user_id: u64,
        status: &privchat_protocol::presence::OnlineStatusInfo,
    ) -> PresenceEntry {
        use privchat_protocol::presence::OnlineStatus;
        
        // 判断是否在线（Online 或 Recently 视为在线）
        let is_online = matches!(status.status, OnlineStatus::Online | OnlineStatus::Recently);
        
        // 获取设备类型（取第一个设备）
        let device_type = status.online_devices.first()
            .map(|d| format!("{:?}", d));
        
        PresenceEntry {
            user_id,
            is_online,
            last_seen: Some(status.last_seen),
            device_type,
        }
    }
    async fn do_initialize(
        inner: Arc<RwLock<Option<Arc<privchat_sdk::PrivchatSDK>>>>,
        config: PrivchatConfig,
    ) -> Result<(), PrivchatError> {
        let sdk_config: privchat_sdk::PrivchatConfig = config.try_into()?;
        // Note: initialize() already returns Arc<PrivchatSDK>
        let sdk = privchat_sdk::PrivchatSDK::initialize(sdk_config).await?;
        
        let mut guard = inner.write().await;
        *guard = Some(sdk);
        
        Ok(())
    }
    
    /// Start background network status listener task
    /// Forwards NetworkMonitor events (Online/Offline/Connecting/Limited) to delegate.
    fn start_network_status_listener(
        inner: Arc<RwLock<Option<Arc<privchat_sdk::PrivchatSDK>>>>,
        delegate: Arc<RwLock<Option<Box<dyn PrivchatDelegate>>>>,
    ) -> tokio::task::JoinHandle<()> {
        get_runtime().spawn(async move {
            info!("Network status listener task started");
            let receiver = {
                let guard = inner.read().await;
                guard.as_ref().map(|sdk| sdk.subscribe_network_status())
            };
            let mut receiver = match receiver {
                Some(r) => r,
                None => {
                    debug!("SDK not ready for network subscription");
                    return;
                }
            };
            while let Ok(ev) = receiver.recv().await {
                let delegate_guard = delegate.read().await;
                if let Some(d) = delegate_guard.as_ref() {
                    d.on_network_status_changed(ev.old_status.into(), ev.new_status.into());
                }
            }
            debug!("Network status listener task ended");
        })
    }

    /// Start background event listener task
    /// Uses get_runtime().spawn so we always run on our static runtime, regardless of
    /// which thread the FFI async is executed on (UniFFI/Kotlin may use different threads).
    fn start_event_listener(
        sdk: Arc<privchat_sdk::PrivchatSDK>,
        event_queue: Arc<Mutex<Vec<SDKEvent>>>,
        delegate: Arc<RwLock<Option<Box<dyn PrivchatDelegate>>>>,
        send_observers: Arc<Mutex<HashMap<u64, Arc<dyn SendObserver>>>>,
        timeline_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn TimelineObserver>)>>>,
        channel_list_observers: Arc<Mutex<HashMap<u64, Arc<dyn ChannelListObserver>>>>,
        typing_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn TypingObserver>)>>>,
        receipts_observers: Arc<Mutex<HashMap<u64, (u64, Arc<dyn ReceiptsObserver>)>>>,
    ) -> tokio::task::JoinHandle<()> {
        get_runtime().spawn(async move {
            info!("Event listener task started");
            
            // 订阅 SDK 事件（使用公开的订阅方法）
            let mut receiver = sdk.subscribe_events().await;
            
            let mut last_connection_state = ConnectionState::Disconnected;
            
            loop {
                match receiver.recv().await {
                    Ok(sdk_event) => {
                        debug!("Received SDK event: {}", sdk_event.event_type());
                        
                        // 处理 SendStatusUpdate 事件（按 message.id）
                        if let privchat_sdk::events::SDKEvent::SendStatusUpdate {
                            channel_id,
                            id,
                            state,
                            attempts,
                            error,
                            ..
                        } = &sdk_event {
                            // 转换为 FFI 类型并通知观察者
                            let send_state = match state {
                                privchat_sdk::events::SendStatusState::Enqueued => SendState::Enqueued,
                                privchat_sdk::events::SendStatusState::Sending => SendState::Sending,
                                privchat_sdk::events::SendStatusState::Sent => SendState::Sent,
                                privchat_sdk::events::SendStatusState::Retrying => SendState::Retrying,
                                privchat_sdk::events::SendStatusState::Failed => SendState::Failed,
                            };
                            
                            let ffi_update = SendUpdate {
                                channel_id: *channel_id,
                                id: *id,
                                state: send_state,
                                attempts: *attempts,
                                error: error.clone(),
                            };
                            
                            let guard = send_observers.lock().await;
                            for observer in guard.values() {
                                observer.on_update(ffi_update.clone());
                            }
                        }
                        
                        // 处理 TimelineDiff 事件
                        if let privchat_sdk::events::SDKEvent::TimelineDiff {
                            channel_id,
                            diff_kind,
                            ..
                        } = &sdk_event {
                            use privchat_sdk::events::TimelineDiffKind as SdkDiffKind;
                            
                            let ffi_diff = match diff_kind {
                                SdkDiffKind::Reset { messages } => {
                                    TimelineDiffKind::Reset {
                                        values: messages.iter().map(|m| Self::convert_timeline_message(m)).collect(),
                                    }
                                }
                                SdkDiffKind::Append { messages } => {
                                    TimelineDiffKind::Append {
                                        values: messages.iter().map(|m| Self::convert_timeline_message(m)).collect(),
                                    }
                                }
                                SdkDiffKind::UpdateByItemId { item_id, message } => {
                                    TimelineDiffKind::UpdateByItemId {
                                        item_id: *item_id,
                                        value: Self::convert_timeline_message(message),
                                    }
                                }
                                SdkDiffKind::RemoveByItemId { item_id } => {
                                    TimelineDiffKind::RemoveByItemId {
                                        item_id: *item_id,
                                    }
                                }
                            };
                            
                            let guard = timeline_observers.lock().await;
                            for (obs_channel_id, observer) in guard.values() {
                                if *obs_channel_id == *channel_id {
                                    observer.on_diff(ffi_diff.clone());
                                }
                            }
                        }
                        
                        // 处理 TypingIndicator 事件
                        if let privchat_sdk::events::SDKEvent::TypingIndicator(typing_event) = &sdk_event {
                            let ffi_event = TypingIndicatorEvent {
                                channel_id: typing_event.channel_id,
                                user_id: typing_event.user_id,
                                is_typing: typing_event.is_typing,
                            };
                            
                            let guard = typing_observers.lock().await;
                            for (obs_channel_id, observer) in guard.values() {
                                if *obs_channel_id == typing_event.channel_id {
                                    observer.on_typing(ffi_event.clone());
                                }
                            }
                        }
                        
                        // 处理 ReadReceiptReceived 事件
                        if let privchat_sdk::events::SDKEvent::ReadReceiptReceived(receipt_event) = &sdk_event {
                            let ffi_event = ReadReceiptEvent {
                                channel_id: receipt_event.channel_id,
                                server_message_id: receipt_event.message_id,
                                reader_uid: receipt_event.reader_uid,
                                timestamp: receipt_event.read_at,
                            };
                            
                            let guard = receipts_observers.lock().await;
                            for (obs_channel_id, observer) in guard.values() {
                                if *obs_channel_id == receipt_event.channel_id {
                                    observer.on_receipt(ffi_event.clone());
                                }
                            }
                        }
                        
                        // 处理 ChannelListUpdate 事件
                        if let privchat_sdk::events::SDKEvent::ChannelListUpdate {
                            update_kind,
                            ..
                        } = &sdk_event {
                            use privchat_sdk::events::ChannelListUpdateKind as SdkUpdateKind;
                            
                            match update_kind {
                                SdkUpdateKind::Reset { channels } => {
                                    let ffi_entries: Vec<ChannelListEntry> = channels.iter()
                                        .map(|c| Self::convert_sdk_channel_list_entry_to_ffi(c))
                                        .collect();
                                    let guard = channel_list_observers.lock().await;
                                    for observer in guard.values() {
                                        observer.on_reset(ffi_entries.clone());
                                    }
                                }
                                SdkUpdateKind::Update { channel } => {
                                    let ffi_entry = Self::convert_sdk_channel_list_entry_to_ffi(channel);
                                    let guard = channel_list_observers.lock().await;
                                    for observer in guard.values() {
                                        observer.on_update(ffi_entry.clone());
                                    }
                                }
                                SdkUpdateKind::Remove { channel_id: _ } => {
                                    // 对于删除，可以发送一个空的更新，或者让 UI 层处理
                                    // 这里暂时不处理，UI 层可以通过 on_reset 重新加载
                                }
                            }
                        }
                        
                        // Convert SDK event to FFI event
                        if let Some(ffi_event) = Self::convert_sdk_event(&sdk_event) {
                            // 1. Add to queue for polling
                            {
                                let mut queue = event_queue.lock().await;
                                queue.push(ffi_event.clone());
                                
                                // Limit queue size to prevent memory issues
                                const MAX_QUEUE_SIZE: usize = 1000;
                                if queue.len() > MAX_QUEUE_SIZE {
                                    queue.remove(0);
                                    debug!("Event queue full, dropped oldest event");
                                }
                            }
                            
                            // 2. Call delegate if set
                            let delegate_guard = delegate.read().await;
                            if let Some(ref d) = *delegate_guard {
                                Self::dispatch_event_to_delegate(
                                    d.as_ref(),
                                    &sdk_event,
                                    &ffi_event,
                                    &mut last_connection_state,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        error!("Event listener error: {:?}", e);
                        // On broadcast error, try to recover by resubscribing
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        break;
                    }
                }
            }
            
            info!("Event listener task ended");
        })
    }
    
    /// Dispatch event to delegate callback
    /// 
    /// UniFFI 0.31: Type-safe callbacks - no JSON serialization needed
    /// This method is called from the event listener task, which has access to the SDK.
    fn dispatch_event_to_delegate(
        delegate: &dyn PrivchatDelegate,
        sdk_event: &privchat_sdk::events::SDKEvent,
        ffi_event: &SDKEvent,
        _last_connection_state: &mut ConnectionState,
    ) {
        use privchat_sdk::events::SDKEvent as SdkEvent;
        use crate::events::{MessageEntry, MessageStatus};
        
        // Dispatch specific event types to specialized callbacks
        match sdk_event {
            SdkEvent::MessageReceived { server_message_id, channel_id, channel_type, from_uid, timestamp, content } => {
                // ✅ 现在 MessageReceived 事件包含 content 字段
                // 确保时间戳是毫秒级（如果小于 1e12，认为是秒级，需要乘以 1000）
                let timestamp_ms = if *timestamp < 1_000_000_000_000u64 {
                    // 秒级时间戳，转换为毫秒
                    *timestamp * 1000
                } else {
                    // 已经是毫秒级
                    *timestamp
                };
                
                debug!("[FFI] 📥 收到消息事件: server_message_id={}, content={}, timestamp={} (原始={})", 
                       *server_message_id, 
                       content.chars().take(50).collect::<String>(),
                       timestamp_ms,
                       *timestamp);
                
                let message = MessageEntry {
                    id: *server_message_id,
                    server_message_id: Some(*server_message_id),
                    channel_id: *channel_id,
                    channel_type: *channel_type,
                    from_uid: *from_uid,
                    content: content.clone(),
                    status: MessageStatus::Sent,
                    timestamp: timestamp_ms,
                };
                
                delegate.on_message_received(message);
            }
            
            SdkEvent::ConnectionStateChanged { new_state, .. } => {
                use privchat_sdk::events::ConnectionState as SdkConnState;
                use crate::events::ConnectionState as FfiConnState;
                
                // UniFFI 0.31: Type-safe ConnectionState enum
                let state = match new_state {
                    SdkConnState::Disconnected => FfiConnState::Disconnected,
                    SdkConnState::Connecting => FfiConnState::Connecting,
                    SdkConnState::Connected => FfiConnState::Connected,
                    SdkConnState::Reconnecting => FfiConnState::Reconnecting,
                };
                
                delegate.on_connection_state_changed(state);
            }
            
            _ => {
                // UniFFI 0.31: Type-safe SDKEvent object
                delegate.on_event(ffi_event.clone());
            }
        }
    }
    
    /// Convert SDK event to FFI event
    fn convert_sdk_event(sdk_event: &privchat_sdk::events::SDKEvent) -> Option<SDKEvent> {
        use crate::events::EventType;
        use privchat_sdk::events::SDKEvent as SdkEvent;
        
        let event_type = match sdk_event {
            SdkEvent::MessageReceived { .. } => EventType::MessageReceived,
            SdkEvent::MessageSent { .. } => EventType::MessageSent,
            SdkEvent::MessageSendFailed { .. } => EventType::MessageFailed,
            SdkEvent::ConnectionStateChanged { .. } => EventType::ConnectionStateChanged,
            SdkEvent::TypingIndicator(_) | SdkEvent::TypingStarted(_) | SdkEvent::TypingStopped(_) => {
                EventType::TypingIndicator
            }
            SdkEvent::ReadReceiptReceived(_) => EventType::ReadReceipt,
            SdkEvent::UserPresenceChanged { .. } => EventType::UserPresenceChanged,
            _ => {
                // Unsupported event types for now
                return None;
            }
        };
        
        // Serialize event data as JSON
        let data = match serde_json::to_string(sdk_event) {
            Ok(json) => json,
            Err(e) => {
                error!("Failed to serialize event: {:?}", e);
                return None;
            }
        };
        
        Some(SDKEvent {
            event_type,
            data,
            timestamp: sdk_event.timestamp(),
        })
    }
    
    /// Observe message send status updates
    /// 
    /// Returns a token that can be used to unsubscribe later.
    /// This is the only place where `local_message_id` is exposed (transport layer).
    /// 
    /// **FFI 层实现**：简单注册观察者，事件监听任务会自动处理 SendStatusUpdate 事件。
    /// 业务逻辑在 SDK 层（通过 EventManager），FFI 层只做类型转换和转发。
    pub fn observe_sends(
        self: Arc<Self>,
        observer: Box<dyn SendObserver>,
    ) -> u64 {
        let token = self.send_observer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let observers = self.send_observers.clone();
        
        get_runtime().block_on(async move {
            observers.lock().await.insert(token, Arc::from(observer));
        });
        
        info!("Send observer registered with token: {}", token);
        token
    }
    
    /// Unobserve message send status updates
    /// 
    /// Removes the observer identified by the token.
    pub fn unobserve_sends(
        self: Arc<Self>,
        token: u64,
    ) -> bool {
        let observers = self.send_observers.clone();
        let removed = get_runtime().block_on(async move {
            observers.lock().await.remove(&token).is_some()
        });
        
        if removed {
            info!("Send observer unregistered with token: {}", token);
        } else {
            debug!("Send observer token {} not found", token);
        }
        
        removed
    }
    
    /// Observe timeline updates for a specific channel
    /// 
    /// Returns a token that can be used to unsubscribe later.
    /// **FFI 层实现**：简单注册观察者，事件监听任务会自动处理 TimelineDiff 事件。
    pub fn observe_timeline(
        self: Arc<Self>,
        channel_id: u64,
        observer: Box<dyn TimelineObserver>,
    ) -> u64 {
        let token = self.timeline_observer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let observers = self.timeline_observers.clone();
        
        get_runtime().block_on(async move {
            let mut guard = observers.lock().await;
            guard.insert(token, (channel_id, Arc::from(observer)));
        });
        
        info!("Timeline observer registered with token: {} for channel: {}", token, channel_id);
        token
    }
    
    /// Unobserve timeline updates
    pub fn unobserve_timeline(
        self: Arc<Self>,
        token: u64,
    ) -> bool {
        let observers = self.timeline_observers.clone();
        let removed = get_runtime().block_on(async move {
            observers.lock().await.remove(&token).is_some()
        });
        
        if removed {
            info!("Timeline observer unregistered with token: {}", token);
        } else {
            debug!("Timeline observer token {} not found", token);
        }
        
        removed
    }
    
    /// Observe channel list updates
    /// 
    /// Returns a token that can be used to unsubscribe later.
    /// **FFI 层实现**：简单注册观察者，事件监听任务会自动处理 ChannelListUpdate 事件。
    pub fn observe_channel_list(
        self: Arc<Self>,
        observer: Box<dyn ChannelListObserver>,
    ) -> u64 {
        let token = self.channel_list_observer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let observers = self.channel_list_observers.clone();
        
        get_runtime().block_on(async move {
            observers.lock().await.insert(token, Arc::from(observer));
        });
        
        info!("Channel list observer registered with token: {}", token);
        token
    }
    
    /// Unobserve channel list updates
    pub fn unobserve_channel_list(
        self: Arc<Self>,
        token: u64,
    ) -> bool {
        let observers = self.channel_list_observers.clone();
        let removed = get_runtime().block_on(async move {
            observers.lock().await.remove(&token).is_some()
        });
        
        if removed {
            info!("Channel list observer unregistered with token: {}", token);
        } else {
            debug!("Channel list observer token {} not found", token);
        }
        
        removed
    }
    
    /// Observe typing indicator updates for a specific channel
    /// 
    /// Returns a token that can be used to unsubscribe later.
    /// **FFI 层实现**：简单注册观察者，事件监听任务会自动处理 TypingIndicator 事件。
    pub fn observe_typing(
        self: Arc<Self>,
        channel_id: u64,
        observer: Box<dyn TypingObserver>,
    ) -> u64 {
        let token = self.typing_observer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let observers = self.typing_observers.clone();
        
        get_runtime().block_on(async move {
            let mut guard = observers.lock().await;
            guard.insert(token, (channel_id, Arc::from(observer)));
        });
        
        info!("Typing observer registered with token: {} for channel: {}", token, channel_id);
        token
    }
    
    /// Unobserve typing indicator updates
    pub fn unobserve_typing(
        self: Arc<Self>,
        token: u64,
    ) -> bool {
        let observers = self.typing_observers.clone();
        let removed = get_runtime().block_on(async move {
            observers.lock().await.remove(&token).is_some()
        });
        
        if removed {
            info!("Typing observer unregistered with token: {}", token);
        } else {
            debug!("Typing observer token {} not found", token);
        }
        
        removed
    }
    
    /// Observe read receipt updates for a specific channel
    /// 
    /// Returns a token that can be used to unsubscribe later.
    /// **FFI 层实现**：简单注册观察者，事件监听任务会自动处理 ReadReceiptReceived 事件。
    pub fn observe_receipts(
        self: Arc<Self>,
        channel_id: u64,
        observer: Box<dyn ReceiptsObserver>,
    ) -> u64 {
        let token = self.receipts_observer_counter.fetch_add(1, Ordering::Relaxed) + 1;
        let observers = self.receipts_observers.clone();
        
        get_runtime().block_on(async move {
            let mut guard = observers.lock().await;
            guard.insert(token, (channel_id, Arc::from(observer)));
        });
        
        info!("Receipts observer registered with token: {} for channel: {}", token, channel_id);
        token
    }
    
    /// Unobserve read receipt updates
    pub fn unobserve_receipts(
        self: Arc<Self>,
        token: u64,
    ) -> bool {
        let observers = self.receipts_observers.clone();
        let removed = get_runtime().block_on(async move {
            observers.lock().await.remove(&token).is_some()
        });
        
        if removed {
            info!("Receipts observer unregistered with token: {}", token);
        } else {
            debug!("Receipts observer token {} not found", token);
        }
        
        removed
    }
    
    /// 辅助方法：转换 TimelineMessage 为 FFI Message（客户端 id 为 message.id）
    fn convert_timeline_message(msg: &privchat_sdk::events::TimelineMessage) -> MessageEntry {
        use crate::events::MessageStatus;
        MessageEntry {
            id: msg.id,
            server_message_id: msg.server_message_id,
            channel_id: msg.channel_id,
            channel_type: msg.channel_type,
            from_uid: msg.from_uid,
            content: msg.content.clone(),
            status: MessageStatus::Sent,
            timestamp: msg.timestamp,
        }
    }
    
    /// 辅助方法：转换 SDK Message 为 FFI Message
    /// client_id 为 message.id（客户端唯一标识，用于 itemId/游标）
    fn convert_sdk_message_to_ffi(
        sdk_msg: &privchat_sdk::storage::entities::Message,
        client_id: u64,
    ) -> MessageEntry {
        use crate::events::MessageStatus;
        
        let status = match sdk_msg.status {
            0 => MessageStatus::Pending,
            1 => MessageStatus::Sending,
            2 => MessageStatus::Sent,
            3 => MessageStatus::Sent,
            4 => MessageStatus::Read,
            5 => MessageStatus::Failed,
            6 => MessageStatus::Failed,
            7 => MessageStatus::Failed,
            8 => MessageStatus::Sending,
            9 => MessageStatus::Failed,
            10 => MessageStatus::Sent,
            _ => MessageStatus::Sent,
        };
        
        let timestamp_ms = sdk_msg.timestamp
            .filter(|&ts| ts > 0)
            .unwrap_or_else(|| {
                if sdk_msg.created_at > 0 {
                    sdk_msg.created_at
                } else {
                    use std::time::{SystemTime, UNIX_EPOCH};
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as i64
                }
            }) as u64;
        
        MessageEntry {
            id: client_id,
            server_message_id: sdk_msg.server_message_id,
            channel_id: sdk_msg.channel_id,
            channel_type: sdk_msg.channel_type,
            from_uid: sdk_msg.from_uid,
            content: sdk_msg.content.clone(),
            status,
            timestamp: timestamp_ms,
        }
    }
    
    /// 辅助方法：转换 SDK ChannelListEntry 为 FFI ChannelListEntry
    fn convert_sdk_channel_list_entry_to_ffi(
        entry: &privchat_sdk::events::ChannelListEntry,
    ) -> ChannelListEntry {
        ChannelListEntry {
            channel_id: entry.channel_id,
            channel_type: entry.channel_type,
            name: entry.name.clone(),
            last_ts: entry.last_ts,
            notifications: entry.notifications,
            messages: entry.messages,
            mentions: entry.mentions,
            marked_unread: entry.marked_unread,
            is_favourite: entry.is_favourite,
            is_low_priority: entry.is_low_priority,
            avatar_url: entry.avatar_url.clone(),
            is_dm: entry.is_dm,
            is_encrypted: entry.is_encrypted,
            member_count: entry.member_count,
            topic: entry.topic.clone(),
            latest_event: entry.latest_event.as_ref().map(|e| LatestChannelEvent {
                event_type: e.event_type.clone(),
                content: e.content.clone(),
                timestamp: e.timestamp,
            }),
        }
    }
    
}
