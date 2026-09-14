//! Account-scoped local reads bypass the network actor, not the database actor.
//! No connection, authentication refresh or synchronization is performed here.
use crate::{storage_actor::StorageHandle, *};

#[derive(Clone)]
pub struct LocalReader {
    storage: StorageHandle,
    owner_uid: String,
}

impl PrivchatSdk {
    pub async fn local_reader(&self) -> Result<Option<LocalReader>> {
        self.ensure_running()?;
        let mut ready = self.local_storage.clone();
        let storage = loop {
            if let Some(storage) = ready.borrow().clone() {
                break storage;
            }
            ready
                .changed()
                .await
                .map_err(|_| self.actor_channel_error())?;
        };
        let Some(owner_uid) = storage.load_current_uid().await? else {
            return Ok(None);
        };
        Ok(Some(LocalReader { storage, owner_uid }))
    }

    pub async fn local_session_snapshot(&self) -> Result<Option<SessionSnapshot>> {
        match self.local_reader().await? {
            Some(reader) => reader.session().await,
            None => Ok(None),
        }
    }
}

impl LocalReader {
    pub async fn message(&self, id: u64) -> Result<Option<StoredMessage>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.get_message_by_id(u, id)
            })
            .await
    }
    pub async fn session(&self) -> Result<Option<SessionSnapshot>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), |s, u| s.load_session(u))
            .await
    }
    pub async fn channels(&self, limit: usize, offset: usize) -> Result<Vec<StoredChannel>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                let mut channels = s.list_channels(u, limit, offset)?;
                for channel in &mut channels {
                    if channel.last_local_message_id > 0 {
                        if let Some(message) =
                            s.get_message_by_id(u, channel.last_local_message_id)?
                        {
                            if message.channel_id == channel.channel_id
                                && message.channel_type == channel.channel_type
                            {
                                channel.last_message_type = Some(message.message_type);
                            }
                        }
                    }
                }
                Ok(channels)
            })
            .await
    }
    pub async fn friends(&self, limit: usize, offset: usize) -> Result<Vec<StoredFriend>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.list_friends(u, limit, offset)
            })
            .await
    }
    pub async fn groups(&self, limit: usize, offset: usize) -> Result<Vec<StoredGroup>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.list_groups(u, limit, offset)
            })
            .await
    }
    pub async fn group_members(
        &self,
        id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroupMember>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.list_group_members(u, id, limit, offset)
            })
            .await
    }
    pub async fn messages(
        &self,
        id: u64,
        kind: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.list_messages(u, id, kind, limit, offset)
            })
            .await
    }
    pub async fn message_extra(&self, id: u64) -> Result<Option<StoredMessageExtra>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.get_message_extra(u, id)
            })
            .await
    }
    pub async fn older_messages(
        &self,
        id: u64,
        kind: i32,
        before: u64,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                Ok(s.list_messages_around(u, id, kind, before, limit, 0)?
                    .into_iter()
                    .filter(|m| m.server_message_id.is_some_and(|id| id < before))
                    .collect())
            })
            .await
    }
    pub async fn channel(&self, id: u64) -> Result<Option<StoredChannel>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.get_channel_by_id(u, id)
            })
            .await
    }
    pub async fn channel_extra(&self, id: u64, kind: i32) -> Result<Option<StoredChannelExtra>> {
        self.storage
            .read_scoped(self.owner_uid.clone(), move |s, u| {
                s.get_channel_extra(u, id, kind)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_store::LocalStore;

    #[tokio::test]
    async fn local_reads_complete_while_network_actor_is_blocked_and_reject_account_switch() {
        let dir = std::env::temp_dir().join(format!(
            "privchat-local-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = LocalStore::open_at(dir.clone()).unwrap();
        for uid in ["10001", "10002"] {
            store
                .save_login(
                    uid,
                    &LoginResult {
                        user_id: uid.parse().unwrap(),
                        token: "local-test-token".into(),
                        device_id: "local-test-device".into(),
                        refresh_token: None,
                        expires_at: 0,
                    },
                )
                .unwrap();
        }
        store.save_current_uid("10001").unwrap();
        store
            .upsert_friend(
                "10001",
                &UpsertFriendInput {
                    user_id: 55,
                    tags: None,
                    is_pinned: false,
                    created_at: 1,
                    version: 1,
                    updated_at: 1,
                    status: 1,
                    is_outgoing: None,
                    request_message: None,
                    request_source: None,
                    request_source_id: None,
                },
            )
            .unwrap();
        drop(store);
        let mut config = PrivchatConfig::default();
        config.data_dir = dir.display().to_string();
        let sdk = PrivchatSdk::new(config);
        let reader = sdk.local_reader().await.unwrap().unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        sdk.tx
            .send(Command::HoldActorForLocalReadTest {
                entered: entered_tx,
                release: release_rx,
            })
            .await
            .unwrap();
        entered_rx.await.unwrap();
        let reads = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert_eq!(reader.session().await.unwrap().unwrap().user_id, 10001);
            reader.channels(10, 0).await.unwrap();
            assert_eq!(reader.friends(10, 0).await.unwrap()[0].user_id, 55);
            reader.groups(10, 0).await.unwrap();
            reader.messages(5, 2, 10, 0).await.unwrap();
            reader.group_members(5, 9, 0).await.unwrap();
        })
        .await;
        reader
            .storage
            .save_current_uid("10002".into())
            .await
            .unwrap();
        assert!(
            reader.friends(10, 0).await.is_err(),
            "old reader must never read another account"
        );
        let _ = release_tx.send(());
        sdk.shutdown().await;
        assert!(reads.is_ok(), "local reads waited behind the network actor");
        drop(sdk);
        drop(reader);
        let _ = std::fs::remove_dir_all(dir);
    }
}
