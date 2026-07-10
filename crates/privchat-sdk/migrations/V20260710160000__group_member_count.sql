-- 群成员数缓存：服务端 privchat_groups.member_count 是权威计数，随 group 实体同步
-- 下发（GroupSyncPayload.member_count / server channel_service 已发）。此前本地 group
-- 表无此列，ChannelListEntry.member_count 恒 0，群标题「(N)」无数据源只能靠九宫格
-- 成员预览缓存兜底。补列后随 group upsert 落库，频道列表查询直接读取。
ALTER TABLE "group" ADD COLUMN member_count INTEGER NOT NULL DEFAULT 0;
