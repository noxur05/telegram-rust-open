-- Create pending_payments table
-- Tracks active payment sessions (auto-expires after 24 hours)

CREATE TABLE IF NOT EXISTS pending_payments (
    id SERIAL PRIMARY KEY,
    payment_id VARCHAR NOT NULL UNIQUE,
    telegram_user_id BIGINT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now()),
    expires_at TIMESTAMP NOT NULL
);

-- Index for fast lookups by user
CREATE INDEX IF NOT EXISTS idx_pending_payments_telegram_user_id
    ON pending_payments (telegram_user_id);

-- Index for cleanup queries on expired payments
CREATE INDEX IF NOT EXISTS idx_pending_payments_expires_at
    ON pending_payments (expires_at);
