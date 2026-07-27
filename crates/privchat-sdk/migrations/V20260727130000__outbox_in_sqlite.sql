-- Outbox lives in the same database as `message` (MESSAGE_SPEC §8.3).
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
CREATE TABLE IF NOT EXISTS outbox (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Local `message.id`. Unique: one outbound command per message.
    local_message_id  INTEGER UNIQUE NOT NULL,
    -- 'message' | 'attachment'. Attachments differ only in needing an upload
    -- before the send, not in their durability rules.
    command_type      TEXT NOT NULL,
    channel_id        INTEGER NOT NULL,
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
