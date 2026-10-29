-- Add unique constraint on transactions.payment_id to prevent double-credit races.
-- The CTE in db::top_up_balance does an existence check then INSERT, but two concurrent
-- /check requests can both pass the existence check on separate snapshots and each
-- insert a transaction for the same payment_id, double-crediting the user.
-- Partial unique index because internal debits/refunds intentionally store NULL.
CREATE UNIQUE INDEX IF NOT EXISTS transactions_payment_id_unique
    ON transactions (payment_id)
    WHERE payment_id IS NOT NULL;
