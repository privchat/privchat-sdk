-- F-sync.2: friend 表纳入"好友申请"状态以参与多设备 entity sync。
--
-- 现状（V20241119070909）：friend 表只存 accepted 好友（status 隐含=1），
-- 没有 status / direction / 申请附言 / 来源等字段。
--
-- F-sync.1 已经把 server 端 friendships 多 status 状态通过 sync_entities("friend")
-- 分发出来；这次 client 端补字段，让所有非 accepted 状态都能正确落本地。
--
-- status 取值与 server 端 FriendshipStatus 对齐：
--   0=pending / 1=accepted / 2=blocked / 3=rejected / 4=recalled / 5=expired
--
-- is_outgoing 仅对 status != 1 (非 accepted) 有意义：
--   1 = viewer 是 requester（"我发出的"）
--   0 = viewer 是 receiver（"我收到的"）
-- accepted 行的 is_outgoing 由 server 不填，存 NULL。

ALTER TABLE friend ADD COLUMN status INTEGER NOT NULL DEFAULT 1;
ALTER TABLE friend ADD COLUMN is_outgoing INTEGER;
ALTER TABLE friend ADD COLUMN request_message TEXT;
ALTER TABLE friend ADD COLUMN request_source TEXT;
ALTER TABLE friend ADD COLUMN request_source_id TEXT;

CREATE INDEX IF NOT EXISTS idx_friend_status ON friend(status);
