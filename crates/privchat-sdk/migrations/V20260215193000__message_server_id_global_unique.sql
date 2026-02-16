-- Enforce global uniqueness for server_message_id (stored in message.message_id).
-- Keep the earliest local row when historical duplicates exist.
DELETE FROM message
WHERE id IN (
    SELECT newer.id
    FROM message AS newer
    JOIN message AS older
      ON newer.message_id = older.message_id
     AND newer.message_id IS NOT NULL
     AND newer.message_id > 0
     AND newer.id > older.id
);

DROP INDEX IF EXISTS uniq_message_channel_server_id;

CREATE UNIQUE INDEX IF NOT EXISTS uniq_message_server_id
ON message (message_id)
WHERE message_id IS NOT NULL AND message_id > 0;
