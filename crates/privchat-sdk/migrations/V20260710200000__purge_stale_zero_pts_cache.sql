-- SDK-HISTORY（MESSAGE_HISTORY spec §2.5）：显示排序权威切换为 (pts, server_message_id)。
-- 历史上三条路径落过 pts=0 的"已确认"行（hydrate 落 0 / ack 丢弃 message_seq），
-- 这些行在新排序下无法正确定位。本地库是服务端历史的缓存（spec §1/§2），
-- 脏缓存的正确处理 = 清除后按需从服务端回填（sync/hydrate/around 都会带真实 pts 重建），
-- 不是本地猜序。仅清除"已有 server_message_id 且状态已确认"的行；
-- 未上服务器的 pending 行（server_message_id 为空）一律保留。
DELETE FROM message
WHERE (pts IS NULL OR pts = 0)
  AND COALESCE(server_message_id, 0) > 0
  AND status >= 2;
