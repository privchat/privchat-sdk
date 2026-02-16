-- Standardize message schema naming:
-- - id: local primary key (message.id)
-- - local_message_id: client-side dedupe id
-- - server_message_id: server global message id

ALTER TABLE message ADD COLUMN server_message_id INTEGER;

UPDATE message
SET server_message_id = message_id
WHERE (server_message_id IS NULL OR server_message_id = 0)
  AND message_id IS NOT NULL
  AND message_id > 0;

DROP INDEX IF EXISTS uniq_message_channel_server_id;
DROP INDEX IF EXISTS uniq_message_server_id;

DELETE FROM message
WHERE id IN (
    SELECT newer.id
    FROM message AS newer
    JOIN message AS older
      ON newer.server_message_id = older.server_message_id
     AND newer.server_message_id IS NOT NULL
     AND newer.server_message_id > 0
     AND newer.id > older.id
);

CREATE UNIQUE INDEX IF NOT EXISTS uniq_message_server_id
ON message (server_message_id)
WHERE server_message_id IS NOT NULL AND server_message_id > 0;
