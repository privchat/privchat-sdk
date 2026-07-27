-- Messages the server has accepted but whose local commit did not land.
--
-- The outbound queue (sled) and the message table (SQLite) cannot share a
-- transaction, so there is a window where `direct_send_message` succeeds and
-- `mark_message_sent` then fails. Dropping the queue item there — which is
-- what the code used to do, to avoid a duplicate send — threw away the only
-- record that the message HAD been delivered: after a restart nothing knew
-- the server id, so the row stayed "sending" forever.
--
-- This table is that record. It lives in the same database as `message`, so
-- replaying the acknowledgement is one local SQLite transaction: mark sent
-- and delete the row together. Recovery never goes back on the wire — the
-- message is already delivered, and re-sending it would rely on server-side
-- idempotency records that do not live forever.
CREATE TABLE IF NOT EXISTS outbound_ack_pending (
    -- Local message.id; matches the outbound queue key it replaces.
    message_id        INTEGER PRIMARY KEY,
    server_message_id INTEGER NOT NULL,
    message_seq       INTEGER NOT NULL,
    -- Local replay attempts. Never caps out into a "give up and re-send"
    -- path: the message exists remotely, so the only correct outcome is to
    -- converge locally or report broken local data.
    attempts          INTEGER NOT NULL DEFAULT 0,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);
