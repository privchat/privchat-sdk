-- =============================================
-- PrivChat SDK 最终数据库架构
-- 这是执行完所有迁移文件后的完整表结构
-- 版本：20241119070909
-- =============================================

-- 消息表 - 核心消息存储（支持群聊/频道/1对1）
CREATE TABLE IF NOT EXISTS message (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER,  -- u64，使用 INTEGER 存储（SQLite 支持 i64，u64 值通常不会超过 i64::MAX）
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
);

-- 消息表索引（local_message_id 非唯一，允许多行存 0，协议字段仅存库）
CREATE INDEX IF NOT EXISTS msg_channel_index ON message (channel_id, channel_type);
CREATE INDEX IF NOT EXISTS msg_local_message_id_index ON message (local_message_id);
CREATE INDEX IF NOT EXISTS searchable_word_index ON message (searchable_word);
CREATE INDEX IF NOT EXISTS type_index ON message (type);

-- 频道/群组表 - 1 channel = 1 row，channel_id 为主键从源头禁止重复（行业标准）
CREATE TABLE IF NOT EXISTS channel (
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
);

-- 频道成员表 - 群组成员管理
CREATE TABLE IF NOT EXISTS channel_member (
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

-- 频道成员表索引
CREATE UNIQUE INDEX IF NOT EXISTS channel_member_index ON channel_member (channel_id, channel_type, member_uid);
CREATE INDEX IF NOT EXISTS idx_channel_member_version ON channel_member (channel_id, channel_type, version);
CREATE INDEX IF NOT EXISTS channel_member_channel_index ON channel_member (channel_id, channel_type);
CREATE INDEX IF NOT EXISTS idx_channel_member_role_index ON channel_member (channel_id, channel_type, role);

-- 消息反应表 - 消息表情回复
CREATE TABLE IF NOT EXISTS message_reaction (
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

-- 消息反应表索引
CREATE UNIQUE INDEX IF NOT EXISTS chat_msg_reaction_index ON message_reaction (message_id, uid, emoji);

-- 机器人表
CREATE TABLE IF NOT EXISTS robot (
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

-- 机器人表索引
CREATE UNIQUE INDEX IF NOT EXISTS robot_id_robot_index ON robot (robot_id);

-- 机器人菜单表
CREATE TABLE IF NOT EXISTS robot_menu (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    robot_id VARCHAR(40) NOT NULL DEFAULT '',
    cmd VARCHAR(100) NOT NULL DEFAULT '',
    remark VARCHAR(100) NOT NULL DEFAULT '',
    type VARCHAR(100) NOT NULL DEFAULT '',
    created_at INTEGER DEFAULT 0,  -- 毫秒时间戳（BIGINT）
    updated_at TEXT
);

-- 机器人菜单表索引
CREATE INDEX IF NOT EXISTS bot_id_robot_menu_index ON robot_menu (robot_id);

-- 消息扩展信息表 - 已读状态、撤回、编辑等
CREATE TABLE IF NOT EXISTS message_extra (
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
);

-- 消息扩展表索引
CREATE UNIQUE INDEX IF NOT EXISTS message_extra_idx ON message_extra (message_id);
CREATE INDEX IF NOT EXISTS idx_message_extra ON message_extra (channel_id, channel_type);

-- 提醒表 - 消息提醒功能
CREATE TABLE IF NOT EXISTS reminder (
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

-- 提醒表索引
CREATE INDEX IF NOT EXISTS idx_channel ON reminder (channel_id, channel_type);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_reminder ON reminder (reminder_id);
CREATE INDEX IF NOT EXISTS version_reminder ON reminder (version);

-- 会话扩展信息表 - 草稿、阅读位置等
CREATE TABLE IF NOT EXISTS channel_extra (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL DEFAULT 0,  -- u64，使用 INTEGER 存储
    channel_type SMALLINT NOT NULL DEFAULT 0,
    browse_to UNSIGNED BIG INT NOT NULL DEFAULT 0,
    keep_pts UNSIGNED BIG INT NOT NULL DEFAULT 0,  -- ⭐ 改名：keep_message_seq -> keep_pts
    keep_offset_y INTEGER NOT NULL DEFAULT 0,
    draft VARCHAR(1000) NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    draft_updated_at UNSIGNED BIG INT NOT NULL DEFAULT 0
);

-- 会话扩展表索引
CREATE UNIQUE INDEX IF NOT EXISTS idx_channel_channel_extra ON channel_extra (channel_id, channel_type);

-- =============================================
-- Entity Model V1: user, group, group_member, friend
-- =============================================

-- 1. user 表（好友/群成员/陌生人共用）
CREATE TABLE IF NOT EXISTS "user" (
    user_id         INTEGER PRIMARY KEY,
    username        TEXT,
    nickname        TEXT,
    alias           TEXT,
    avatar          TEXT NOT NULL DEFAULT '',
    user_type       INTEGER NOT NULL DEFAULT 0,
    is_deleted      INTEGER NOT NULL DEFAULT 0,
    channel_id      TEXT NOT NULL DEFAULT '',
    updated_at      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_user_updated_at ON "user"(updated_at);
CREATE INDEX IF NOT EXISTS idx_user_channel_id ON "user"(channel_id) WHERE channel_id != '';

-- 2. group 表
CREATE TABLE IF NOT EXISTS "group" (
    group_id        INTEGER PRIMARY KEY,
    name            TEXT,
    avatar          TEXT NOT NULL DEFAULT '',
    owner_id        INTEGER,
    is_dismissed    INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_group_updated_at ON "group"(updated_at);

-- 3. group_member 表
CREATE TABLE IF NOT EXISTS group_member (
    group_id        INTEGER NOT NULL,
    user_id         INTEGER NOT NULL,
    role            INTEGER NOT NULL DEFAULT 2,
    status          INTEGER NOT NULL DEFAULT 0,
    alias           TEXT,
    is_muted        INTEGER NOT NULL DEFAULT 0,
    joined_at       INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_group_member_group ON group_member(group_id);
CREATE INDEX IF NOT EXISTS idx_group_member_user ON group_member(user_id);
CREATE INDEX IF NOT EXISTS idx_group_member_status ON group_member(group_id, status);

-- 4. friend 表（仅关系，用户信息在 user）
CREATE TABLE IF NOT EXISTS friend (
    user_id         INTEGER PRIMARY KEY,
    tags            TEXT,
    is_pinned       INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_friend_user_id ON friend(user_id);
CREATE INDEX IF NOT EXISTS idx_friend_updated_at ON friend(updated_at);

-- =============================================
-- @提及表 - 管理消息中的@提及记录
-- =============================================
CREATE TABLE IF NOT EXISTS mention (
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

-- @提及表索引
CREATE INDEX IF NOT EXISTS idx_mention_channel_user ON mention(channel_id, channel_type, mentioned_user_id, is_read);
CREATE INDEX IF NOT EXISTS idx_mention_message ON mention(message_id);

-- =============================================
-- 数据库版本信息表
-- =============================================
CREATE TABLE IF NOT EXISTS schema_version (
    version TEXT PRIMARY KEY,
    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- 插入当前版本信息
INSERT INTO schema_version (version) VALUES ('20241119070909');
