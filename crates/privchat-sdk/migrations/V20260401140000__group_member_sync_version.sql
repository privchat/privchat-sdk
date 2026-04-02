ALTER TABLE group_member ADD COLUMN version INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_group_member_version
    ON group_member(group_id, version DESC, user_id DESC);
