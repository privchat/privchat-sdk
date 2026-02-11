use std::sync::mpsc;
use std::thread;

use tokio::sync::oneshot;

use crate::local_store::{LocalStore, StoragePaths};
use crate::{
    Error, LoginResult, MentionInput, NewMessage, Result, SessionSnapshot, StoredBlacklistEntry,
    StoredChannel, StoredChannelExtra, StoredChannelMember, StoredFriend, StoredGroup,
    StoredGroupMember, StoredMessage, StoredMessageExtra, StoredMessageReaction, StoredReminder,
    StoredUser, UnreadMentionCount, UpsertBlacklistInput, UpsertChannelExtraInput,
    UpsertChannelInput, UpsertChannelMemberInput, UpsertFriendInput, UpsertGroupInput,
    UpsertGroupMemberInput, UpsertMessageReactionInput, UpsertReminderInput, UpsertUserInput,
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
    ClearCurrentUid {
        resp: oneshot::Sender<Result<()>>,
    },
    NormalQueuePush {
        uid: String,
        message_id: u64,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<u64>>,
    },
    NormalQueuePeek {
        uid: String,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<(u64, Vec<u8>)>>>,
    },
    NormalQueueAck {
        uid: String,
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    SelectFileQueue {
        uid: String,
        route_key: String,
        resp: oneshot::Sender<Result<usize>>,
    },
    FileQueuePush {
        uid: String,
        queue_index: usize,
        message_id: u64,
        payload: Vec<u8>,
        resp: oneshot::Sender<Result<u64>>,
    },
    FileQueuePeek {
        uid: String,
        queue_index: usize,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<(u64, Vec<u8>)>>>,
    },
    FileQueueAck {
        uid: String,
        queue_index: usize,
        message_ids: Vec<u64>,
        resp: oneshot::Sender<Result<usize>>,
    },
    CreateLocalMessage {
        uid: String,
        input: NewMessage,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetMessageById {
        uid: String,
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessage>>>,
    },
    ListMessages {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessage>>>,
    },
    UpsertChannel {
        uid: String,
        input: UpsertChannelInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelById {
        uid: String,
        channel_id: u64,
        resp: oneshot::Sender<Result<Option<StoredChannel>>>,
    },
    ListChannels {
        uid: String,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannel>>>,
    },
    UpsertChannelExtra {
        uid: String,
        input: UpsertChannelExtraInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelExtra {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<Option<StoredChannelExtra>>>,
    },
    MarkMessageSent {
        uid: String,
        message_id: u64,
        server_message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateLocalMessageId {
        uid: String,
        message_id: u64,
        local_message_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpdateMessageStatus {
        uid: String,
        message_id: u64,
        status: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRead {
        uid: String,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessageRevoke {
        uid: String,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
        resp: oneshot::Sender<Result<()>>,
    },
    EditMessage {
        uid: String,
        message_id: u64,
        content: String,
        edited_at: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    SetMessagePinned {
        uid: String,
        message_id: u64,
        is_pinned: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    GetMessageExtra {
        uid: String,
        message_id: u64,
        resp: oneshot::Sender<Result<Option<StoredMessageExtra>>>,
    },
    MarkChannelRead {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<()>>,
    },
    GetChannelUnreadCount {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        resp: oneshot::Sender<Result<i32>>,
    },
    GetTotalUnreadCount {
        uid: String,
        exclude_muted: bool,
        resp: oneshot::Sender<Result<i32>>,
    },
    UpsertUser {
        uid: String,
        input: UpsertUserInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetUserById {
        uid: String,
        user_id: u64,
        resp: oneshot::Sender<Result<Option<StoredUser>>>,
    },
    ListUsersByIds {
        uid: String,
        user_ids: Vec<u64>,
        resp: oneshot::Sender<Result<Vec<StoredUser>>>,
    },
    UpsertFriend {
        uid: String,
        input: UpsertFriendInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteFriend {
        uid: String,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListFriends {
        uid: String,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredFriend>>>,
    },
    UpsertBlacklistEntry {
        uid: String,
        input: UpsertBlacklistInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteBlacklistEntry {
        uid: String,
        blocked_user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListBlacklistEntries {
        uid: String,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredBlacklistEntry>>>,
    },
    UpsertGroup {
        uid: String,
        input: UpsertGroupInput,
        resp: oneshot::Sender<Result<()>>,
    },
    GetGroupById {
        uid: String,
        group_id: u64,
        resp: oneshot::Sender<Result<Option<StoredGroup>>>,
    },
    ListGroups {
        uid: String,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroup>>>,
    },
    UpsertGroupMember {
        uid: String,
        input: UpsertGroupMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    DeleteGroupMember {
        uid: String,
        group_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    ListGroupMembers {
        uid: String,
        group_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredGroupMember>>>,
    },
    UpsertChannelMember {
        uid: String,
        input: UpsertChannelMemberInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListChannelMembers {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredChannelMember>>>,
    },
    DeleteChannelMember {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    UpsertMessageReaction {
        uid: String,
        input: UpsertMessageReactionInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListMessageReactions {
        uid: String,
        message_id: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredMessageReaction>>>,
    },
    RecordMention {
        uid: String,
        input: MentionInput,
        resp: oneshot::Sender<Result<u64>>,
    },
    GetUnreadMentionCount {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<i32>>,
    },
    ListUnreadMentionMessageIds {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
        resp: oneshot::Sender<Result<Vec<u64>>>,
    },
    MarkMentionRead {
        uid: String,
        message_id: u64,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    MarkAllMentionsRead {
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        resp: oneshot::Sender<Result<()>>,
    },
    GetAllUnreadMentionCounts {
        uid: String,
        user_id: u64,
        resp: oneshot::Sender<Result<Vec<UnreadMentionCount>>>,
    },
    UpsertReminder {
        uid: String,
        input: UpsertReminderInput,
        resp: oneshot::Sender<Result<()>>,
    },
    ListPendingReminders {
        uid: String,
        reminder_uid: u64,
        limit: usize,
        offset: usize,
        resp: oneshot::Sender<Result<Vec<StoredReminder>>>,
    },
    MarkReminderDone {
        uid: String,
        reminder_id: u64,
        done: bool,
        resp: oneshot::Sender<Result<()>>,
    },
    KvPut {
        uid: String,
        key: String,
        value: Vec<u8>,
        resp: oneshot::Sender<Result<()>>,
    },
    KvGet {
        uid: String,
        key: String,
        resp: oneshot::Sender<Result<Option<Vec<u8>>>>,
    },
    GetStoragePaths {
        uid: String,
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

    pub async fn clear_current_uid(&self) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ClearCurrentUid { resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_push(
        &self,
        uid: String,
        message_id: u64,
        payload: Vec<u8>,
    ) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueuePush {
                uid,
                message_id,
                payload,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_peek(
        &self,
        uid: String,
        limit: usize,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueuePeek {
                uid,
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn normal_queue_ack(&self, uid: String, message_ids: Vec<u64>) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::NormalQueueAck {
                uid,
                message_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn select_file_queue(&self, uid: String, route_key: String) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SelectFileQueue {
                uid,
                route_key,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_push(
        &self,
        uid: String,
        queue_index: usize,
        message_id: u64,
        payload: Vec<u8>,
    ) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueuePush {
                uid,
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
        uid: String,
        queue_index: usize,
        limit: usize,
    ) -> Result<Vec<(u64, Vec<u8>)>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueuePeek {
                uid,
                queue_index,
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn file_queue_ack(
        &self,
        uid: String,
        queue_index: usize,
        message_ids: Vec<u64>,
    ) -> Result<usize> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::FileQueueAck {
                uid,
                queue_index,
                message_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn create_local_message(&self, uid: String, input: NewMessage) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::CreateLocalMessage {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_by_id(
        &self,
        uid: String,
        message_id: u64,
    ) -> Result<Option<StoredMessage>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetMessageById {
                uid,
                message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_messages(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessage>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListMessages {
                uid,
                channel_id,
                channel_type,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel(&self, uid: String, input: UpsertChannelInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannel {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_by_id(
        &self,
        uid: String,
        channel_id: u64,
    ) -> Result<Option<StoredChannel>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelById {
                uid,
                channel_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channels(
        &self,
        uid: String,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannel>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListChannels {
                uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_extra(
        &self,
        uid: String,
        input: UpsertChannelExtraInput,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannelExtra {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_extra(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<Option<StoredChannelExtra>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelExtra {
                uid,
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_message_sent(
        &self,
        uid: String,
        message_id: u64,
        server_message_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkMessageSent {
                uid,
                message_id,
                server_message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn update_local_message_id(
        &self,
        uid: String,
        message_id: u64,
        local_message_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpdateLocalMessageId {
                uid,
                message_id,
                local_message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn update_message_status(
        &self,
        uid: String,
        message_id: u64,
        status: i32,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpdateMessageStatus {
                uid,
                message_id,
                status,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_read(
        &self,
        uid: String,
        message_id: u64,
        channel_id: u64,
        channel_type: i32,
        is_read: bool,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessageRead {
                uid,
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
        uid: String,
        message_id: u64,
        revoked: bool,
        revoker: Option<u64>,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessageRevoke {
                uid,
                message_id,
                revoked,
                revoker,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn edit_message(
        &self,
        uid: String,
        message_id: u64,
        content: &str,
        edited_at: i32,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::EditMessage {
                uid,
                message_id,
                content: content.to_string(),
                edited_at,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn set_message_pinned(
        &self,
        uid: String,
        message_id: u64,
        is_pinned: bool,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::SetMessagePinned {
                uid,
                message_id,
                is_pinned,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_message_extra(
        &self,
        uid: String,
        message_id: u64,
    ) -> Result<Option<StoredMessageExtra>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetMessageExtra {
                uid,
                message_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_channel_read(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkChannelRead {
                uid,
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_channel_unread_count(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
    ) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetChannelUnreadCount {
                uid,
                channel_id,
                channel_type,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_total_unread_count(&self, uid: String, exclude_muted: bool) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetTotalUnreadCount {
                uid,
                exclude_muted,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_user(&self, uid: String, input: UpsertUserInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertUser {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_user_by_id(&self, uid: String, user_id: u64) -> Result<Option<StoredUser>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetUserById {
                uid,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_users_by_ids(
        &self,
        uid: String,
        user_ids: Vec<u64>,
    ) -> Result<Vec<StoredUser>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListUsersByIds {
                uid,
                user_ids,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_friend(&self, uid: String, input: UpsertFriendInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertFriend {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_friend(&self, uid: String, user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteFriend {
                uid,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_friends(
        &self,
        uid: String,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredFriend>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListFriends {
                uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_blacklist_entry(
        &self,
        uid: String,
        input: UpsertBlacklistInput,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertBlacklistEntry {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_blacklist_entry(&self, uid: String, blocked_user_id: u64) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteBlacklistEntry {
                uid,
                blocked_user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_blacklist_entries(
        &self,
        uid: String,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredBlacklistEntry>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListBlacklistEntries {
                uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group(&self, uid: String, input: UpsertGroupInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertGroup {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_group_by_id(&self, uid: String, group_id: u64) -> Result<Option<StoredGroup>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetGroupById {
                uid,
                group_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_groups(
        &self,
        uid: String,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroup>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListGroups {
                uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_group_member(
        &self,
        uid: String,
        input: UpsertGroupMemberInput,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertGroupMember {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_channel_member(
        &self,
        uid: String,
        input: UpsertChannelMemberInput,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertChannelMember {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_channel_members(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredChannelMember>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListChannelMembers {
                uid,
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
        uid: String,
        channel_id: u64,
        channel_type: i32,
        member_uid: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteChannelMember {
                uid,
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
        uid: String,
        group_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredGroupMember>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListGroupMembers {
                uid,
                group_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn delete_group_member(
        &self,
        uid: String,
        group_id: u64,
        user_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::DeleteGroupMember {
                uid,
                group_id,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_message_reaction(
        &self,
        uid: String,
        input: UpsertMessageReactionInput,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertMessageReaction {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_message_reactions(
        &self,
        uid: String,
        message_id: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredMessageReaction>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListMessageReactions {
                uid,
                message_id,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn record_mention(&self, uid: String, input: MentionInput) -> Result<u64> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::RecordMention {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_unread_mention_count(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<i32> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetUnreadMentionCount {
                uid,
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
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
        limit: usize,
    ) -> Result<Vec<u64>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListUnreadMentionMessageIds {
                uid,
                channel_id,
                channel_type,
                user_id,
                limit,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_mention_read(
        &self,
        uid: String,
        message_id: u64,
        user_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkMentionRead {
                uid,
                message_id,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_all_mentions_read(
        &self,
        uid: String,
        channel_id: u64,
        channel_type: i32,
        user_id: u64,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkAllMentionsRead {
                uid,
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
        uid: String,
        user_id: u64,
    ) -> Result<Vec<UnreadMentionCount>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetAllUnreadMentionCounts {
                uid,
                user_id,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn upsert_reminder(&self, uid: String, input: UpsertReminderInput) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::UpsertReminder {
                uid,
                input,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn list_pending_reminders(
        &self,
        uid: String,
        reminder_uid: u64,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StoredReminder>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::ListPendingReminders {
                uid,
                reminder_uid,
                limit,
                offset,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn mark_reminder_done(
        &self,
        uid: String,
        reminder_id: u64,
        done: bool,
    ) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::MarkReminderDone {
                uid,
                reminder_id,
                done,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_put(&self, uid: String, key: String, value: Vec<u8>) -> Result<()> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvPut {
                uid,
                key,
                value,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn kv_get(&self, uid: String, key: String) -> Result<Option<Vec<u8>>> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::KvGet {
                uid,
                key,
                resp: resp_tx,
            })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub async fn get_storage_paths(&self, uid: String) -> Result<StoragePaths> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(StorageCmd::GetStoragePaths { uid, resp: resp_tx })
            .map_err(|_| Error::ActorClosed)?;
        resp_rx.await.map_err(|_| Error::ActorClosed)?
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(StorageCmd::Shutdown);
    }
}

fn run_loop(store: LocalStore, rx: mpsc::Receiver<StorageCmd>) {
    while let Ok(cmd) = rx.recv() {
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
            StorageCmd::ClearCurrentUid { resp } => {
                let _ = resp.send(store.clear_current_uid());
            }
            StorageCmd::NormalQueuePush {
                uid,
                message_id,
                payload,
                resp,
            } => {
                let _ = resp.send(store.normal_queue_push(&uid, message_id, &payload));
            }
            StorageCmd::NormalQueuePeek { uid, limit, resp } => {
                let _ = resp.send(store.normal_queue_peek(&uid, limit));
            }
            StorageCmd::NormalQueueAck {
                uid,
                message_ids,
                resp,
            } => {
                let _ = resp.send(store.normal_queue_ack(&uid, &message_ids));
            }
            StorageCmd::SelectFileQueue {
                uid,
                route_key,
                resp,
            } => {
                let _ = resp.send(store.select_file_queue(&uid, &route_key));
            }
            StorageCmd::FileQueuePush {
                uid,
                queue_index,
                message_id,
                payload,
                resp,
            } => {
                let _ = resp.send(store.file_queue_push(&uid, queue_index, message_id, &payload));
            }
            StorageCmd::FileQueuePeek {
                uid,
                queue_index,
                limit,
                resp,
            } => {
                let _ = resp.send(store.file_queue_peek(&uid, queue_index, limit));
            }
            StorageCmd::FileQueueAck {
                uid,
                queue_index,
                message_ids,
                resp,
            } => {
                let _ = resp.send(store.file_queue_ack(&uid, queue_index, &message_ids));
            }
            StorageCmd::CreateLocalMessage { uid, input, resp } => {
                let _ = resp.send(store.create_local_message(&uid, &input));
            }
            StorageCmd::GetMessageById {
                uid,
                message_id,
                resp,
            } => {
                let _ = resp.send(store.get_message_by_id(&uid, message_id));
            }
            StorageCmd::ListMessages {
                uid,
                channel_id,
                channel_type,
                limit,
                offset,
                resp,
            } => {
                let _ =
                    resp.send(store.list_messages(&uid, channel_id, channel_type, limit, offset));
            }
            StorageCmd::UpsertChannel { uid, input, resp } => {
                let _ = resp.send(store.upsert_channel(&uid, &input));
            }
            StorageCmd::GetChannelById {
                uid,
                channel_id,
                resp,
            } => {
                let _ = resp.send(store.get_channel_by_id(&uid, channel_id));
            }
            StorageCmd::ListChannels {
                uid,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_channels(&uid, limit, offset));
            }
            StorageCmd::UpsertChannelExtra { uid, input, resp } => {
                let _ = resp.send(store.upsert_channel_extra(&uid, &input));
            }
            StorageCmd::GetChannelExtra {
                uid,
                channel_id,
                channel_type,
                resp,
            } => {
                let _ = resp.send(store.get_channel_extra(&uid, channel_id, channel_type));
            }
            StorageCmd::MarkMessageSent {
                uid,
                message_id,
                server_message_id,
                resp,
            } => {
                let _ = resp.send(store.mark_message_sent(&uid, message_id, server_message_id));
            }
            StorageCmd::UpdateLocalMessageId {
                uid,
                message_id,
                local_message_id,
                resp,
            } => {
                let _ =
                    resp.send(store.update_local_message_id(&uid, message_id, local_message_id));
            }
            StorageCmd::UpdateMessageStatus {
                uid,
                message_id,
                status,
                resp,
            } => {
                let _ = resp.send(store.update_message_status(&uid, message_id, status));
            }
            StorageCmd::SetMessageRead {
                uid,
                message_id,
                channel_id,
                channel_type,
                is_read,
                resp,
            } => {
                let _ = resp.send(store.set_message_read(
                    &uid,
                    message_id,
                    channel_id,
                    channel_type,
                    is_read,
                ));
            }
            StorageCmd::SetMessageRevoke {
                uid,
                message_id,
                revoked,
                revoker,
                resp,
            } => {
                let _ = resp.send(store.set_message_revoke(&uid, message_id, revoked, revoker));
            }
            StorageCmd::EditMessage {
                uid,
                message_id,
                content,
                edited_at,
                resp,
            } => {
                let _ = resp.send(store.edit_message(&uid, message_id, &content, edited_at));
            }
            StorageCmd::SetMessagePinned {
                uid,
                message_id,
                is_pinned,
                resp,
            } => {
                let _ = resp.send(store.set_message_pinned(&uid, message_id, is_pinned));
            }
            StorageCmd::GetMessageExtra {
                uid,
                message_id,
                resp,
            } => {
                let _ = resp.send(store.get_message_extra(&uid, message_id));
            }
            StorageCmd::MarkChannelRead {
                uid,
                channel_id,
                channel_type,
                resp,
            } => {
                let _ = resp.send(store.mark_channel_read(&uid, channel_id, channel_type));
            }
            StorageCmd::GetChannelUnreadCount {
                uid,
                channel_id,
                channel_type,
                resp,
            } => {
                let _ = resp.send(store.get_channel_unread_count(&uid, channel_id, channel_type));
            }
            StorageCmd::GetTotalUnreadCount {
                uid,
                exclude_muted,
                resp,
            } => {
                let _ = resp.send(store.get_total_unread_count(&uid, exclude_muted));
            }
            StorageCmd::UpsertUser { uid, input, resp } => {
                let _ = resp.send(store.upsert_user(&uid, &input));
            }
            StorageCmd::GetUserById { uid, user_id, resp } => {
                let _ = resp.send(store.get_user_by_id(&uid, user_id));
            }
            StorageCmd::ListUsersByIds {
                uid,
                user_ids,
                resp,
            } => {
                let _ = resp.send(store.list_users_by_ids(&uid, &user_ids));
            }
            StorageCmd::UpsertFriend { uid, input, resp } => {
                let _ = resp.send(store.upsert_friend(&uid, &input));
            }
            StorageCmd::DeleteFriend { uid, user_id, resp } => {
                let _ = resp.send(store.delete_friend(&uid, user_id));
            }
            StorageCmd::ListFriends {
                uid,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_friends(&uid, limit, offset));
            }
            StorageCmd::UpsertBlacklistEntry { uid, input, resp } => {
                let _ = resp.send(store.upsert_blacklist_entry(&uid, &input));
            }
            StorageCmd::DeleteBlacklistEntry {
                uid,
                blocked_user_id,
                resp,
            } => {
                let _ = resp.send(store.delete_blacklist_entry(&uid, blocked_user_id));
            }
            StorageCmd::ListBlacklistEntries {
                uid,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_blacklist_entries(&uid, limit, offset));
            }
            StorageCmd::UpsertGroup { uid, input, resp } => {
                let _ = resp.send(store.upsert_group(&uid, &input));
            }
            StorageCmd::GetGroupById {
                uid,
                group_id,
                resp,
            } => {
                let _ = resp.send(store.get_group_by_id(&uid, group_id));
            }
            StorageCmd::ListGroups {
                uid,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_groups(&uid, limit, offset));
            }
            StorageCmd::UpsertGroupMember { uid, input, resp } => {
                let _ = resp.send(store.upsert_group_member(&uid, &input));
            }
            StorageCmd::DeleteGroupMember {
                uid,
                group_id,
                user_id,
                resp,
            } => {
                let _ = resp.send(store.delete_group_member(&uid, group_id, user_id));
            }
            StorageCmd::ListGroupMembers {
                uid,
                group_id,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_group_members(&uid, group_id, limit, offset));
            }
            StorageCmd::UpsertChannelMember { uid, input, resp } => {
                let _ = resp.send(store.upsert_channel_member(&uid, &input));
            }
            StorageCmd::ListChannelMembers {
                uid,
                channel_id,
                channel_type,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_channel_members(
                    &uid,
                    channel_id,
                    channel_type,
                    limit,
                    offset,
                ));
            }
            StorageCmd::DeleteChannelMember {
                uid,
                channel_id,
                channel_type,
                member_uid,
                resp,
            } => {
                let _ = resp.send(store.delete_channel_member(
                    &uid,
                    channel_id,
                    channel_type,
                    member_uid,
                ));
            }
            StorageCmd::UpsertMessageReaction { uid, input, resp } => {
                let _ = resp.send(store.upsert_message_reaction(&uid, &input));
            }
            StorageCmd::ListMessageReactions {
                uid,
                message_id,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_message_reactions(&uid, message_id, limit, offset));
            }
            StorageCmd::RecordMention { uid, input, resp } => {
                let _ = resp.send(store.record_mention(&uid, &input));
            }
            StorageCmd::GetUnreadMentionCount {
                uid,
                channel_id,
                channel_type,
                user_id,
                resp,
            } => {
                let _ = resp.send(store.get_unread_mention_count(
                    &uid,
                    channel_id,
                    channel_type,
                    user_id,
                ));
            }
            StorageCmd::ListUnreadMentionMessageIds {
                uid,
                channel_id,
                channel_type,
                user_id,
                limit,
                resp,
            } => {
                let _ = resp.send(store.list_unread_mention_message_ids(
                    &uid,
                    channel_id,
                    channel_type,
                    user_id,
                    limit,
                ));
            }
            StorageCmd::MarkMentionRead {
                uid,
                message_id,
                user_id,
                resp,
            } => {
                let _ = resp.send(store.mark_mention_read(&uid, message_id, user_id));
            }
            StorageCmd::MarkAllMentionsRead {
                uid,
                channel_id,
                channel_type,
                user_id,
                resp,
            } => {
                let _ = resp.send(store.mark_all_mentions_read(
                    &uid,
                    channel_id,
                    channel_type,
                    user_id,
                ));
            }
            StorageCmd::GetAllUnreadMentionCounts { uid, user_id, resp } => {
                let _ = resp.send(store.get_all_unread_mention_counts(&uid, user_id));
            }
            StorageCmd::UpsertReminder { uid, input, resp } => {
                let _ = resp.send(store.upsert_reminder(&uid, &input));
            }
            StorageCmd::ListPendingReminders {
                uid,
                reminder_uid,
                limit,
                offset,
                resp,
            } => {
                let _ = resp.send(store.list_pending_reminders(&uid, reminder_uid, limit, offset));
            }
            StorageCmd::MarkReminderDone {
                uid,
                reminder_id,
                done,
                resp,
            } => {
                let _ = resp.send(store.mark_reminder_done(&uid, reminder_id, done));
            }
            StorageCmd::KvPut {
                uid,
                key,
                value,
                resp,
            } => {
                let _ = resp.send(store.kv_put(&uid, &key, &value));
            }
            StorageCmd::KvGet { uid, key, resp } => {
                let _ = resp.send(store.kv_get(&uid, &key));
            }
            StorageCmd::GetStoragePaths { uid, resp } => {
                let _ = resp.send(store.ensure_user_storage(&uid));
            }
            StorageCmd::Shutdown => break,
        }
    }
}
