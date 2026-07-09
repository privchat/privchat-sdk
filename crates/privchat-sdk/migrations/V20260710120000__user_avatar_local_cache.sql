-- AVATAR_CACHE_SPEC P1: 头像本地缓存（URL 即缓存键，内容寻址下 URL 不变 ⇒ 内容不变）
-- avatar_local_path: 本地缓存文件绝对路径，空 = 未缓存
-- avatar_cached_url: 该缓存文件对应的源 URL，与最新 avatar 不等 ⇒ 缓存过期
ALTER TABLE "user" ADD COLUMN avatar_local_path TEXT NOT NULL DEFAULT '';
ALTER TABLE "user" ADD COLUMN avatar_cached_url TEXT NOT NULL DEFAULT '';
