-- Per-plan device limit, required by the Happ client policy.
--
-- The Marzban fork (ghcr.io/noxur05/marzban:custom-latest) validates
-- `client_type` and `device_limit` as a pair: UserCreate.validate_device_policy
-- rejects a create where one is set and the other is null. Since the bot now
-- creates every account with client_type="happ", every plan needs a device count.
--
-- DEFAULT 2 so existing rows become valid without an admin editing each one.
--
-- NOTE: this is also applied by db::ensure_subscription_tables() at startup,
-- because CI ships only the binary and never runs migrations. This file exists
-- so the schema history stays complete and a fresh DB built from migrations/
-- matches a DB built by the running binary.
ALTER TABLE subscriptions
    ADD COLUMN IF NOT EXISTS device_limit INTEGER NOT NULL DEFAULT 2;
