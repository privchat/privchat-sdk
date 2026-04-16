-- 添加 peer_read_pts 列：对端已读游标，用于冷启动时恢复"已读"状态显示
ALTER TABLE channel_extra ADD COLUMN peer_read_pts UNSIGNED BIG INT NOT NULL DEFAULT 0;
