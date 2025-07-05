-- =============================================
-- PrivChat SDK 最终数据库架构
-- 这是执行完所有迁移文件后的完整表结构
-- 版本：20240101000001
-- =============================================

-- 消息表 - 核心消息存储（支持群聊/频道/1对1）
CREATE TABLE message (
    client_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT,
    message_seq BIGINT DEFAULT 0,
    channel_id TEXT NOT NULL DEFAULT '',
    channel_type INT DEFAULT 0,
    timestamp INTEGER,
    from_uid TEXT NOT NULL DEFAULT '',
    type INT DEFAULT 0,
    content TEXT NOT NULL DEFAULT '',
    status INT DEFAULT 0,
    voice_status INT DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    searchable_word TEXT NOT NULL DEFAULT '',
    client_msg_no TEXT NOT NULL DEFAULT '',
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
    expire_timestamp BIGINT DEFAULT 0
);

-- 消息表索引
CREATE INDEX msg_channel_index ON message (channel_id, channel_type);
CREATE UNIQUE INDEX IF NOT EXISTS msg_client_msg_no_index ON message (client_msg_no);
CREATE INDEX searchable_word_index ON message (searchable_word);
CREATE INDEX type_index ON message (type);

-- 会话表 - 会话列表管理
CREATE TABLE conversation (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL DEFAULT '',
    channel_type INT DEFAULT 0,
    last_client_msg_no TEXT NOT NULL DEFAULT '',
    last_msg_timestamp INTEGER,
    unread_count INT DEFAULT 0,
    is_deleted INT DEFAULT 0,
    version BIGINT DEFAULT 0,
    extra TEXT NOT NULL DEFAULT '',
    -- 会话最后消息序号
    last_msg_seq BIGINT NOT NULL DEFAULT 0,
    -- 父频道支持（线程/回复）
    parent_channel_id TEXT NOT NULL DEFAULT '',
    parent_channel_type INT DEFAULT 0
);

-- 会话表索引
CREATE UNIQUE INDEX IF NOT EXISTS conversation_msg_index_channel ON conversation (channel_id, channel_type);
CREATE INDEX conversation_msg_index_time ON conversation (last_msg_timestamp);

-- 频道/群组表 - 频道和群组信息
CREATE TABLE channel (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL DEFAULT '',
    channel_type INT DEFAULT 0,
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
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    -- 头像缓存键
    avatar_cache_key TEXT NOT NULL DEFAULT '',
    -- 远程扩展信息
    remote_extra TEXT DEFAULT '',
    -- 阅后即焚功能
    flame SMALLINT NOT NULL DEFAULT 0,
    flame_second INTEGER NOT NULL DEFAULT 0,
    -- 设备标识
    device_flag INTEGER NOT NULL DEFAULT 0,
    -- 父频道支持（线程/回复）
    parent_channel_id TEXT NOT NULL DEFAULT '',
    parent_channel_type INT DEFAULT 0
);

-- 频道表索引
CREATE UNIQUE INDEX IF NOT EXISTS channel_index ON channel (channel_id, channel_type);

-- 频道成员表 - 群组成员管理
CREATE TABLE channel_members (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL DEFAULT '',
    channel_type INT DEFAULT 0,
    member_uid TEXT NOT NULL DEFAULT '',
    member_name TEXT NOT NULL DEFAULT '',
    member_remark TEXT NOT NULL DEFAULT '',
    member_avatar TEXT NOT NULL DEFAULT '',
    member_invite_uid TEXT NOT NULL DEFAULT '',
    role INT DEFAULT 0,
    status INT DEFAULT 1,
    is_deleted INT DEFAULT 0,
    robot INT DEFAULT 0,
    version BIGINT DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT '',
    extra TEXT NOT NULL DEFAULT '',
    -- 禁言功能
    forbidden_expiration_time BIGINT DEFAULT 0,
    -- 头像缓存键
    member_avatar_cache_key TEXT NOT NULL DEFAULT ''
);

-- 频道成员表索引
CREATE UNIQUE INDEX IF NOT EXISTS channel_members_index ON channel_members (channel_id, channel_type, member_uid);
CREATE INDEX IF NOT EXISTS idx_channel_member_version ON channel_members (channel_id, channel_type, version);
CREATE INDEX IF NOT EXISTS channel_members_channel_index ON channel_members (channel_id, channel_type);
CREATE INDEX IF NOT EXISTS idx_channel_members_role_index ON channel_members (channel_id, channel_type, role);

-- 消息反应表 - 消息表情回复
CREATE TABLE message_reaction (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL DEFAULT '',
    channel_type INT DEFAULT 0,
    uid TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL DEFAULT '',
    emoji TEXT NOT NULL DEFAULT '',
    message_id TEXT NOT NULL DEFAULT '',
    seq BIGINT DEFAULT 0,
    is_deleted INT DEFAULT 0,
    created_at TEXT
);

-- 消息反应表索引
CREATE UNIQUE INDEX IF NOT EXISTS chat_msg_reaction_index ON message_reaction (message_id, uid, emoji);

-- 机器人表
CREATE TABLE robot (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    robot_id VARCHAR(40) NOT NULL DEFAULT '',
    status SMALLINT NOT NULL DEFAULT 1,
    version BIGINT NOT NULL DEFAULT 0,
    inline_on SMALLINT NOT NULL DEFAULT 0,
    placeholder VARCHAR(255) NOT NULL DEFAULT '',
    username VARCHAR(40) NOT NULL DEFAULT '',
    created_at TEXT,
    updated_at TEXT
);

-- 机器人表索引
CREATE UNIQUE INDEX robot_id_robot_index ON robot (robot_id);

-- 机器人菜单表
CREATE TABLE robot_menu (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    robot_id VARCHAR(40) NOT NULL DEFAULT '',
    cmd VARCHAR(100) NOT NULL DEFAULT '',
    remark VARCHAR(100) NOT NULL DEFAULT '',
    type VARCHAR(100) NOT NULL DEFAULT '',
    created_at TEXT,
    updated_at TEXT
);

-- 机器人菜单表索引
CREATE INDEX bot_id_robot_menu_index ON robot_menu (robot_id);

-- 消息扩展信息表 - 已读状态、撤回、编辑等
CREATE TABLE message_extra (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT,
    channel_id TEXT,
    channel_type SMALLINT NOT NULL DEFAULT 0,
    readed INTEGER NOT NULL DEFAULT 0,
    readed_count INTEGER NOT NULL DEFAULT 0,
    unread_count INTEGER NOT NULL DEFAULT 0,
    revoke SMALLINT NOT NULL DEFAULT 0,
    revoker TEXT,
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
CREATE TABLE reminders (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    reminder_id INTEGER NOT NULL DEFAULT 0,
    message_id TEXT NOT NULL DEFAULT '',
    message_seq UNSIGNED BIG INT NOT NULL DEFAULT 0,
    channel_id VARCHAR(100) NOT NULL DEFAULT '',
    channel_type SMALLINT NOT NULL DEFAULT 0,
    uid VARCHAR(100) NOT NULL DEFAULT '',
    type INTEGER NOT NULL DEFAULT 0,
    text VARCHAR(255) NOT NULL DEFAULT '',
    data VARCHAR(1000) NOT NULL DEFAULT '',
    is_locate SMALLINT NOT NULL DEFAULT 0,
    version BIGINT NOT NULL DEFAULT 0,
    done SMALLINT NOT NULL DEFAULT 0,
    need_upload SMALLINT NOT NULL DEFAULT 0,
    -- 提醒发布者
    publisher TEXT DEFAULT ''
);

-- 提醒表索引
CREATE INDEX IF NOT EXISTS idx_channel ON reminders (channel_id, channel_type);
CREATE UNIQUE INDEX IF NOT EXISTS uidx_reminder ON reminders (reminder_id);
CREATE INDEX IF NOT EXISTS version_reminders ON reminders (version);

-- 会话扩展信息表 - 草稿、阅读位置等
CREATE TABLE conversation_extra (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id VARCHAR(100) NOT NULL DEFAULT '',
    channel_type SMALLINT NOT NULL DEFAULT 0,
    browse_to UNSIGNED BIG INT NOT NULL DEFAULT 0,
    keep_message_seq UNSIGNED BIG INT NOT NULL DEFAULT 0,
    keep_offset_y INTEGER NOT NULL DEFAULT 0,
    draft VARCHAR(1000) NOT NULL DEFAULT '',
    version BIGINT NOT NULL DEFAULT 0,
    draft_updated_at UNSIGNED BIG INT NOT NULL DEFAULT 0
);

-- 会话扩展表索引
CREATE UNIQUE INDEX IF NOT EXISTS idx_channel_conversation_extra ON conversation_extra (channel_id, channel_type);

-- =============================================
-- 数据库版本信息表
-- =============================================
CREATE TABLE schema_version (
    version TEXT PRIMARY KEY,
    applied_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- 插入当前版本信息
INSERT INTO schema_version (version) VALUES ('20240101000001'); 