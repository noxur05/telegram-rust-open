-- Historical payments to re-check with the admin Reconcile action.
--
-- Deliberately NOT pending_payments: rows here are never touched by the payment
-- poller, so history can only ever be credited after an admin has seen the
-- dry-run list. Retro-crediting moves real money, and the ledger cannot tell a
-- payment that was never credited from one that was compensated by hand (admin
-- adjustments carry payment_id = NULL).
--
-- Seeded separately at deploy time from the pre-existing DB dump rather than
-- from this file, to keep real telegram ids out of version control:
--
--   awk '/^COPY public\.pending_payments/,/^\\\.$/' /root/telegram_bot.sql \
--     | sed -n '2,$p' | grep -v '^\\.$' \
--     | awk -F'\t' '{printf "INSERT INTO reconcile_candidates (payment_id, telegram_user_id, created_at) VALUES (%s, %s, %s) ON CONFLICT (payment_id) DO NOTHING;\n", "'"'"'"$2"'"'"'", $3, "'"'"'"$4"'"'"'"}'
--
-- Re-running the seed or the reconcile itself is safe: payment_id is UNIQUE here,
-- and crediting is keyed on payment_id in the transactions ledger.
CREATE TABLE IF NOT EXISTS reconcile_candidates (
    id SERIAL PRIMARY KEY,
    payment_id VARCHAR NOT NULL UNIQUE,
    telegram_user_id BIGINT NOT NULL,
    created_at TIMESTAMP NOT NULL,
    -- Stamped once the row has been examined, whatever the outcome, so a second
    -- Reconcile run doesn't re-list payments already dealt with.
    resolved_at TIMESTAMP NULL,
    -- Terminal status observed at resolve time, or the reason it was skipped.
    resolution VARCHAR NULL
);

CREATE INDEX IF NOT EXISTS idx_reconcile_candidates_unresolved
    ON reconcile_candidates (resolved_at)
    WHERE resolved_at IS NULL;
