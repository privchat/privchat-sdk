-- 添加 peer_user_id 列：DM 会话对端用户 ID，由 channel 同步直接下发并持久化。
-- 客户端据此用 presenceMap[peer_user_id] 直查在线状态，取代 channel_member JOIN /
-- message.from_uid 推断（见 project_presence_architecture_direct）。群聊为空。
ALTER TABLE channel ADD COLUMN peer_user_id INTEGER;
