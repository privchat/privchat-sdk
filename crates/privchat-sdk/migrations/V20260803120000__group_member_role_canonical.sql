-- 群成员角色改用协议权威编码：Member=0 / Owner=1 / Admin=2。
--
-- 本地表历史上用的是 owner=2 / admin=1 / member=0（与 UI 的 `isOwner = role == 2`
-- 配套），与协议冻结的编号不同：owner 与 admin **互换**。
--
-- 互换不能写成两条 UPDATE —— 第一条把 2 改成 1 之后，第二条会把它们连同原本的
-- admin 一起再改回 2。必须一次 CASE 完成。
--
-- 幂等性由 refinery 的迁移历史表保证：本文件只会执行一次。**不要**把它改成
-- 「重复执行也安全」的形式——owner/admin 互换没有这样的写法，任何"再跑一遍"
-- 都会把两个角色换回去。
UPDATE group_member
   SET role = CASE role
                WHEN 2 THEN 1   -- 旧 owner -> 新 owner
                WHEN 1 THEN 2   -- 旧 admin -> 新 admin
                ELSE 0          -- 旧 member 及任何脏值 -> member（往上猜是提权）
              END;
