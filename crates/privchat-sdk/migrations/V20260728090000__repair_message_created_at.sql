-- 一次性修复三件互相牵连的事：created_at 的语义、受它影响的媒体落盘路径、
-- 以及 history 回填丢掉 metadata 造成的「永远加载不出来的图片」。
-- 顺序有依赖，不能重排。

-- ── 1. 媒体重置：必须发生在改写 created_at **之前** ──────────────────────
-- 媒体按 files/{yyyymm}/{message_id}/ 落盘，yyyymm 取自 created_at。第 2 步会
-- 把 created_at 从「入库时间」改成「发送时间」，跨月的行改完之后就再也找不到
-- 自己已经下载好的文件了。把这些行的下载状态清零，让 SDK 按新路径重新取一份
-- （缩略图很小，重下一次远比留一堆指向空处的行好）。
-- 只有在这一步还能同时看到新旧两个值，一旦 created_at 被改写就无从判断了。
UPDATE message
SET thumb_status = 0, media_downloaded = 0
WHERE type IN (1, 2, 3, 4)
  AND COALESCE(server_message_id, 0) > 0
  AND COALESCE(timestamp, 0) > 0
  AND strftime('%Y%m', created_at / 1000, 'unixepoch') <> strftime(
        '%Y%m',
        (CASE WHEN timestamp >= 100000000000 THEN timestamp ELSE timestamp * 1000 END) / 1000,
        'unixepoch'
      );

-- ── 2. created_at：把「发送时间」从「本机入库时间」手里拿回来 ─────────────
-- 旧写法 INSERT/UPDATE 时 created_at = now()，消息真实时间只进了从未被读的
-- timestamp 列。客户端读取、显示、（当时）排序用的恰恰是 created_at，于是上滑
-- 翻页在今天补进来的历史全部带着今天的时间戳，还排到了真正更新的消息后面
-- （生产实测：channel 45 的 pts 1..14 全被写成同一分钟）。
--
-- 两列的关系是可判定的，不用猜：timestamp 是发送时间，但历史上混过两种单位
-- —— push 路径按秒写（PushMessageRequest.timestamp 是 u32 秒），history/sync
-- 路径按毫秒写。10^11 毫秒 = 1973 年：真毫秒一定在其上，真秒一定在其下。
--
-- 只改已确认的服务端消息。本机 pending 行的 created_at 本来就该是本机时间。
UPDATE message
SET created_at = CASE
        WHEN timestamp >= 100000000000 THEN timestamp
        ELSE timestamp * 1000
    END
WHERE COALESCE(server_message_id, 0) > 0
  AND COALESCE(timestamp, 0) > 0
  -- 只改真的错了的行：差一秒以内说明 created_at 本来就是同一时刻的更精确版本
  -- （push 行的入库时刻≈发送时刻，且带毫秒；timestamp 列那边只有秒）。
  -- 无条件覆盖会把这些行的精度降到整秒，白白弄丢信息。
  AND ABS(created_at - (CASE
        WHEN timestamp >= 100000000000 THEN timestamp
        ELSE timestamp * 1000
    END)) >= 1000;

-- ── 3. 清掉 history 回填时丢了 metadata 的媒体行 ─────────────────────────
-- 那条路径把 extra 写成空串，而 file_id / thumbnail_file_id / thumbnail_url 全
-- 住在 extra 里。结果：缩略图触发点找不到任何可下载的东西，判定「这条消息本来
-- 就没有缩略图」并写下 thumb_status=3——一个终态，此后永不重试。生产实测这些
-- 行确实是 extra_len=0 + thumb_status=3，屏幕上就是一直空着的灰块。
--
-- 光把 thumb_status 改回 0 没用：extra 还是空的，下一次触发会再判一次 3。本地
-- 库是服务端历史的缓存，脏缓存的正确处理是清除后按需回填（与
-- V20260710200000 同一套路），回填代码现在会带上 metadata。
DELETE FROM message
WHERE type IN (1, 2, 3, 4)
  AND COALESCE(server_message_id, 0) > 0
  AND status >= 2
  AND COALESCE(extra, '') = '';
