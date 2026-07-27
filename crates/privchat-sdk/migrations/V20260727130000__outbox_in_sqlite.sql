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
    -- Optional: commands for the same entity must go out in order.
    ordering_key      TEXT,
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
