-- 清除由 read cursor 投影凭空造出来的会话行。
--
-- 写入侧已修（local_store.rs `project_channel_read_cursor` 改成 update-only）：会话行只能
-- 由 channel 实体同步创建，read cursor 是读状态 projection（CLIENT_UI_SPEC §3.1 /
-- READ_STATUS_SPEC §6）。但已经落在设备上的裸行不会自己消失——修了写入侧，老用户列表里
-- 那些无名、无头像、点不开的会话还在，观感上等于没修。
--
-- 判据要能精确圈出「只被 cursor 投影碰过」的行，宁可漏删不可错删：
--   version = 0        —— 实体同步一律写服务端 sync_version（>0），保留既有 version 的
--                         预览更新路径也不会把它降到 0；cursor 投影根本不写这一列。
--   三个展示字段全空   —— 实体同步会写 channel_name / channel_remark / avatar（DM 可能
--                         只有其中一个为空，三个同时为空且 version=0 只有裸行满足）。
--
-- 消息本身不动：它们仍在 message 表里。频道实体到达时 upsert_channel 会重建会话行，
-- 消息随即显形；实体永远不来（用户已不属于该频道）就该看不见——这正是「会话列表以
-- channel 表为准」的语义。
DELETE FROM channel
WHERE version = 0
  AND channel_name = ''
  AND channel_remark = ''
  AND avatar = '';
