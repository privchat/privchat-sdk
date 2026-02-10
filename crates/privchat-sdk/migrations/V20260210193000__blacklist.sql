-- Local-first blacklist cache (per-user DB)
CREATE TABLE IF NOT EXISTS blacklist (
    blocked_user_id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_blacklist_updated_at ON blacklist(updated_at);
