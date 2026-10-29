CREATE TABLE IF NOT EXISTS marz_limits (
    id SERIAL PRIMARY KEY,
    admin_id INTEGER NOT NULL REFERENCES marz_admins(id) ON DELETE CASCADE,
    usage_limit BIGINT,
    user_limit INTEGER,
    start_date DATE NOT NULL,
    UNIQUE(admin_id, start_date)
);

CREATE INDEX IF NOT EXISTS idx_marz_limits_admin_date
    ON marz_limits(admin_id, start_date DESC);

ALTER TABLE marz_admins DROP COLUMN IF EXISTS usage_limit;
ALTER TABLE marz_admins DROP COLUMN IF EXISTS user_limit;
