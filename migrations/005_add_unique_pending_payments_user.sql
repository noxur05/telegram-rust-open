-- Add unique constraint on telegram_user_id for pending_payments upsert
-- Drop existing index first (it's a plain index, not unique)
DROP INDEX IF EXISTS idx_pending_payments_telegram_user_id;
CREATE UNIQUE INDEX idx_pending_payments_telegram_user_id ON pending_payments (telegram_user_id);
