CREATE TABLE IF NOT EXISTS marz_admins (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    telegram_id BIGINT UNIQUE,
    reset_day INTEGER NOT NULL CHECK (reset_day >= 1 AND reset_day <= 31),
    usage_limit BIGINT,
    user_limit INTEGER
);

CREATE TABLE IF NOT EXISTS marz_snapshots (
    id SERIAL PRIMARY KEY,
    admin_id INTEGER NOT NULL REFERENCES marz_admins(id) ON DELETE CASCADE,
    total_usage BIGINT NOT NULL,
    user_count INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now())
);

CREATE INDEX IF NOT EXISTS idx_marz_snapshots_admin_created
    ON marz_snapshots(admin_id, created_at DESC);
