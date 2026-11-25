//! Isolated process-death / lost ACK acceptance. No server test hook is used.
use crate::account_manager::{MultiAccountManager, DIRECT_SYNC_CHANNEL_TYPE};
use crate::BoxResult;
use privchat_sdk::{PrivchatConfig, PrivchatSdk, ServerEndpoint, TransportProtocol};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

#[derive(Serialize, Deserialize)]
struct Fixture {
    channel_id: u64,
    user_id: u64,
    username: String,
    password: String,
    device_id: String,
    tag: String,
    data_dir: PathBuf,
}

fn port(key: &str) -> BoxResult<u16> {
    Ok(std::env::var(key)?.parse()?)
}

fn signal(dir: &Path, name: &str, value: impl AsRef<[u8]>) -> BoxResult<()> {
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, value)?;
    std::fs::rename(tmp, dir.join(name))?;
    Ok(())
}

async fn await_marker(dir: &Path, name: &str) -> BoxResult<String> {
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            match std::fs::read_to_string(dir.join(name)) {
                Ok(value) => return Ok(value),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| std::io::Error::other(format!("timed out waiting for {name}")))?
}

struct ScopedChild(Child);

impl ScopedChild {
    fn kill_and_wait(&mut self) -> BoxResult<()> {
        self.0.kill()?;
        self.0.wait()?;
        Ok(())
    }
}

impl Drop for ScopedChild {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn_child(dir: &Path, mode: &str) -> BoxResult<ScopedChild> {
    Ok(ScopedChild(
        Command::new(std::env::current_exe()?)
            .env("PRIVCHAT_RESTART_FAULT_MODE", mode)
            .env("PRIVCHAT_RESTART_FAULT_DIR", dir)
            .spawn()?,
    ))
}

pub async fn run(manager: &MultiAccountManager, dir: &Path) -> BoxResult<()> {
    let alice = manager.account_config("alice")?;
    let channel_id = manager
        .cached_direct_channel("alice", "bob")
        .ok_or_else(|| std::io::Error::other("fault gate requires phase2"))?;
    let tag = format!(
        "restart-ack-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis()
    );
    let fixture = Fixture {
        channel_id,
        user_id: alice.user_id,
        username: alice.username,
        password: alice.password,
        device_id: privchat_sdk::new_device_id(),
        tag,
        data_dir: dir.join("device"),
    };
    signal(dir, "fixture", serde_json::to_vec(&fixture)?)?;
    let mut sender = spawn_child(dir, "send")?;
    await_marker(dir, "sender-ready").await?;
    // The proxy acknowledges that downstream is blackholed before enqueue.
    signal(dir, "arm", "")?;
    await_marker(dir, "armed").await?;
    signal(dir, "enqueue", "")?;
    await_marker(dir, "sender-pending").await?;
    sender.kill_and_wait()?;
    signal(dir, "restart-server", "")?;
    let committed_id: u64 = await_marker(dir, "server-restarted")
        .await?
        .trim()
        .parse()?;
    let mut recovered = spawn_child(dir, "recover")?;
    let resumed_id: u64 = await_marker(dir, "sender-recovered")
        .await?
        .trim()
        .parse()?;
    if committed_id != resumed_id {
        return Err(
            std::io::Error::other("recovered sender received a different message ID").into(),
        );
    }
    if !recovered.0.wait()?.success() {
        return Err(std::io::Error::other("recovery child failed to exit cleanly").into());
    }

    let bob = manager.sdk("bob")?;
    let bob_cfg = manager.account_config("bob")?;
    bob.connect().await?;
    bob.authenticate(bob_cfg.user_id, bob_cfg.token, bob_cfg.device_id)
        .await?;
    let reader = bob
        .local_reader()
        .await?
        .ok_or_else(|| std::io::Error::other("no Bob local reader"))?;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let rows = reader
                .messages(channel_id, i32::from(DIRECT_SYNC_CHANNEL_TYPE), 50, 0)
                .await?;
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| row.content.contains(&fixture.tag))
                .collect();
            if matches.len() == 1 && matches[0].server_message_id == Some(committed_id) {
                break Ok::<_, crate::BoxError>(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await??;
    let unread_before = bob
        .get_channel_unread_count(channel_id, i32::from(DIRECT_SYNC_CHANNEL_TYPE))
        .await?;
    if unread_before < 1 {
        return Err(std::io::Error::other("message missing from Bob's unread projection").into());
    }
    let pts: u64 = await_marker(dir, "committed-pts").await?.trim().parse()?;
    let read = manager.mark_read("bob", channel_id, pts).await?;
    if read.last_read_pts < pts {
        return Err(std::io::Error::other(
            "server read cursor did not cover the recovered message",
        )
        .into());
    }
    manager.refresh_local_views("bob").await?;
    if bob
        .get_channel_unread_count(channel_id, i32::from(DIRECT_SYNC_CHANNEL_TYPE))
        .await?
        != 0
    {
        return Err(std::io::Error::other("Bob's unread projection did not clear").into());
    }
    signal(dir, "passed", committed_id.to_string())?;
    println!("[PASS] real ACK loss, sender death, server restart and read reconciliation");
    Ok(())
}

pub async fn child(mode: &str) -> BoxResult<()> {
    let dir = PathBuf::from(
        std::env::var_os("PRIVCHAT_RESTART_FAULT_DIR")
            .ok_or_else(|| std::io::Error::other("missing isolated fault directory"))?,
    );
    let f: Fixture = serde_json::from_slice(&std::fs::read(dir.join("fixture"))?)?;
    let kind = i32::from(DIRECT_SYNC_CHANNEL_TYPE);
    let endpoint = if mode == "send" {
        port("PRIVCHAT_FAULT_PROXY_PORT")?
    } else if mode == "recover" {
        port("PRIVCHAT_TCP_PORT")?
    } else {
        return Err(std::io::Error::other("unknown fault child mode").into());
    };
    let sdk = PrivchatSdk::new(PrivchatConfig {
        endpoints: vec![ServerEndpoint {
            protocol: TransportProtocol::Tcp,
            host: "127.0.0.1".into(),
            port: endpoint,
            path: None,
            use_tls: false,
        }],
        spki_pins: vec![std::env::var("PRIVCHAT_SPKI_PINS")?],
        data_dir: f.data_dir.display().to_string(),
        ..Default::default()
    });
    sdk.connect().await?;
    let login = sdk.login(f.username, f.password, f.device_id).await?;
    sdk.authenticate(login.user_id, login.token, login.device_id)
        .await?;
    sdk.run_bootstrap_sync().await?;

    if mode == "send" {
        signal(&dir, "sender-ready", "")?;
        await_marker(&dir, "enqueue").await?;
        let id = sdk
            .create_local_message_queued(
                privchat_sdk::NewMessage {
                    channel_id: f.channel_id,
                    channel_type: kind,
                    from_uid: f.user_id,
                    message_type: 0,
                    content: f.tag,
                    searchable_word: String::new(),
                    setting: 0,
                    extra: String::new(),
                    mime_type: None,
                    media_downloaded: false,
                    thumb_status: 0,
                },
                None,
                "message",
                Vec::new(),
                None,
            )
            .await?;
        signal(&dir, "local-row", id.to_string())?;
        await_marker(&dir, "commit-confirmed").await?;
        let row = sdk
            .get_message_by_id(id)
            .await?
            .ok_or_else(|| std::io::Error::other("lost local outbox row"))?;
        if row.status == 2
            || row.server_message_id.is_some()
            || !sdk
                .peek_outbound_messages(100)
                .await?
                .iter()
                .any(|row| row.message_id == id)
        {
            return Err(
                std::io::Error::other("sender learned the ACK before the injected crash").into(),
            );
        }
        signal(&dir, "sender-pending", "")?;
        // Parent must kill this process; graceful shutdown would miss the fault.
        std::future::pending::<()>().await;
    } else {
        let id: u64 = await_marker(&dir, "local-row").await?.parse()?;
        let server_id = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Some(row) = sdk.get_message_by_id(id).await? {
                    if row.status == 2
                        && !sdk
                            .peek_outbound_messages(100)
                            .await?
                            .iter()
                            .any(|row| row.message_id == id)
                    {
                        if let Some(server_id) = row.server_message_id {
                            break Ok::<_, crate::BoxError>(server_id);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await??;
        sdk.shutdown().await;
        signal(&dir, "sender-recovered", server_id.to_string())?;
    }
    Ok(())
}
