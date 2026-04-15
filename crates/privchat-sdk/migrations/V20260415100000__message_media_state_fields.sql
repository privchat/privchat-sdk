-- 消息表新增媒体状态字段，驱动 UI 本地媒体展示
-- 详见 MESSAGE_SPEC §1.5 / DATABASE_SPEC §4.1

ALTER TABLE message ADD COLUMN mime_type TEXT DEFAULT NULL;
ALTER TABLE message ADD COLUMN media_downloaded SMALLINT NOT NULL DEFAULT 0;
ALTER TABLE message ADD COLUMN thumb_status SMALLINT NOT NULL DEFAULT 0;
-- thumb_status: 0=missing, 1=ready, 2=failed
