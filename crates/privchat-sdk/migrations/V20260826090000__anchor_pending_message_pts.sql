-- 让存量的未确认消息回到它当时的位置（SDK_ENTITY_MODEL_SPEC §2.6.2）。
--
-- 显示排序以 pts 为主键。未确认的消息以前不写 pts（留 0），旧版靠「把未确认整组压到
-- 列表末尾」来躲开「pts=0 会窜到会话顶端」，代价是位置由确认与否决定：早上发失败的
-- 消息排到了晚上收到的消息后面。
--
-- 建行时锚定 pts 的改动只对新行生效，这里把已经躺在库里的行补齐：锚点取「同会话中
-- 比它早、且已确认的最后一条」的 pts。取不到（它前面没有已确认消息）就保持 0，本来
-- 就该排在最前。
UPDATE message
   SET pts = COALESCE(
       (SELECT MAX(m2.pts)
          FROM message m2
         WHERE m2.channel_id = message.channel_id
           AND m2.channel_type = message.channel_type
           AND COALESCE(m2.server_message_id, 0) > 0
           AND m2.created_at <= message.created_at),
       0)
 WHERE COALESCE(server_message_id, 0) <= 0
   AND COALESCE(pts, 0) = 0;
