-- Allow a user to have more than one open invoice at a time.
--
-- Migration 005 made telegram_user_id UNIQUE so that store_pending_payment could
-- upsert on it. The side effect was that creating a second invoice OVERWROTE the
-- first: the old payment_id was erased, so money sent to that invoice could never
-- be credited. The manual /check <id> command was the only escape hatch, and the
-- payment poller replaces it — so the constraint has to go, and every invoice a
-- user creates stays tracked and polled independently.
--
-- payment_id keeps its UNIQUE from migration 003; it is the natural key and what
-- the ledger dedupes on.
DROP INDEX IF EXISTS idx_pending_payments_telegram_user_id;

CREATE INDEX IF NOT EXISTS idx_pending_payments_telegram_user_id
    ON pending_payments (telegram_user_id);

-- Set the first time the provider reported funds for this invoice, so the
-- "payment detected" message fires exactly once — including across a restart,
-- which an in-memory set would not survive.
ALTER TABLE pending_payments ADD COLUMN IF NOT EXISTS detected_at TIMESTAMP NULL;
