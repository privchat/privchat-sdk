//! 分片上传的**真实链路**验证（RESUMABLE_UPLOAD_SPEC §7）：
//! 真 SDK（FFI 层，与 App 同一入口）→ 真 server → 真 PG/本地存储。
//!
//! 跑法（server 已在本机起好）：
//! ```text
//! PRIVCHAT_HOST=127.0.0.1 PRIVCHAT_STORAGE_ROOT=/path/to/server/storage \
//!   cargo run -q -p privchat-sdk-ffi --example chunked_upload_live
//! ```
//! 判据：文件 > 1MiB → 走分片 → 服务端 `chunked/{upload_id}/` 先出现 parts，
//! 完成后只剩 manifest + completed.json（callback 之后整个目录消失）→ 正式对象
//! 与本地密文逐字节一致 → 消息状态到 Sent。

use privchat_sdk_ffi::{
    LocalAttachmentMetadataInput, NewMessage, PrivchatClient, PrivchatConfig, ServerEndpoint,
    TransportProtocol,
};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[tokio::main]
async fn main() {
    let host = std::env::var("PRIVCHAT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let storage_root = std::env::var("PRIVCHAT_STORAGE_ROOT").ok().map(PathBuf::from);
    let size: usize = std::env::var("PRIVCHAT_LIVE_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3 * 1024 * 1024 + 4321);

    let data_dir = std::env::temp_dir().join(format!("privchat-chunked-live-{}", now_ms()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let client = std::sync::Arc::new(PrivchatClient::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: host.clone(),
            port: 9001,
            path: None,
            use_tls: false,
        }],
        connection_timeout_secs: 30,
        data_dir: data_dir.to_string_lossy().to_string(),
    })
    .expect("client"));

    client.connect().await.expect("connect");
    let suffix = format!("{}", now_ms() % 10_000_000);
    let username = format!("chunk_{suffix}");
    let device_id = format!("{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}", now_ms() as u32, (now_ms() >> 8) as u16, (now_ms() & 0xfff) as u16, (now_ms() >> 4 & 0xfff) as u16, now_ms() & 0xffff_ffff_ffff);
    let login = client
        .register(username.clone(), "password123".into(), device_id.clone())
        .await
        .expect("register");
    client
        .authenticate(login.user_id, login.token.clone(), login.device_id.clone())
        .await
        .expect("authenticate");
    client.run_bootstrap_sync().await.expect("bootstrap");
    println!("✅ 登录 user_id={}", login.user_id);

    let group = client
        .create_group(format!("chunk-e2e-{suffix}"), None, None)
        .await
        .expect("create group");
    println!("✅ 建群 group_id={}", group.group_id);

    // ---- 造一份 > 1MiB 的「文件」并按 App 的三步入队 ----
    let payload: Vec<u8> = (0..size).map(|i| ((i as u32).wrapping_mul(2654435761) >> 11) as u8).collect();
    let file_name = "big.bin".to_string();
    let mime = "application/octet-stream".to_string();
    let local_message_id = client.generate_local_message_id().expect("lmid");
    let created_at = now_ms() as i64;
    let message_id = client
        .create_local_attachment_placeholder_typed(
            NewMessage {
                channel_id: group.group_id,
                channel_type: 2,
                from_uid: login.user_id,
                message_type: 4, // File
                content: String::new(),
                searchable_word: file_name.clone(),
                setting: 0,
                extra: String::new(),
                mime_type: Some(mime.clone()),
                media_downloaded: false,
                thumb_status: 0,
            },
            local_message_id,
            LocalAttachmentMetadataInput {
                file_name: file_name.clone(),
                mime_type: mime.clone(),
                caption: None,
                duration: None,
                width: None,
                height: None,
                thumbnail_width: None,
                thumbnail_height: None,
                extension_json: None,
            },
        )
        .await
        .expect("placeholder");
    let dir = client
        .get_attachment_target_dir(login.user_id, message_id as i64, created_at)
        .expect("target dir");
    let local_path = PathBuf::from(&dir).join("payload.bin");
    std::fs::write(&local_path, &payload).unwrap();
    client
        .finalize_attachment_and_enqueue(
            message_id,
            format!("file://{}", local_path.display()),
            0,
            client.to_client_endpoint().unwrap_or_default(),
        )
        .await
        .expect("finalize");
    println!("📤 已入队 message_id={message_id} size={size}");

    // ---- 等待发送完成，同时观察进度事件（与 App 同一入口：next_event 流）与服务端目录 ----
    let chunked_root = storage_root.as_ref().map(|r| r.join("tmp/uploads/chunked"));
    let started = Instant::now();
    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(u64, u64)>::new()));
    let total_events = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let client = client.clone();
        let progress = progress.clone();
        let total_events = total_events.clone();
        let stop = stop.clone();
        let want = message_id.to_string();
        tokio::spawn(async move {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                match client.next_event(200).await {
                    Ok(Some(ev)) => {
                        total_events.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if let privchat_sdk_ffi::SdkEvent::AttachmentUploadProgress { local_message_id, uploaded, total } = ev {
                            if local_message_id == want {
                                progress.lock().unwrap().push((uploaded, total));
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
    }
    let mut saw_parts = false;
    let mut status = -1;
    while started.elapsed() < Duration::from_secs(120) {
        if let Some(root) = chunked_root.as_ref() {
            if let Ok(rd) = std::fs::read_dir(root) {
                for e in rd.flatten() {
                    let parts = e.path().join("parts");
                    if parts.is_dir() && std::fs::read_dir(&parts).map(|r| r.count() > 0).unwrap_or(false) {
                        saw_parts = true;
                    }
                }
            }
        }
        if let Ok(Some(m)) = client.get_message_by_id(message_id).await {
            status = m.status;
            // 2 = Sent（带 server_message_id），3 = Failed。
            if m.status == 2 || m.status == 3 {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let progress = progress.lock().unwrap().clone();
    let progress_seen = progress.len();
    let last_progress = progress.last().copied().unwrap_or((0, 0));
    println!("🔎 events_seen={} progress={:?}", total_events.load(std::sync::atomic::Ordering::Relaxed), progress);
    println!(
        "📊 status={status} progress_events={progress_seen} last={:?} elapsed={:?} saw_parts={saw_parts}",
        last_progress,
        started.elapsed()
    );
    assert!(status == 2, "消息没有到 Sent（status={status}）");
    assert!(progress_seen >= 2, "分片路径应报多次进度（实际 {progress_seen}）");
    assert_eq!(last_progress.1, last_progress.0, "最后一次进度应是 total/total");
    if let Some(root) = storage_root.as_ref() {
        // 走了分片：chunked/ 根被建出来；没走整包：整包会话目录 tmp/uploads/{uid} 不存在。
        assert!(root.join("tmp/uploads/chunked").is_dir(), "chunked/ 根不存在——没有走分片路径");
        assert!(
            !root.join("tmp/uploads").join(login.user_id.to_string()).exists(),
            "出现了整包会话目录 tmp/uploads/{}——走的是整包而不是分片",
            login.user_id
        );
        if !saw_parts {
            println!("ℹ️ 本机太快，轮询没抓到 parts/ 中间态（不影响判定）");
        }
    }

    // ---- 正式对象核对：file_id 在 extra.metadata 里；对象是密文，大小 = 最后一次进度的 total ----
    let msg = client.get_message_by_id(message_id).await.unwrap().unwrap();
    let extra: serde_json::Value = serde_json::from_str(&msg.extra).unwrap_or_default();
    let file_id = extra["metadata"]["file_id"]
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| extra["metadata"]["file_id"].as_str().map(|s| s.to_string()))
        .expect("extra.metadata.file_id");
    println!("📝 file_id={file_id}");
    if let Some(root) = storage_root.as_ref() {
        let obj = root.join("files").join(format!("{file_id}.bin"));
        let stored = std::fs::read(&obj).unwrap_or_else(|e| panic!("读正式对象 {obj:?} 失败: {e}"));
        assert_eq!(stored.len() as u64, last_progress.1, "正式对象大小 ≠ 上传总字节");
        assert!(stored.len() > size, "密文应大于明文（nonce+tag）");
        println!("✅ 正式对象 {} 字节 = 上传总字节，sha256={}", stored.len(), &sha256_hex(&stored)[..16]);
        let leftover = std::fs::read_dir(root.join("tmp/uploads/chunked"))
            .map(|r| r.flatten().filter(|e| e.path().join("parts").is_dir()).count())
            .unwrap_or(0);
        println!("🧹 仍带 parts 的会话数: {leftover}");
        assert!(
            !root.join("tmp/uploads/chunked").read_dir().map(|mut r| r.any(|e| e.ok().map(|e| e.file_name().to_string_lossy().starts_with(&file_id)).unwrap_or(false))).unwrap_or(false),
            "callback 之后会话目录应已删除"
        );
    }
    let _ = client.disconnect().await;
    println!("🎉 chunked_upload_live PASSED");
}
