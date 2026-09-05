-- PrivChat 本地库（SQLite）—— 1.0.0 beta1 合并基线。
--
-- 🔴 **只给全新本地库用。** 它是「跑完 V20241119070909 到 V20260826090000 全部 22 条
-- 历史迁移之后」的结构快照，由那个库的 sqlite_master 导出，合并时与逐条跑的结果
-- 对拍过。
--
-- 历史迁移已删除，所以**装着旧版本的设备升上来时，refinery 会看到一个版本对不上
-- 的账本**。本地库是缓存不是真源（消息历史以服务端为权威，见
-- MESSAGE_HISTORY spec），所以 local_store 在这种情况下会删库重建，而不是带着
-- 一个说不清结构的库继续跑。见 `init_user_db`。
--
-- 加新东西请新增 V2、V3…，不要改这个文件：改了它，老设备的账本摘要就对不上，
-- 而 refinery 只按版本号判断，不会告诉你结构其实变了。

CREATE TABLE blacklist (
    blocked_user_id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE channel (
    channel_id INTEGER PRIMARY KEY,
    channel_type INT DEFAULT 0,
    -- 会话列表相关字段
    last_local_message_id INTEGER NOT NULL DEFAULT 0,
    last_msg_timestamp INTEGER,
    unread_count INT DEFAULT 0,
    last_msg_pts INTEGER NOT NULL DEFAULT 0,
    -- 频道信息字段
    show_nick INT DEFAULT 0,
    username TEXT NOT NULL DEFAULT '',
    channel_name TEXT NOT NULL DEFAULT '',
    channel_remark TEXT NOT NULL DEFAULT '',
    top INT DEFAULT 0,
    mute INT DEFAULT 0,
    save INT DEFAULT 0,
    forbidden INT DEFAULT 0,
    follow INT DEFAULT 0,
    is_deleted INT DEFAULT 0,
    receipt INT DEFAULT 0,
    status INT DEFAULT 1,
    invite INT DEFAULT 0,
    robot INT DEFAULT 0,
    version BIGINT DEFAULT 0,
    online SMALLINT NOT NULL DEFAULT 0,
    last_offline INTEGER NOT NULL DEFAULT 0,
    avatar TEXT NOT NULL DEFAULT '',
    category TEXT NOT NULL DEFAULT '',
    extra TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0,
    avatar_cache_key TEXT NOT NULL DEFAULT '',
    remote_extra TEXT DEFAULT '',
    flame SMALLINT NOT NULL DEFAULT 0,
    flame_second INTEGER NOT NULL DEFAULT 0,
    device_flag INTEGER NOT NULL DEFAULT 0,
    parent_channel_id INTEGER NOT NULL DEFAULT 0,
    parent_channel_type INT DEFAULT 0,
    last_msg_content TEXT NOT NULL DEFAULT ''
, peer_user_id INTEGER);

CREATE TABLE channel_extra (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type SMALLINT NOT NULL DEFAULT 0,
    browse_to UNSIGNED BIG INT NOT NULL DEFAULT 0,
    keep_pts UNSIGNED BIG INT NOT NULL DEFAULT 0,  -- ⭐ 改名：keep_message_seq -> keep_pts
    keep_offset_y INTEGER NOT NULL DEFAULT 0,
    draft VARCHAR(1000) NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    draft_updated_at UNSIGNED BIG INT NOT NULL DEFAULT 0
, peer_read_pts UNSIGNED BIG INT NOT NULL DEFAULT 0);

CREATE TABLE channel_member (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type INT DEFAULT 0,
    member_uid INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    member_name TEXT NOT NULL DEFAULT '',
    member_remark TEXT NOT NULL DEFAULT '',
    member_avatar TEXT NOT NULL DEFAULT '',
    member_invite_uid INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    role INT DEFAULT 0,
    status INT DEFAULT 1,
    is_deleted INT DEFAULT 0,
    robot INT DEFAULT 0,
    version BIGINT DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    updated_at INTEGER NOT NULL DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    extra TEXT NOT NULL DEFAULT '',
    -- 禁言功能
    forbidden_expiration_time BIGINT DEFAULT 0,
    -- 头像缓存键
    member_avatar_cache_key TEXT NOT NULL DEFAULT ''
);

CREATE TABLE friend (
    user_id         INTEGER PRIMARY KEY,
    tags            TEXT,
    is_pinned       INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
, version INTEGER NOT NULL DEFAULT 0, status INTEGER NOT NULL DEFAULT 1, is_outgoing INTEGER, request_message TEXT, request_source TEXT, request_source_id TEXT);

CREATE TABLE "group" (
    group_id        INTEGER PRIMARY KEY,
    name            TEXT,
    avatar          TEXT NOT NULL DEFAULT '',
    owner_id        INTEGER,
    is_dismissed    INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
, version INTEGER NOT NULL DEFAULT 0, member_count INTEGER NOT NULL DEFAULT 0);

CREATE TABLE group_member (
    group_id        INTEGER NOT NULL,
    user_id         INTEGER NOT NULL,
    role            INTEGER NOT NULL DEFAULT 2,
    status          INTEGER NOT NULL DEFAULT 0,
    alias           TEXT,
    is_muted        INTEGER NOT NULL DEFAULT 0,
    joined_at       INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0, version INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, user_id)
);

CREATE TABLE mention (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL,        -- 消息ID
    channel_id INTEGER NOT NULL,        -- 频道ID
    channel_type INTEGER NOT NULL,      -- 频道类型
    mentioned_user_id INTEGER NOT NULL, -- 被@的用户ID
    sender_id INTEGER NOT NULL,         -- 发送者ID
    is_mention_all INTEGER NOT NULL DEFAULT 0,  -- 是否@全体成员
    created_at INTEGER NOT NULL,        -- 创建时间（毫秒时间戳）
    is_read INTEGER NOT NULL DEFAULT 0, -- 是否已读
    UNIQUE(message_id, mentioned_user_id)  -- 确保同一消息对同一用户只记录一次
);

CREATE TABLE message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER,  -- 历史字段（迁移后会标准化为 server_message_id）
    pts BIGINT DEFAULT 0,  -- pts（对齐 Telegram，per-channel 顺序）
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type INT DEFAULT 0,
    timestamp INTEGER,
    from_uid INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    type INT DEFAULT 0,
    content TEXT NOT NULL DEFAULT '',
    status INT DEFAULT 0,
    voice_status INT DEFAULT 0,
    created_at INTEGER NOT NULL DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    updated_at INTEGER NOT NULL DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    searchable_word TEXT NOT NULL DEFAULT '',
    local_message_id INTEGER NOT NULL DEFAULT '',
    is_deleted INT DEFAULT 0,
    setting INT DEFAULT 0,
    order_seq BIGINT DEFAULT 0,
    extra TEXT NOT NULL DEFAULT '',
    -- 阅后即焚功能字段
    flame SMALLINT NOT NULL DEFAULT 0,
    flame_second INTEGER NOT NULL DEFAULT 0,
    viewed SMALLINT NOT NULL DEFAULT 0,
    viewed_at INTEGER NOT NULL DEFAULT 0,
    -- 话题支持
    topic_id TEXT NOT NULL DEFAULT '',
    -- 消息过期时间
    expire_time BIGINT DEFAULT 0,
    expire_timestamp BIGINT DEFAULT 0,
    -- 消息撤回状态
    revoked SMALLINT NOT NULL DEFAULT 0,
    revoked_at BIGINT DEFAULT 0,
    revoked_by INTEGER DEFAULT NULL  -- u64，使用 INTEGER 存储（可选，客户端可能不需要知道是谁撤回的）
, server_message_id INTEGER, mime_type TEXT DEFAULT NULL, media_downloaded SMALLINT NOT NULL DEFAULT 0, thumb_status SMALLINT NOT NULL DEFAULT 0, created_at_precision INTEGER NOT NULL DEFAULT 1);

CREATE TABLE message_extra (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER,  -- u64，使用 INTEGER 存储
    channel_id INTEGER,  -- u64，使用 INTEGER 存储
    channel_type SMALLINT NOT NULL DEFAULT 0,
    readed INTEGER NOT NULL DEFAULT 0,
    readed_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    revoke SMALLINT NOT NULL DEFAULT 0,
    revoker INTEGER,  -- u64，使用 INTEGER 存储
    extra_version BIGINT NOT NULL DEFAULT 0,
    is_mutual_deleted SMALLINT NOT NULL DEFAULT 0,
    content_edit TEXT,
    edited_at INTEGER NOT NULL DEFAULT 0,
    need_upload SMALLINT NOT NULL DEFAULT 0,
    -- 固定消息功能
    is_pinned INT DEFAULT 0
, delivered INTEGER NOT NULL DEFAULT 0, delivered_at INTEGER NOT NULL DEFAULT 0);

CREATE TABLE message_reaction (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type INT DEFAULT 0,
    uid INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    name TEXT NOT NULL DEFAULT '',
    emoji TEXT NOT NULL DEFAULT '',
    message_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    seq BIGINT DEFAULT 0,
    is_deleted INT DEFAULT 0,
    created_at INTEGER DEFAULT 0  -- 毫秒时间戳（BIGINT）
);

CREATE TABLE outbox (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Stable idempotency id for one logical operation. Unchanged across
    -- retries, sent to the server, which dedupes on
    -- (user_id, device_id, command_id).
    command_id        TEXT UNIQUE NOT NULL,
    command_type      TEXT NOT NULL,
    -- Message-class commands only; NULL for plain RPC commands.
    message_id        INTEGER,
    -- Optional: collapse repeated state changes for the same target while
    -- they are still unsent. like -> unlike -> like leaves one row.
    -- Distinct from command_id: that one stops the SERVER repeating work,
    -- this one stops the CLIENT sending work it has already superseded.
    coalesce_key      TEXT,
    channel_id        INTEGER,
    -- typed protocol bytes; never hand-rolled JSON.
    payload           BLOB NOT NULL,
    -- Attachment upload routing (was the sled file-queue index).
    route_key         TEXT,
    status            TEXT NOT NULL DEFAULT 'pending',
    retry_count       INTEGER NOT NULL DEFAULT 0,
    next_attempt_at   INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

CREATE TABLE reminder (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    reminder_id INTEGER NOT NULL DEFAULT 0,
    message_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    pts UNSIGNED BIG INT NOT NULL DEFAULT 0,  -- ⭐ 改名：message_seq -> pts
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type SMALLINT NOT NULL DEFAULT 0,
    uid INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    type INTEGER NOT NULL DEFAULT 0,
    text VARCHAR(255) NOT NULL DEFAULT '',
    data VARCHAR(1000) NOT NULL DEFAULT '',
    is_locate SMALLINT NOT NULL DEFAULT 0,
    version BIGINT NOT NULL DEFAULT 0,
    done SMALLINT NOT NULL DEFAULT 0,
    need_upload SMALLINT NOT NULL DEFAULT 0,
    -- 提醒发布者
    publisher INTEGER DEFAULT NULL  -- u64，使用 INTEGER 存储
);

CREATE TABLE robot (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    robot_id VARCHAR(40) NOT NULL DEFAULT '',
    status SMALLINT NOT NULL DEFAULT 1,
    version BIGINT NOT NULL DEFAULT 0,
    inline_on SMALLINT NOT NULL DEFAULT 0,
    placeholder VARCHAR(255) NOT NULL DEFAULT '',
    username VARCHAR(40) NOT NULL DEFAULT '',
    created_at INTEGER DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    updated_at TEXT
);

CREATE TABLE robot_menu (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    robot_id VARCHAR(40) NOT NULL DEFAULT '',
    cmd VARCHAR(100) NOT NULL DEFAULT '',
    remark VARCHAR(100) NOT NULL DEFAULT '',
    type VARCHAR(100) NOT NULL DEFAULT '',
    created_at INTEGER DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    updated_at TEXT
);

CREATE TABLE schema_version (
    version TEXT PRIMARY KEY,
    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE "user" (
    user_id         INTEGER PRIMARY KEY,
    username        TEXT,
    nickname        TEXT,
    alias           TEXT,
    avatar          TEXT NOT NULL DEFAULT '',
    user_type       INTEGER NOT NULL DEFAULT 0,
    is_deleted      INTEGER NOT NULL DEFAULT 0,
    channel_id      TEXT NOT NULL DEFAULT '',
    updated_at      INTEGER NOT NULL DEFAULT 0
, version INTEGER NOT NULL DEFAULT 0, avatar_local_path TEXT NOT NULL DEFAULT '', avatar_cached_url TEXT NOT NULL DEFAULT '');

CREATE INDEX bot_id_robot_menu_index ON robot_menu (robot_id);

CREATE INDEX channel_member_channel_index ON channel_member (channel_id, channel_type);

CREATE UNIQUE INDEX channel_member_index ON channel_member (channel_id, channel_type, member_uid);

CREATE UNIQUE INDEX chat_msg_reaction_index ON message_reaction (message_id, uid, emoji);

CREATE INDEX idx_blacklist_updated_at ON blacklist(updated_at);

CREATE INDEX idx_channel ON reminder (channel_id, channel_type);

CREATE UNIQUE INDEX idx_channel_channel_extra ON channel_extra (channel_id, channel_type);

CREATE INDEX idx_channel_member_role_index ON channel_member (channel_id, channel_type, role);

CREATE INDEX idx_channel_member_version ON channel_member (channel_id, channel_type, version);

CREATE INDEX idx_friend_status ON friend(status);

CREATE INDEX idx_friend_updated_at ON friend(updated_at);

CREATE INDEX idx_friend_user_id ON friend(user_id);

CREATE INDEX idx_friend_version ON friend(version DESC, user_id DESC);

CREATE INDEX idx_group_member_group ON group_member(group_id);

CREATE INDEX idx_group_member_status ON group_member(group_id, status);

CREATE INDEX idx_group_member_user ON group_member(user_id);

CREATE INDEX idx_group_member_version
    ON group_member(group_id, version DESC, user_id DESC);

CREATE INDEX idx_group_updated_at ON "group"(updated_at);

CREATE INDEX idx_group_version ON "group"(version DESC, group_id DESC);

CREATE INDEX idx_mention_channel_user ON mention(channel_id, channel_type, mentioned_user_id, is_read);

CREATE INDEX idx_mention_message ON mention(message_id);

CREATE INDEX idx_message_extra ON message_extra (channel_id, channel_type);

CREATE INDEX idx_outbox_coalesce
    ON outbox (coalesce_key) WHERE coalesce_key IS NOT NULL;

CREATE INDEX idx_outbox_due
    ON outbox (status, next_attempt_at, created_at);

CREATE UNIQUE INDEX idx_outbox_message
    ON outbox (message_id) WHERE message_id IS NOT NULL;

CREATE INDEX idx_user_channel_id ON "user"(channel_id) WHERE channel_id != '';

CREATE INDEX idx_user_updated_at ON "user"(updated_at);

CREATE INDEX idx_user_version ON "user"(version DESC, user_id DESC);

CREATE UNIQUE INDEX message_extra_idx ON message_extra (message_id);

CREATE INDEX msg_channel_index ON message (channel_id, channel_type);

CREATE INDEX msg_local_message_id_index ON message (local_message_id);

CREATE UNIQUE INDEX robot_id_robot_index ON robot (robot_id);

CREATE INDEX searchable_word_index ON message (searchable_word);

CREATE INDEX type_index ON message (type);

CREATE UNIQUE INDEX uidx_reminder ON reminder (reminder_id);

CREATE UNIQUE INDEX uniq_message_server_id
ON message (server_message_id)
WHERE server_message_id IS NOT NULL AND server_message_id > 0;

CREATE INDEX version_reminder ON reminder (version);
