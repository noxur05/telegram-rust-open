-- Durable record of which Telegram user owns which Marzban account.
--
-- Ownership is read from the account's `note` at runtime (see handlers/subscriptions.rs),
-- but the note is free text that any of the panel's 30+ reseller admins can edit. Before
-- this table, a cleared or retyped note was an UNRECOVERABLE lockout: the account dropped
-- out of the user's listing, extend rejected it, and nothing anywhere mapped it back to a
-- user — `transactions.description` is the literal string 'Subscription purchase' and
-- records no account name.
--
-- The `tg_{id}_{ts}` username used to be an accidental backup, because the id was IN the
-- name. Letting users choose their own names removes that, which is why this lands in the
-- same change.
--
-- This is a RECOVERY record, not the runtime authority: restoring a wiped note is a
-- lookup here away. Making it the primary check is a separate change.
CREATE TABLE IF NOT EXISTS marzban_accounts (
    marzban_username VARCHAR PRIMARY KEY,
    telegram_id      BIGINT NOT NULL,
    created_at       TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now())
);

CREATE INDEX IF NOT EXISTS idx_marzban_accounts_telegram_id
    ON marzban_accounts (telegram_id);
