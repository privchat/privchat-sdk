-- Client Command Outbox (SYNC_SPEC §3.3): every durable network mutation,
-- in the same database as `message`.
--
-- Previously the outbound queue was a separate sled store. Two engines cannot
-- share a transaction, so "server accepted the message" and "local row says
-- sent" could never be committed together: whatever order they ran in, a
-- crash in between left the message either stuck sending forever or queued
-- for a second send. Every compensation for that window — a side table of
-- pending acks, a replay pass, per-queue bookkeeping — was another source of
-- truth to keep consistent.
--
-- One database removes the window instead of compensating for it:
--
--   send   : UPDATE message SET status=sending + INSERT outbox   (one tx)
--   ack    : UPDATE message SET status=sent,... + DELETE outbox   (one tx)
--   failure: the transaction rolls back, so the outbox row is simply still
--            there and the retry reuses the same local_message_id, which the
--            server dedupes.
--
-- There is no lease and no owner token here: the Rust SDK drives this from a
-- single storage actor. (The TypeScript SDK does need leases — several
-- browser tabs share one account database — but that is a property of the
-- browser, not of this design.)
-- This is the queue for EVERY durable network mutation, not just messages:
-- moment likes, comments, reactions, edits, revokes, friend changes, channel
-- settings. Sending a message is one `command_type` among them. A
-- message-shaped table would have to be rebuilt the moment the first
-- non-message command needs offline retry.
CREATE TABLE IF NOT EXISTS outbox (
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

CREATE INDEX IF NOT EXISTS idx_outbox_due
    ON outbox (status, next_attempt_at, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_message
    ON outbox (message_id) WHERE message_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_outbox_coalesce
    ON outbox (coalesce_key) WHERE coalesce_key IS NOT NULL;

-- Retire the compensation table this design makes unnecessary. A second place
-- that can hold "this message was really sent" is exactly what the single
-- database model exists to remove.
--
-- Anything still in it describes a message the server accepted, so apply it
-- here rather than dropping it on the floor. Migrations run before the SDK
-- does anything, so this is the only chance to do so.
-- 同一个 server_message_id 可能已经由推送/同步落成了另一条 message 行。直接
-- UPDATE 会撞 server_message_id 全局唯一索引，整条迁移失败——用户升级即打不开
-- 数据库。所以先处理重复行。
--
-- 保留的 canonical 行是待确认的那条（`outbound_ack_pending.message_id`）：它是
-- 用户在本机看到的那条，reply/草稿等本地引用都指着它。重复行是同步补进来的
-- 副本。
--
-- 但**不能直接删副本**：reaction / extra / mention / reminder 都按 message.id
-- 关联在副本上，删了就是丢数据。先把这些引用改指到保留行，再删。
CREATE TEMP TABLE IF NOT EXISTS _ack_dup AS
SELECT p.message_id AS keep_id, m.id AS drop_id
FROM outbound_ack_pending p
JOIN message m ON m.server_message_id = p.server_message_id
WHERE m.id != p.message_id;

-- 改指之前先合并：这几张表都有唯一索引
--   message_extra(message_id)
--   message_reaction(message_id, uid, emoji)
--   mention(message_id, mentioned_user_id)
-- 保留行与重复行如果各有一条对应记录，直接 UPDATE 会撞唯一键，整条迁移失败
-- ——用户升级即打不开数据库。所以：先删掉「保留行已经有了」的那些副本记录，
-- 剩下的才改指过去。

-- message_extra：一条消息一行，按 extra_version 取新的那份。
DELETE FROM message_extra
WHERE message_id IN (SELECT drop_id FROM _ack_dup)
  AND EXISTS (
      SELECT 1 FROM message_extra keep
      JOIN _ack_dup d ON d.keep_id = keep.message_id
      WHERE d.drop_id = message_extra.message_id
        AND keep.extra_version >= message_extra.extra_version
  );
-- 副本更新则反过来删掉保留行那份，让副本改指过去。
DELETE FROM message_extra
WHERE message_id IN (SELECT keep_id FROM _ack_dup)
  AND EXISTS (
      SELECT 1 FROM message_extra drop_row
      JOIN _ack_dup d ON d.drop_id = drop_row.message_id
      WHERE d.keep_id = message_extra.message_id
        AND drop_row.extra_version > message_extra.extra_version
  );
UPDATE message_extra
SET message_id = (SELECT keep_id FROM _ack_dup WHERE drop_id = message_extra.message_id)
WHERE message_id IN (SELECT drop_id FROM _ack_dup);

-- reaction / mention 是集合语义：同一个 (目标, 用户, 值) 重复就是同一件事，
-- 保留行已有就丢弃副本那条。
DELETE FROM message_reaction
WHERE message_id IN (SELECT drop_id FROM _ack_dup)
  AND EXISTS (
      SELECT 1 FROM message_reaction keep
      JOIN _ack_dup d ON d.keep_id = keep.message_id
      WHERE d.drop_id = message_reaction.message_id
        AND keep.uid = message_reaction.uid
        AND keep.emoji = message_reaction.emoji
  );
UPDATE message_reaction
SET message_id = (SELECT keep_id FROM _ack_dup WHERE drop_id = message_reaction.message_id)
WHERE message_id IN (SELECT drop_id FROM _ack_dup);

DELETE FROM mention
WHERE message_id IN (SELECT drop_id FROM _ack_dup)
  AND EXISTS (
      SELECT 1 FROM mention keep
      JOIN _ack_dup d ON d.keep_id = keep.message_id
      WHERE d.drop_id = mention.message_id
        AND keep.mentioned_user_id = mention.mentioned_user_id
  );
UPDATE mention
SET message_id = (SELECT keep_id FROM _ack_dup WHERE drop_id = mention.message_id)
WHERE message_id IN (SELECT drop_id FROM _ack_dup);

-- reminder 没有 (message_id, *) 唯一索引，直接改指。
UPDATE reminder
SET message_id = (SELECT keep_id FROM _ack_dup WHERE drop_id = reminder.message_id)
WHERE message_id IN (SELECT drop_id FROM _ack_dup);

DELETE FROM message WHERE id IN (SELECT drop_id FROM _ack_dup);

DROP TABLE IF EXISTS _ack_dup;

UPDATE message
SET server_message_id = (
        SELECT p.server_message_id FROM outbound_ack_pending p
        WHERE p.message_id = message.id
    ),
    pts = (
        SELECT p.message_seq FROM outbound_ack_pending p
        WHERE p.message_id = message.id
    ),
    order_seq = (
        SELECT p.message_seq FROM outbound_ack_pending p
        WHERE p.message_id = message.id
    ),
    status = 2
WHERE id IN (SELECT message_id FROM outbound_ack_pending);

DROP TABLE IF EXISTS outbound_ack_pending;
