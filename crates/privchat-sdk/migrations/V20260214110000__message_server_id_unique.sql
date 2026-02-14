-- Ensure one logical row per (channel_id, server_message_id).
-- Keep the earliest local row (smallest id) when historical duplicates exist.
DELETE FROM message
WHERE id IN (
    SELECT newer.id
    FROM message AS newer
    JOIN message AS older
      ON newer.channel_id = older.channel_id
     AND newer.message_id = older.message_id
     AND newer.message_id IS NOT NULL
     AND newer.message_id > 0
     AND newer.id > older.id
);

CREATE UNIQUE INDEX IF NOT EXISTS uniq_message_channel_server_id
ON message (channel_id, message_id)
WHERE message_id IS NOT NULL AND message_id > 0;
