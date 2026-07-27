-- created_at 修复：把「消息发送时间」从「本机入库时间」手里拿回来。
--
-- 旧写法 INSERT/UPDATE 时 created_at = now()，而消息真实时间只写进了从未被读的
-- timestamp 列。客户端读取和显示的恰恰是 created_at，于是：上滑翻页在今天补进来的
-- 几周前的历史，全部带着今天的时间戳，并且排到了真正更新的消息后面
-- （生产实测：channel 45 的 pts 1..14 全被写成同一分钟）。
--
-- 两列的关系是可判定的，不需要猜：
--   * timestamp 是发送时间，但历史上混过两种单位——push 路径按秒写
--     （PushMessageRequest.timestamp 是 u32 秒），history/sync 路径按毫秒写。
--     10^11 毫秒 = 1973 年：真毫秒一定在其上，真秒一定在其下。
--   * created_at 只有在 timestamp 完全不可用（<=0）时才保留原值——那时本机入库
--     时间虽然不准，但比 1970 好。
--
-- 只改已确认的服务端消息（server_message_id > 0）。本机 pending 行的 created_at
-- 本来就该是本机时间，不能动。
UPDATE message
SET created_at = CASE
        WHEN timestamp >= 100000000000 THEN timestamp
        ELSE timestamp * 1000
    END
WHERE COALESCE(server_message_id, 0) > 0
  AND COALESCE(timestamp, 0) > 0
  -- 只改真的错了的行:差一秒以内说明 created_at 本来就是同一时刻的更精确版本
  -- （push 行的入库时刻≈发送时刻,且带毫秒;timestamp 列那边只有秒）。
  -- 无条件覆盖会把这些行的精度降到整秒,白白弄丢信息。
  AND ABS(created_at - (CASE
        WHEN timestamp >= 100000000000 THEN timestamp
        ELSE timestamp * 1000
    END)) >= 1000;
