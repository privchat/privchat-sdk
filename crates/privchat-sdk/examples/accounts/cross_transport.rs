//! One isolated server, two native transports and an actual browser peer.
use crate::account_manager::{MultiAccountManager, GROUP_SYNC_CHANNEL_TYPE};
use crate::{boxed_err, phases::TestPhases, BoxResult};
use privchat_sdk::{NewMessage, StoredMessage};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};

const GROUP: u8 = GROUP_SYNC_CHANNEL_TYPE;

fn write(dir: &Path, name: &str, value: Value) -> BoxResult<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec(&value)?)?;
    std::fs::rename(tmp, dir.join(name))?;
    Ok(())
}

async fn read(dir: &Path, name: &str) -> BoxResult<Value> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if dir.join("browser-error").is_file() {
            return Err(boxed_err("browser assertion failed; inspect browser log"));
        }
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            return Ok(serde_json::from_slice(&bytes)?);
        }
        if Instant::now() >= deadline {
            return Err(boxed_err(format!("timeout waiting for {name}")));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn message(
    manager: &MultiAccountManager,
    account: &str,
    channel: u64,
    predicate: impl Fn(&StoredMessage) -> bool,
) -> BoxResult<StoredMessage> {
    let sdk = manager.sdk(account)?;
    let reader = sdk
        .local_reader()
        .await?
        .ok_or_else(|| boxed_err("no local reader"))?;
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let rows = reader.messages(channel, GROUP as i32, 100, 0).await?;
        let mut hits = rows.into_iter().filter(&predicate);
        if let Some(hit) = hits.next() {
            if hits.next().is_some() {
                return Err(boxed_err("duplicate local projection"));
            }
            return Ok(hit);
        }
        if Instant::now() >= deadline {
            return Err(boxed_err(format!("missing local message for {account}")));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

pub(crate) async fn run(manager: &mut MultiAccountManager, dir: &Path) -> BoxResult<()> {
    if !TestPhases::phase2_friend_system(manager).await?.success {
        return Err(boxed_err("failed to set up fixture friends"));
    }
    let bob = manager.user_id("bob")?;
    let browser = manager.account_config("charlie")?;
    let group = manager
        .create_group("alice", "three transports", vec![bob, browser.user_id])
        .await?;
    let id = group.group_id;
    manager.sdk("charlie")?.disconnect().await?;
    for key in ["alice", "bob"] {
        manager.refresh_local_views(key).await?;
    }
    write(
        dir,
        "fixture",
        json!({
            "group": id.to_string(), "uid": browser.user_id.to_string(),
            "device": browser.device_id, "token": browser.token,
        }),
    )?;
    read(dir, "browser-ready").await?;
    if !manager.sdk("alice")?.is_connected().await? || !manager.sdk("bob")?.is_connected().await? {
        return Err(boxed_err(
            "both fixed native transports must stay connected",
        ));
    }

    let q = manager
        .send_text("alice", id, GROUP, "sd12-from-quic")
        .await?;
    let t = manager.send_text("bob", id, GROUP, "sd12-from-tcp").await?;
    let qid = q
        .server_msg_id
        .ok_or_else(|| boxed_err("QUIC send refused"))?;
    let tid = t
        .server_msg_id
        .ok_or_else(|| boxed_err("TCP send refused"))?;
    message(manager, "bob", id, |row| row.server_message_id == Some(qid)).await?;
    message(manager, "alice", id, |row| {
        row.server_message_id == Some(tid)
    })
    .await?;
    write(
        dir,
        "online",
        json!({ "quic": qid.to_string(), "tcp": tid.to_string() }),
    )?;
    let w = read(dir, "browser-sent").await?["id"]
        .as_str()
        .ok_or_else(|| boxed_err("missing browser server message ID"))?
        .parse::<u64>()?;
    for key in ["alice", "bob"] {
        message(manager, key, id, |row| row.server_message_id == Some(w)).await?;
    }
    println!("[PASS] 3 transports interoperate in one group");

    let path = manager.base_dir.join("cross-transport.png");
    image::RgbImage::from_fn(96, 64, |x, y| image::Rgb([x as u8, y as u8, 160])).save(&path)?;
    std::fs::copy(&path, dir.join("attachment.png"))?;
    let alice = manager.sdk("alice")?;
    let local = alice
        .create_local_attachment_placeholder(
            NewMessage {
                channel_id: id,
                channel_type: GROUP as i32,
                from_uid: manager.user_id("alice")?,
                message_type: privchat_protocol::ContentMessageType::Image as i32,
                content: path.display().to_string(),
                searchable_word: String::new(),
                setting: 0,
                extra: String::new(),
                mime_type: Some("image/png".into()),
                media_downloaded: true,
                thumb_status: 0,
            },
            None,
        )
        .await?;
    alice
        .finalize_attachment_and_enqueue(
            local,
            path.display().to_string(),
            0,
            "image".into(),
            vec![],
        )
        .await?;
    let sent = message(manager, "alice", id, |row| {
        row.message_id == local && row.status == 2
    })
    .await?;
    let image_id = sent
        .server_message_id
        .ok_or_else(|| boxed_err("missing attachment message ID"))?;
    let peer = message(manager, "bob", id, |row| {
        row.server_message_id == Some(image_id)
    })
    .await?;
    let metadata: Value = serde_json::from_str(&peer.extra)?;
    let file_id = metadata["metadata"]["file_id"]
        .as_u64()
        .ok_or_else(|| boxed_err("missing file_id"))?;
    write(
        dir,
        "attachment",
        json!({"message": image_id.to_string(), "file": file_id.to_string()}),
    )?;
    read(dir, "browser-offline").await?;
    println!("[PASS] attachment ordinary send; browser downloaded identical bytes");

    manager.message_revoke("alice", id, qid).await?;
    message(manager, "bob", id, |row| {
        row.server_message_id == Some(qid) && row.revoked
    })
    .await?;
    let offline = manager.send_text("bob", id, GROUP, "sd12-offline").await?;
    let offline_id = offline
        .server_msg_id
        .ok_or_else(|| boxed_err("offline message not committed"))?;
    let pts = offline
        .pts
        .ok_or_else(|| boxed_err("offline message has no PTS"))?;
    write(
        dir,
        "resume",
        json!({ "revoked": qid.to_string(), "message": offline_id.to_string(), "pts": pts.to_string() }),
    )?;
    read(dir, "browser-read").await?;
    let stats = manager.message_read_stats("bob", id, offline_id).await?;
    if stats.read_count != 1 || stats.recipient_count != 2 {
        return Err(boxed_err(format!("wrong server read stats: {stats:?}")));
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let extra = manager
            .get_local_channel_extra("bob", id, GROUP as i32)
            .await?;
        if extra.is_some_and(|value| value.peer_read_pts >= pts) {
            break;
        }
        if Instant::now() >= deadline {
            return Err(boxed_err("missing local peer read cursor"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("[PASS] WebSocket reconnect filled missing message and revoke; read converged");
    Ok(())
}
