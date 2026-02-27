use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use crate::local_store::{LocalAccountEntry, LocalStore, StoragePaths};
use crate::{
    Error, LoginResult, MentionInput, NewMessage, Result, SessionSnapshot, StoredBlacklistEntry,
    StoredChannel, StoredChannelExtra, StoredChannelMember, StoredFriend, StoredGroup,
    StoredGroupMember, StoredMessage, StoredMessageExtra, StoredMessageReaction, StoredReminder,
    StoredUser, UnreadMentionCount, UpsertBlacklistInput, UpsertChannelExtraInput,
    UpsertChannelInput, UpsertChannelMemberInput, UpsertFriendInput, UpsertGroupInput,
    UpsertGroupMemberInput, UpsertMessageReactionInput, UpsertReminderInput,
    UpsertRemoteMessageInput, UpsertRemoteMessageResult, UpsertUserInput,
};

enum StorageCmd {
    SaveLogin {
        uid: String,
        login: LoginResult,
        resp: oneshot::Sender<Result<()>>,
    },
    SetBootstrapCompleted {
        uid: String,
        completed: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    LoadSession {
        uid: String,
        resp: oneshot::Sender<Result<Option<SessionSnapshot>>>,
    },
    ClearSession {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    LoadCurrentUid {
        resp: oneshot::Sender<Result<Option<String>>>,
    },
    SaveCurrentUid {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    ClearCurrentUid {
        resp: oneshot::Sender<Result<()>>,
    },
    WipeUserFull {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    ListLocalAccounts {
        resp: oneshot::Sender<Result<(Option<String>, Vec<LocalAccountEntry>)>>,
    },
    FlushUser {
        uid: String,
        resp: oneshot::Sender<Result<()>>,
    },
    NormalQueuePush {
        message_id: u64,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<u64>>,
    },
    NormalQueuePeek {
        limit: usize,
        resp: oneshot::Sender<Result<Vec<(u64, Vec<u8>)>>>,
    },
    NormalQueueAck {
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    SelectFileQueue {
        route_key: String,
        resp: oneshot::Sender<Result<usize>>,
    },
    FileQueueCount {
        resp: oneshot::Sender<Result<usize>>,
    },
    FileQueuePush {
        queue_index: usize,
        message_id: u64,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<u64>>,
    },
    FileQueuePeek {
        queue_index: usize,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<(u64, Vec<u8>)>>>,
    },
    FileQueueAck {
        queue_index: usize,
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    CreateLocalMessage {
        local_message_id: u64,
        input: NewMessage,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetMessageById {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessage>>>,
    },
    UpsertRemoteMessageWithResult {
        input: UpsertRemoteMessageInput,
        resp: oneshot::Sender<Result<UpsertRemoteMessageResult>>,
    },
    #[allow(dead_code)]
    BatchUpsertRemoteMessages {
        inputs: Vec<UpsertRemoteMessageInput>,
        resp: oneshot::Sender<Vec<Result<UpsertRemoteMessageResult>>>,
    },
    ListMessages {
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessage>>>,
    },
    UpsertChannel {
        input: UpsertChannelInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelById {
        channel_id: u64,
        resp: oneshot::Sender<Result<Option<StoredChannel>>>,
    },
    ListChannels {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannel>>>,
    },
    UpsertChannelExtra {
        input: UpsertChannelExtraInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelExtra {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<Option<StoredChannelExtra>>>,
    },
    MarkMessageSent {
        message_id: u64,
        server_message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateLocalMessageId {
        message_id: u64,
        local_message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    GetLocalMessageId {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<u64>>>,
    },
    GetMessageIdByServerMessageId {
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
        resp: oneshot::Sender<Result<Option<u64>>>,
    },
    UpdateMessageStatus {
        message_id: u64,
        status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRead {
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRevoke {
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
        resp: oneshot::Sender<Result<()>>,
    },
    EditMessage {
        message_id: u64,
        content: String,
        edited_at: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessagePinned {
        message_id: u64,
        is_pinned: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    GetMessageExtra {
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessageExtra>>>,
    },
    MarkChannelRead {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelUnreadCount {
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<i32>>,
    },
    GetTotalUnreadCount {
        exclude_muted: bool,
        resp: oneshot::Sender<Result<i32>>,
    },
    UpsertUser {
        input: UpsertUserInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetUserById {
        user_id: u64,
        resp: oneshot::Sender<Result<Option<StoredUser>>>,
    },
    ListUsersByIds {
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<Vec<StoredUser>>>,
    },
    UpsertFriend {
        input: UpsertFriendInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteFriend {
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListFriends {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredFriend>>>,
    },
    UpsertBlacklistEntry {
        input: UpsertBlacklistInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteBlacklistEntry {
        blocked_user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListBlacklistEntries {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredBlacklistEntry>>>,
    },
    UpsertGroup {
        input: UpsertGroupInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetGroupById {
        group_id: u64,
        resp: oneshot::Sender<Result<Option<StoredGroup>>>,
    },
    ListGroups {
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroup>>>,
    },
    UpsertGroupMember {
        input: UpsertGroupMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteGroupMember {
        group_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListGroupMembers {
        group_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroupMember>>>,
    },
    UpsertChannelMember {
        input: UpsertChannelMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListChannelMembers {
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannelMember>>>,
    },
    DeleteChannelMember {
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpsertMessageReaction {
        input: UpsertMessageReactionInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListMessageReactions {
        message_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessageReaction>>>,
    },
    RecordMention {
        input: MentionInput,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetUnreadMentionCount {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<i32>>,
    },
    ListUnreadMentionMessageIds {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<u64>>>,
    },
    MarkMentionRead {
        message_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    MarkAllMentionsRead {
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    GetAllUnreadMentionCounts {
        user_id: u64,
        resp: oneshot::Sender<Result<Vec<UnreadMentionCount>>>,
    },
    UpsertReminder {
        input: UpsertReminderInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListPendingReminders {
        reminder_uid: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredReminder>>>,
    },
    MarkReminderDone {
        reminder_id: u64,
        done: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    KvPut {
        key: String,
        value: Vec<u8>,
        resp: oneshot::Sender<Result<()>>,
    },
    KvGet {
        key: String,
        resp: oneshot::Sender<Result<Option<Vec<u8>>>>,
    },
    KvDelete {
        key: String,
        resp: oneshot::Sender<Result<()>>,
    },
    KvScanPrefix {
        prefix: String,
        resp: oneshot::Sender<Result<Vec<(String, Vec<u8>)>>>,
    },
    GetStoragePaths {
        resp: oneshot::Sender<Result<StoragePaths>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct StorageHandle {
    tx: mpsc::Sender<StorageCmd>,
}

impl StorageHandle {
    pub fn start() -> Result<Self> {
        let store = LocalStore::open_default()?;
        let (tx, rx) = mpsc::channel::<StorageCmd>();
        thread::Builder::new()
            .name("privchat-db-actor".to_string())
            .spawn(move || run_loop(store, rx))
            .map_err(|e| Error::Storage(format!("spawn db actor: {e}")))?;
        Ok(Self { tx })
    }

    pub fn start_at(base_dir: std::path::PathBuf) -> Result<Self> {
        let store = LocalStore::open_at(base_dir)?;
        let (tx, rx) = mpsc::channel::<StorageCmd>();
        thread::Builder::new()
            .name("privchat-db-actor".to_string())
            .spawn(move || run_loop(store, rx))
            .map_err(|e| Error::Storage(format!("spawn db actor: {e}")))?;
        Ok(Self { tx })
    }

    pub async fn save_login(&self, uid: String, login: LoginResult) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SaveLogin {
                uid,
                login,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_bootstrap_completed(&self, uid: String, completed: bool) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetBootstrapCompleted {
                uid,
                completed,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn load_session(&self, uid: String) -> Result<Option<SessionSnapshot>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::LoadSession { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn clear_session(&self, uid: String) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ClearSession { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn load_current_uid(&self) -> Result<Option<String>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::LoadCurrentUid { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn save_current_uid(&self, uid: String) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SaveCurrentUid { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn clear_current_uid(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ClearCurrentUid { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn wipe_user_full(&self, uid: String) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::WipeUserFull { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_local_accounts(&self) -> Result<(Option<String>, Vec<LocalAccountEntry>)> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListLocalAccounts { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn flush_user(&self, uid: String) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FlushUser { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_push(&self, message_id: u64, payload: Vec<u8>) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueuePush {
                message_id,
                payload,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_peek(&self, limit: usize) -> Result<Vec<(u64, Vec<u8>)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueuePeek {
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_ack(&self, message_ids: Vec<u64>) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueueAck {
                message_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn select_file_queue(&self, route_key: String) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SelectFileQueue {
                route_key,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_count(&self) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueueCount { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_push(
        &self,
        queue_index: usize,
        message_id: u64,
        payload: Vec<u8>,
    ) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueuePush {
                queue_index,
                message_id,
                payload,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_peek(
        &self,
        queue_index: usize,
        limit: usize,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueuePeek {
                queue_index,
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_ack(&self, queue_index: usize, message_ids: Vec<u64>) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueueAck {
                queue_index,
                message_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn create_local_message(
        &self,
        local_message_id: u64,
        input: NewMessage,
    ) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::CreateLocalMessage {
                local_message_id,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_by_id(&self, message_id: u64) -> Result<Option<StoredMessage>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetMessageById {
                message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_remote_message_with_result(
        &self,
        input: UpsertRemoteMessageInput,
    ) -> Result<UpsertRemoteMessageResult> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertRemoteMessageWithResult {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    /// Batch upsert remote messages in a single SQLite transaction.
    /// Returns a Vec of results, one per input, in the same order.
    #[allow(dead_code)]
    pub async fn batch_upsert_remote_messages(
        &self,
        inputs: Vec<UpsertRemoteMessageInput>,
    ) -> Result<Vec<Result<UpsertRemoteMessageResult>>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::BatchUpsertRemoteMessages {
                inputs,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)
    }

    pub async fn list_messages(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListMessages {
                channel_id,
                channel_type,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel(&self, input: UpsertChannelInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannel {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_by_id(&self, channel_id: u64) -> Result<Option<StoredChannel>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelById {
                channel_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channels(&self, limit: usize, offset: usize) -> Result<Vec<StoredChannel>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListChannels {
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_extra(&self, input: UpsertChannelExtraInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannelExtra {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_extra(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<StoredChannelExtra>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelExtra {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_message_sent(&self, message_id: u64, server_message_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkMessageSent {
                message_id,
                server_message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn update_local_message_id(
        &self,
        message_id: u64,
        local_message_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpdateLocalMessageId {
                message_id,
                local_message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_local_message_id(&self, message_id: u64) -> Result<Option<u64>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetLocalMessageId {
                message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_id_by_server_message_id(
        &self,
        channel_id: u64,
        channel_type: i32,
        server_message_id: u64,
    ) -> Result<Option<u64>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetMessageIdByServerMessageId {
                channel_id,
                channel_type,
                server_message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn update_message_status(&self, message_id: u64, status: i32) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpdateMessageStatus {
                message_id,
                status,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_read(
        &self,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessageRead {
                message_id,
                channel_id,
                channel_type,
                is_read,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_revoke(
        &self,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessageRevoke {
                message_id,
                revoked,
                revoker,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn edit_message(&self, message_id: u64, content: &str, edited_at: i32) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::EditMessage {
                message_id,
                content: content.to_string(),
                edited_at,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_pinned(&self, message_id: u64, is_pinned: bool) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessagePinned {
                message_id,
                is_pinned,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_extra(&self, message_id: u64) -> Result<Option<StoredMessageExtra>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetMessageExtra {
                message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_channel_read(&self, channel_id: u64, channel_type: i32) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkChannelRead {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_unread_count(
        &self,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelUnreadCount {
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_total_unread_count(&self, exclude_muted: bool) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetTotalUnreadCount {
                exclude_muted,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_user(&self, input: UpsertUserInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertUser {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_user_by_id(&self, user_id: u64) -> Result<Option<StoredUser>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetUserById {
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_users_by_ids(&self, user_ids: Vec<u64>) -> Result<Vec<StoredUser>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListUsersByIds {
                user_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_friend(&self, input: UpsertFriendInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertFriend {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_friend(&self, user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteFriend {
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_friends(&self, limit: usize, offset: usize) -> Result<Vec<StoredFriend>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListFriends {
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_blacklist_entry(&self, input: UpsertBlacklistInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertBlacklistEntry {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_blacklist_entry(&self, blocked_user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteBlacklistEntry {
                blocked_user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_blacklist_entries(
        &self,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredBlacklistEntry>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListBlacklistEntries {
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group(&self, input: UpsertGroupInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertGroup {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_group_by_id(&self, group_id: u64) -> Result<Option<StoredGroup>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetGroupById {
                group_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_groups(&self, limit: usize, offset: usize) -> Result<Vec<StoredGroup>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListGroups {
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group_member(&self, input: UpsertGroupMemberInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertGroupMember {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_member(&self, input: UpsertChannelMemberInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannelMember {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channel_members(
        &self,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannelMember>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListChannelMembers {
                channel_id,
                channel_type,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_channel_member(
        &self,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteChannelMember {
                channel_id,
                channel_type,
                member_uid,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_group_members(
        &self,
        group_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroupMember>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListGroupMembers {
                group_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_group_member(&self, group_id: u64, user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteGroupMember {
                group_id,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_message_reaction(&self, input: UpsertMessageReactionInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertMessageReaction {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_message_reactions(
        &self,
        message_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessageReaction>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListMessageReactions {
                message_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn record_mention(&self, input: MentionInput) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::RecordMention {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_unread_mention_count(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetUnreadMentionCount {
                channel_id,
                channel_type,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_unread_mention_message_ids(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
    ) -> Result<Vec<u64>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListUnreadMentionMessageIds {
                channel_id,
                channel_type,
                user_id,
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_mention_read(&self, message_id: u64, user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkMentionRead {
                message_id,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_all_mentions_read(
        &self,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkAllMentionsRead {
                channel_id,
                channel_type,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_all_unread_mention_counts(
        &self,
        user_id: u64,
    ) -> Result<Vec<UnreadMentionCount>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetAllUnreadMentionCounts {
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_reminder(&self, input: UpsertReminderInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertReminder {
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_pending_reminders(
        &self,
        reminder_uid: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredReminder>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListPendingReminders {
                reminder_uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_reminder_done(&self, reminder_id: u64, done: bool) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkReminderDone {
                reminder_id,
                done,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_put(&self, key: String, value: Vec<u8>) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvPut {
                key,
                value,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_get(&self, key: String) -> Result<Option<Vec<u8>>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvGet { key, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_delete(&self, key: String) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvDelete { key, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_scan_prefix(&self, prefix: String) -> Result<Vec<(String, Vec<u8>)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvScanPrefix {
                prefix,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_storage_paths(&self) -> Result<StoragePaths> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetStoragePaths { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(StorageCmd::Shutdown);
    }
}

/// Flush a batch of UpsertRemoteMessage commands in a single transaction.
fn flush_upsert_batch(
    store: &LocalStore,
    batch: Vec<(
        UpsertRemoteMessageInput,
        oneshot::Sender<Result<UpsertRemoteMessageResult>>,
    )>,
) {
    if batch.is_empty() {
        return;
    }
    match store.load_current_uid() {
        Ok(Some(uid)) => {
            let inputs: Vec<_> = batch.iter().map(|(i, _)| i.clone()).collect();
            let results = store.batch_upsert_remote_messages(&uid, &inputs);
            for ((_, resp), result) in batch.into_iter().zip(results.into_iter()) {
                let _ = resp.send(result);
            }
        }
        Ok(None) => {
            for (_, resp) in batch {
                let _ = resp.send(Err(Error::InvalidState(
                    "current uid is not set".to_string(),
                )));
            }
        }
        Err(e) => {
            for (_, resp) in batch {
                let _ = resp.send(Err(Error::Storage(format!("load uid: {e}"))));
            }
        }
    }
}

fn dispatch_cmd_inner(store: &LocalStore, cmd: StorageCmd, rx: &mpsc::Receiver<StorageCmd>) {
    match cmd {
        StorageCmd::UpsertRemoteMessageWithResult { input, resp } => {
            // Even deferred upserts go through batch path
            let mut batch = Vec::with_capacity(32);
            batch.push((input, resp));
            while batch.len() < 32 {
                match rx.try_recv() {
                    Ok(StorageCmd::UpsertRemoteMessageWithResult { input, resp }) => {
                        batch.push((input, resp));
                    }
                    Ok(other) => {
                        flush_upsert_batch(store, batch);
                        dispatch_cmd_inner(store, other, rx);
                        return;
                    }
                    Err(_) => break,
                }
            }
            flush_upsert_batch(store, batch);
        }
        // For all other commands, delegate to the existing per-command handler
        other => handle_single_cmd(store, other),
    }
}

fn handle_single_cmd(store: &LocalStore, cmd: StorageCmd) {
    // Helper macro to reduce boilerplate for commands that need current uid
    macro_rules! with_uid {
        ($resp:expr, |$uid:ident| $body:expr) => {
            let result = match store.load_current_uid() {
                Ok(Some($uid)) => $body,
                Ok(None) => Err(Error::InvalidState("current uid is not set".to_string())),
                Err(e) => Err(e),
            };
            let _ = $resp.send(result);
        };
    }

    match cmd {
        StorageCmd::SaveLogin { uid, login, resp } => {
            let _ = resp.send(store.save_login(&uid, &login));
        }
        StorageCmd::SetBootstrapCompleted {
            uid,
            completed,
            resp,
        } => {
            let _ = resp.send(store.set_bootstrap_completed(&uid, completed));
        }
        StorageCmd::LoadSession { uid, resp } => {
            let _ = resp.send(store.load_session(&uid));
        }
        StorageCmd::ClearSession { uid, resp } => {
            let _ = resp.send(store.clear_session(&uid));
        }
        StorageCmd::LoadCurrentUid { resp } => {
            let _ = resp.send(store.load_current_uid());
        }
        StorageCmd::SaveCurrentUid { uid, resp } => {
            let _ = resp.send(store.save_current_uid(&uid));
        }
        StorageCmd::ClearCurrentUid { resp } => {
            let _ = resp.send(store.clear_current_uid());
        }
        StorageCmd::WipeUserFull { uid, resp } => {
            let _ = resp.send(store.wipe_user_full(&uid));
        }
        StorageCmd::ListLocalAccounts { resp } => {
            let _ = resp.send(store.list_local_accounts());
        }
        StorageCmd::FlushUser { uid, resp } => {
            let _ = resp.send(store.flush_user(&uid));
        }
        StorageCmd::NormalQueuePush {
            message_id,
            payload,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .normal_queue_push(&uid, message_id, &payload));
        }
        StorageCmd::NormalQueuePeek { limit, resp } => {
            with_uid!(resp, |uid| store.normal_queue_peek(&uid, limit));
        }
        StorageCmd::NormalQueueAck { message_ids, resp } => {
            with_uid!(resp, |uid| store.normal_queue_ack(&uid, &message_ids));
        }
        StorageCmd::SelectFileQueue { route_key, resp } => {
            with_uid!(resp, |uid| store.select_file_queue(&uid, &route_key));
        }
        StorageCmd::FileQueueCount { resp } => {
            with_uid!(resp, |uid| store.file_queue_count(&uid));
        }
        StorageCmd::FileQueuePush {
            queue_index,
            message_id,
            payload,
            resp,
        } => {
            with_uid!(resp, |uid| store.file_queue_push(
                &uid,
                queue_index,
                message_id,
                &payload
            ));
        }
        StorageCmd::FileQueuePeek {
            queue_index,
            limit,
            resp,
        } => {
            with_uid!(resp, |uid| store.file_queue_peek(&uid, queue_index, limit));
        }
        StorageCmd::FileQueueAck {
            queue_index,
            message_ids,
            resp,
        } => {
            with_uid!(resp, |uid| store.file_queue_ack(
                &uid,
                queue_index,
                &message_ids
            ));
        }
        StorageCmd::CreateLocalMessage {
            local_message_id,
            input,
            resp,
        } => {
            with_uid!(resp, |uid| store.create_local_message(
                &uid,
                &input,
                local_message_id
            ));
        }
        StorageCmd::GetMessageById { message_id, resp } => {
            with_uid!(resp, |uid| store.get_message_by_id(&uid, message_id));
        }
        StorageCmd::UpsertRemoteMessageWithResult { input, resp } => {
            with_uid!(resp, |uid| store.upsert_remote_message_with_result(&uid, &input));
        }
        StorageCmd::BatchUpsertRemoteMessages { inputs, resp } => {
            let result = match store.load_current_uid() {
                Ok(Some(uid)) => store.batch_upsert_remote_messages(&uid, &inputs),
                Ok(None) => inputs
                    .iter()
                    .map(|_| Err(Error::InvalidState("current uid is not set".to_string())))
                    .collect(),
                Err(e) => inputs
                    .iter()
                    .map(|_| Err(Error::Storage(format!("load uid: {e}"))))
                    .collect(),
            };
            let _ = resp.send(result);
        }
        StorageCmd::ListMessages {
            channel_id,
            channel_type,
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_messages(
                &uid,
                channel_id,
                channel_type,
                limit,
                offset
            ));
        }
        StorageCmd::UpsertChannel { input, resp } => {
            with_uid!(resp, |uid| store.upsert_channel(&uid, &input));
        }
        StorageCmd::GetChannelById { channel_id, resp } => {
            with_uid!(resp, |uid| store.get_channel_by_id(&uid, channel_id));
        }
        StorageCmd::ListChannels {
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_channels(&uid, limit, offset));
        }
        StorageCmd::UpsertChannelExtra { input, resp } => {
            with_uid!(resp, |uid| store.upsert_channel_extra(&uid, &input));
        }
        StorageCmd::GetChannelExtra {
            channel_id,
            channel_type,
            resp,
        } => {
            with_uid!(resp, |uid| store.get_channel_extra(
                &uid,
                channel_id,
                channel_type
            ));
        }
        StorageCmd::MarkMessageSent {
            message_id,
            server_message_id,
            resp,
        } => {
            with_uid!(resp, |uid| store.mark_message_sent(
                &uid,
                message_id,
                server_message_id
            ));
        }
        StorageCmd::UpdateLocalMessageId {
            message_id,
            local_message_id,
            resp,
        } => {
            with_uid!(resp, |uid| store.update_local_message_id(
                &uid,
                message_id,
                local_message_id
            ));
        }
        StorageCmd::GetLocalMessageId { message_id, resp } => {
            with_uid!(resp, |uid| store.get_local_message_id(&uid, message_id));
        }
        StorageCmd::GetMessageIdByServerMessageId {
            channel_id,
            channel_type,
            server_message_id,
            resp,
        } => {
            with_uid!(resp, |uid| store.get_message_id_by_server_message_id(
                &uid,
                channel_id,
                channel_type,
                server_message_id
            ));
        }
        StorageCmd::UpdateMessageStatus {
            message_id,
            status,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .update_message_status(&uid, message_id, status));
        }
        StorageCmd::SetMessageRead {
            message_id,
            channel_id,
            channel_type,
            is_read,
            resp,
        } => {
            with_uid!(resp, |uid| store.set_message_read(
                &uid,
                message_id,
                channel_id,
                channel_type,
                is_read
            ));
        }
        StorageCmd::SetMessageRevoke {
            message_id,
            revoked,
            revoker,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .set_message_revoke(&uid, message_id, revoked, revoker));
        }
        StorageCmd::EditMessage {
            message_id,
            content,
            edited_at,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .edit_message(&uid, message_id, &content, edited_at));
        }
        StorageCmd::SetMessagePinned {
            message_id,
            is_pinned,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .set_message_pinned(&uid, message_id, is_pinned));
        }
        StorageCmd::GetMessageExtra { message_id, resp } => {
            with_uid!(resp, |uid| store.get_message_extra(&uid, message_id));
        }
        StorageCmd::MarkChannelRead {
            channel_id,
            channel_type,
            resp,
        } => {
            with_uid!(resp, |uid| store.mark_channel_read(
                &uid,
                channel_id,
                channel_type
            ));
        }
        StorageCmd::GetChannelUnreadCount {
            channel_id,
            channel_type,
            resp,
        } => {
            with_uid!(resp, |uid| store.get_channel_unread_count(
                &uid,
                channel_id,
                channel_type
            ));
        }
        StorageCmd::GetTotalUnreadCount {
            exclude_muted,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .get_total_unread_count(&uid, exclude_muted));
        }
        StorageCmd::UpsertUser { input, resp } => {
            with_uid!(resp, |uid| store.upsert_user(&uid, &input));
        }
        StorageCmd::GetUserById { user_id, resp } => {
            with_uid!(resp, |uid| store.get_user_by_id(&uid, user_id));
        }
        StorageCmd::ListUsersByIds { user_ids, resp } => {
            with_uid!(resp, |uid| store.list_users_by_ids(&uid, &user_ids));
        }
        StorageCmd::UpsertFriend { input, resp } => {
            with_uid!(resp, |uid| store.upsert_friend(&uid, &input));
        }
        StorageCmd::DeleteFriend { user_id, resp } => {
            with_uid!(resp, |uid| store.delete_friend(&uid, user_id));
        }
        StorageCmd::ListFriends {
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_friends(&uid, limit, offset));
        }
        StorageCmd::UpsertBlacklistEntry { input, resp } => {
            with_uid!(resp, |uid| store.upsert_blacklist_entry(&uid, &input));
        }
        StorageCmd::DeleteBlacklistEntry {
            blocked_user_id,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .delete_blacklist_entry(&uid, blocked_user_id));
        }
        StorageCmd::ListBlacklistEntries {
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .list_blacklist_entries(&uid, limit, offset));
        }
        StorageCmd::UpsertGroup { input, resp } => {
            with_uid!(resp, |uid| store.upsert_group(&uid, &input));
        }
        StorageCmd::GetGroupById { group_id, resp } => {
            with_uid!(resp, |uid| store.get_group_by_id(&uid, group_id));
        }
        StorageCmd::ListGroups {
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_groups(&uid, limit, offset));
        }
        StorageCmd::UpsertGroupMember { input, resp } => {
            with_uid!(resp, |uid| store.upsert_group_member(&uid, &input));
        }
        StorageCmd::DeleteGroupMember {
            group_id,
            user_id,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .delete_group_member(&uid, group_id, user_id));
        }
        StorageCmd::ListGroupMembers {
            group_id,
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .list_group_members(&uid, group_id, limit, offset));
        }
        StorageCmd::UpsertChannelMember { input, resp } => {
            with_uid!(resp, |uid| store.upsert_channel_member(&uid, &input));
        }
        StorageCmd::ListChannelMembers {
            channel_id,
            channel_type,
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_channel_members(
                &uid,
                channel_id,
                channel_type,
                limit,
                offset
            ));
        }
        StorageCmd::DeleteChannelMember {
            channel_id,
            channel_type,
            member_uid,
            resp,
        } => {
            with_uid!(resp, |uid| store.delete_channel_member(
                &uid,
                channel_id,
                channel_type,
                member_uid
            ));
        }
        StorageCmd::UpsertMessageReaction { input, resp } => {
            with_uid!(resp, |uid| store.upsert_message_reaction(&uid, &input));
        }
        StorageCmd::ListMessageReactions {
            message_id,
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .list_message_reactions(&uid, message_id, limit, offset));
        }
        StorageCmd::RecordMention { input, resp } => {
            with_uid!(resp, |uid| store.record_mention(&uid, &input));
        }
        StorageCmd::GetUnreadMentionCount {
            channel_id,
            channel_type,
            user_id,
            resp,
        } => {
            with_uid!(resp, |uid| store.get_unread_mention_count(
                &uid,
                channel_id,
                channel_type,
                user_id
            ));
        }
        StorageCmd::ListUnreadMentionMessageIds {
            channel_id,
            channel_type,
            user_id,
            limit,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_unread_mention_message_ids(
                &uid,
                channel_id,
                channel_type,
                user_id,
                limit
            ));
        }
        StorageCmd::MarkMentionRead {
            message_id,
            user_id,
            resp,
        } => {
            with_uid!(resp, |uid| store
                .mark_mention_read(&uid, message_id, user_id));
        }
        StorageCmd::MarkAllMentionsRead {
            channel_id,
            channel_type,
            user_id,
            resp,
        } => {
            with_uid!(resp, |uid| store.mark_all_mentions_read(
                &uid,
                channel_id,
                channel_type,
                user_id
            ));
        }
        StorageCmd::GetAllUnreadMentionCounts { user_id, resp } => {
            with_uid!(resp, |uid| store
                .get_all_unread_mention_counts(&uid, user_id));
        }
        StorageCmd::UpsertReminder { input, resp } => {
            with_uid!(resp, |uid| store.upsert_reminder(&uid, &input));
        }
        StorageCmd::ListPendingReminders {
            reminder_uid,
            limit,
            offset,
            resp,
        } => {
            with_uid!(resp, |uid| store.list_pending_reminders(
                &uid,
                reminder_uid,
                limit,
                offset
            ));
        }
        StorageCmd::MarkReminderDone {
            reminder_id,
            done,
            resp,
        } => {
            with_uid!(resp, |uid| store.mark_reminder_done(
                &uid,
                reminder_id,
                done
            ));
        }
        StorageCmd::KvPut { key, value, resp } => {
            with_uid!(resp, |uid| store.kv_put(&uid, &key, &value));
        }
        StorageCmd::KvGet { key, resp } => {
            with_uid!(resp, |uid| store.kv_get(&uid, &key));
        }
        StorageCmd::KvDelete { key, resp } => {
            with_uid!(resp, |uid| store.kv_delete(&uid, &key));
        }
        StorageCmd::KvScanPrefix { prefix, resp } => {
            with_uid!(resp, |uid| store.kv_scan_prefix(&uid, &prefix));
        }
        StorageCmd::GetStoragePaths { resp } => {
            with_uid!(resp, |uid| store.ensure_user_storage(&uid));
        }
        StorageCmd::Shutdown => { /* handled in run_loop */ }
    }
}

fn run_loop(store: LocalStore, rx: mpsc::Receiver<StorageCmd>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            StorageCmd::Shutdown => break,
            StorageCmd::UpsertRemoteMessageWithResult { input, resp } => {
                // Batch drain: collect consecutive upsert commands.
                let mut batch: Vec<(
                    UpsertRemoteMessageInput,
                    oneshot::Sender<Result<UpsertRemoteMessageResult>>,
                )> = Vec::with_capacity(32);
                batch.push((input, resp));
                let mut deferred: Vec<StorageCmd> = Vec::new();

                while batch.len() < 32 {
                    match rx.try_recv() {
                        Ok(StorageCmd::UpsertRemoteMessageWithResult { input, resp }) => {
                            batch.push((input, resp));
                        }
                        Ok(other) => {
                            deferred.push(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }

                flush_upsert_batch(&store, batch);

                for cmd in deferred {
                    dispatch_cmd_inner(&store, cmd, &rx);
                }
            }
            other => handle_single_cmd(&store, other),
        }
    }
}
