-- 添加 delivered 字段：message-level delivery receipt
ALTER TABLE message_extra ADD COLUMN delivered INTEGER NOT NULL DEFAULT 0;
ALTER TABLE message_extra ADD COLUMN delivered_at INTEGER NOT NULL DEFAULT 0;
