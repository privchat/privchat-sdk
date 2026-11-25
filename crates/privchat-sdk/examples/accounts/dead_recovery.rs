//! Isolated live verification of the wire hint and the resulting SDK pull.
use crate::{account_manager::MultiAccountManager, BoxError, BoxResult};
use privchat_protocol::{
    decode_message, EntityInvalidationBatch, EntityMutationHint, ENTITY_INVALIDATION_PUSH_TOPIC_V1,
};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

const MESSAGE_ID: u64 = 991_650_005;
const PROBE: &str = "dead-recovery-fixture-only-body";

fn require(ok: bool, context: &str) -> BoxResult<()> {
    if !ok {
        return Err(std::io::Error::other(context).into());
    }
    Ok(())
}

async fn wait_for_file(dir: &Path, name: &str) -> BoxResult<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        while !dir.join(name).is_file() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| std::io::Error::other(format!("timed out waiting for {name}")))?;
    Ok(())
}

fn inspect_inbound_trace(log: &str, scope: &str) -> BoxResult<()> {
    let mut hints = 0;
    for line in log.lines().filter(|line| {
        [
            "decoded PushMessageRequest:",
            "decoded PushBatchRequest:",
            "decoded PublishRequest:",
            "decoded SendMessageRequest:",
        ]
        .iter()
        .any(|prefix| line.contains(prefix))
    }) {
        require(
            !line.contains(&format!("server_message_id: {MESSAGE_ID}")) && !line.contains(PROBE),
            "original timeline message appeared on an inbound push frame",
        )?;
        for array in line.split("payload: [").skip(1) {
            let raw = array
                .split_once(']')
                .ok_or_else(|| std::io::Error::other("incomplete inbound payload trace"))?
                .0;
            let bytes: Vec<u8> = raw
                .split(',')
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().parse())
                .collect::<Result<_, _>>()?;
            require(
                !bytes
                    .windows(PROBE.len())
                    .any(|part| part == PROBE.as_bytes()),
                "original body bytes appeared in a push, publish or batch payload",
            )?;
            if line.contains(&format!("topic: \"{ENTITY_INVALIDATION_PUSH_TOPIC_V1}\"")) {
                let batch: EntityInvalidationBatch = decode_message(&bytes)?;
                if batch
                    .items
                    .iter()
                    .any(|item| item.entity_type == "message_timeline")
                {
                    require(
                        line.contains("channel_id: 0")
                            && line.contains("from_uid: 0")
                            && batch.items.len() == 1,
                        "unexpected envelope metadata on recovery hint",
                    )?;
                    let item = &batch.items[0];
                    require(
                        item.entity_type == "message_timeline"
                            && item.scope.as_deref() == Some(scope)
                            && item.entity_id.is_none()
                            && item.target_version == 0
                            && item.mutation_hint == EntityMutationHint::Unknown,
                        "recovery hint carried unexpected fields",
                    )?;
                    hints += 1;
                }
            }
        }
    }
    require(hints == 1, "expected exactly one decoded timeline hint")
}

pub(crate) async fn run(manager: &MultiAccountManager, dir: &Path) -> BoxResult<()> {
    let channel = manager
        .cached_direct_channel("alice", "bob")
        .ok_or_else(|| std::io::Error::other("recovery gate requires phase2"))?;
    let alice = manager.sdk("alice")?;
    let bob = manager.sdk("bob")?;
    require(
        alice.is_connected().await? && bob.is_connected().await?,
        "both clients must be online",
    )?;
    bob.resume_channel_difference(channel, 1).await?;
    let ar = alice
        .local_reader()
        .await?
        .ok_or_else(|| std::io::Error::other("no Alice reader"))?;
    let br = bob
        .local_reader()
        .await?
        .ok_or_else(|| std::io::Error::other("no Bob reader"))?;
    let trace = dir.join("accounts.log");
    let offset = std::fs::metadata(&trace)?.len();
    let fixture = serde_json::json!({
        "channel": channel.to_string(),
        "alice": manager.user_id("alice")?.to_string(),
        "bob": manager.user_id("bob")?.to_string(),
        "message": MESSAGE_ID.to_string(),
        "body": PROBE,
    });
    std::fs::write(dir.join("fixture.tmp"), serde_json::to_vec(&fixture)?)?;
    std::fs::rename(dir.join("fixture.tmp"), dir.join("fixture"))?;
    wait_for_file(dir, "dead-ready").await?;
    require(
        !br.messages(channel, 1, 50, 0)
            .await?
            .iter()
            .any(|row| row.server_message_id == Some(MESSAGE_ID)),
        "Bob already had the message before the recovery hint",
    )?;

    let url = format!(
        "http://127.0.0.1:{}/api/service/messages/dispatch/recover",
        std::env::var("PRIVCHAT_ADMIN_API_PORT")?.parse::<u16>()?
    );
    let key = std::env::var("SERVICE_MASTER_KEY")?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()?;
    let request = serde_json::json!({
        "request_id": "2bb8c763-eecf-414d-8c41-64d093d85c8a",
        "event_id": MESSAGE_ID.to_string(),
        "user_id": fixture["bob"],
    });
    for _ in 0..2 {
        let reply: serde_json::Value = client
            .post(&url)
            .header("X-Service-Key", &key)
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        require(
            reply["code"] == 0 && reply["data"]["notified_sessions"] == 1,
            "expected one targeted online handoff and the same result on retry",
        )?;
    }
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = br.messages(channel, 1, 50, 0).await?;
            let matches: Vec<_> = rows
                .iter()
                .filter(|row| row.server_message_id == Some(MESSAGE_ID))
                .collect();
            if !matches.is_empty() {
                require(
                    matches.len() == 1 && matches[0].content.contains(PROBE),
                    "invalid pulled projection",
                )?;
                break Ok::<_, BoxError>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    require(
        !ar.messages(channel, 1, 50, 0)
            .await?
            .iter()
            .any(|row| row.server_message_id == Some(MESSAGE_ID)),
        "non-target SDK materialized the recovery fixture",
    )?;
    let mut file = std::fs::File::open(trace)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut log = String::new();
    file.read_to_string(&mut log)?;
    inspect_inbound_trace(&log, &format!("1:{channel}"))?;
    println!("[PASS] one bodyless wire hint, no replay, one authorized local projection");
    std::fs::write(dir.join("wire-passed"), "")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use privchat_protocol::{encode_message, EntityInvalidation, PushMessageRequest};

    #[test]
    fn wire_inspector_rejects_duplicate_hints_and_original_body_pushes() {
        let batch = EntityInvalidationBatch::new_v1(
            41,
            vec![EntityInvalidation {
                entity_type: "message_timeline".into(),
                entity_id: None,
                target_version: 0,
                scope: Some("1:42".into()),
                mutation_hint: EntityMutationHint::Unknown,
            }],
            1000,
        )
        .unwrap();
        let frame = PushMessageRequest {
            topic: ENTITY_INVALIDATION_PUSH_TOPIC_V1.into(),
            payload: encode_message(&batch).unwrap(),
            ..Default::default()
        };
        let log = format!("[SDK.inbound] decoded PushMessageRequest: {frame:?}\n");
        inspect_inbound_trace(&log, "1:42").unwrap();
        assert!(inspect_inbound_trace(&log, "1:43").is_err());
        assert!(inspect_inbound_trace(&format!("{log}{log}"), "1:42").is_err());

        let replay = PushMessageRequest {
            payload: PROBE.as_bytes().to_vec(),
            ..Default::default()
        };
        assert!(inspect_inbound_trace(
            &format!("{log}[SDK.inbound] decoded PushMessageRequest: {replay:?}\n"),
            "1:42"
        )
        .is_err());
    }
}
