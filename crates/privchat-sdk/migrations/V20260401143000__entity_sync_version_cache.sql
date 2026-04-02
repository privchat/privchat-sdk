ALTER TABLE "user" ADD COLUMN version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE "group" ADD COLUMN version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE friend ADD COLUMN version INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_user_version ON "user"(version DESC, user_id DESC);
CREATE INDEX IF NOT EXISTS idx_group_version ON "group"(version DESC, group_id DESC);
CREATE INDEX IF NOT EXISTS idx_friend_version ON friend(version DESC, user_id DESC);
