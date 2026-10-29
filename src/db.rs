use crate::config::Config;
use crate::i18n::Lang;
use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    Value,
};

/// Establish a database connection.
pub async fn establish_connection(config: &Config) -> Result<DatabaseConnection, DbErr> {
    Database::connect(&config.database_url).await
}

/// Save a Telegram user into `users` table. If exists, do nothing.
pub async fn save_user_from_telegram(
    conn: &DatabaseConnection,
    telegram_id: i64,
    username: Option<&str>,
    is_bot: bool,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO users (username, telegram_id, is_bot, balance, created_at) \
         VALUES ($1, $2, $3, 0, TIMEZONE('Asia/Ashgabat', now())) \
         ON CONFLICT (telegram_id) DO NOTHING",
        [
            username
                .map(|u| Value::String(Some(Box::new(u.to_string()))))
                .unwrap_or(Value::String(None)),
            Value::BigInt(Some(telegram_id)),
            Value::Bool(Some(is_bot)),
        ],
    );

    conn.execute(stmt).await.map(|_| ())
}

/// Result struct for balance query
#[derive(Debug, FromQueryResult)]
pub struct UserBalance {
    pub balance: rust_decimal::Decimal,
}

/// Shared result struct for balance-modifying queries.
///
/// Carries only the post-write balance: both writers below return it from the
/// `transactions` INSERT (`RETURNING balance_after`), so the value in the ledger and
/// the value handed to the caller are by construction the same number.
#[derive(Debug, FromQueryResult)]
struct BalanceUpdateResult {
    balance: rust_decimal::Decimal,
}

/// Get user balance by telegram_id. Returns (user_id, balance) if found.
pub async fn get_user_balance(
    conn: &DatabaseConnection,
    telegram_id: i64,
) -> Result<Option<UserBalance>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT balance FROM users WHERE telegram_id = $1 LIMIT 1",
        [Value::BigInt(Some(telegram_id))],
    );

    UserBalance::find_by_statement(stmt).one(conn).await
}

/// Deduct balance from a user and write the ledger row in ONE statement.
///
/// The `UPDATE` and the `transactions` INSERT are a single data-modifying CTE, so they
/// commit or roll back together. They used to be two separately autocommitted
/// statements, and that was a money bug: if the ledger INSERT failed (a connection
/// reset, a pool timeout, a constraint), the balance had *already* been reduced, yet
/// this returned `Err` — and both callers read `Err` as "the deduction did not happen"
/// and return without refunding (`buy.rs`, `extend.rs`). The user was charged, received
/// nothing, and no `CRITICAL:` line was logged because nothing looked wrong here.
/// (A SIGTERM mid-deploy left the same inconsistent state by a different route: the
/// process simply died between the two statements, so no caller saw an `Err` at all.)
///
/// Shape copied from `admin::db::admin_adjust_balance`, which already did it correctly.
///
/// The contract callers depend on: **`Err` means no money moved** — for every
/// server-side failure, and for the `from_f64_retain` guard above, which sends no SQL
/// at all. The one case it cannot cover is a response lost *after* Postgres committed
/// (TCP reset, pool timeout at exactly the wrong moment): the write landed, the caller
/// sees `Err`, and no refund is issued. Closing that needs a client-supplied
/// idempotency key on the debit side; it is far narrower than the window this replaced.
///
/// The conditional `balance >= $1` still prevents a negative balance, and an unmatched
/// UPDATE yields no rows, hence the "Insufficient balance" error the callers match on.
pub async fn deduct_balance(
    conn: &DatabaseConnection,
    telegram_id: i64,
    amount: f64,
    description: &str,
) -> Result<rust_decimal::Decimal, DbErr> {
    let amount_decimal = rust_decimal::Decimal::from_f64_retain(amount)
        .ok_or_else(|| DbErr::Custom("Invalid amount".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH updated AS ( \
             UPDATE users SET balance = balance - $1 \
             WHERE telegram_id = $2 AND balance >= $1 \
             RETURNING id, balance \
         ) \
         INSERT INTO transactions (user_id, amount, balance_after, description, payment_id, created_at) \
         SELECT id, $3, balance, $4, NULL, TIMEZONE('Asia/Ashgabat', now()) \
         FROM updated \
         RETURNING balance_after AS balance",
        [
            Value::Decimal(Some(Box::new(amount_decimal))),
            Value::BigInt(Some(telegram_id)),
            Value::Decimal(Some(Box::new(-amount_decimal))),
            Value::String(Some(Box::new(description.to_string()))),
        ],
    );

    BalanceUpdateResult::find_by_statement(stmt)
        .one(conn)
        .await?
        .map(|r| r.balance)
        .ok_or_else(|| DbErr::Custom("Insufficient balance or user not found".to_string()))
}

/// Check if a transaction with the given payment_id already exists
pub async fn transaction_exists_by_payment_id(
    conn: &DatabaseConnection,
    payment_id: &str,
) -> Result<bool, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*) as count FROM transactions WHERE payment_id = $1",
        [Value::String(Some(Box::new(payment_id.to_string())))],
    );

    #[derive(Debug, FromQueryResult)]
    struct CountResult {
        count: i64,
    }

    let result = CountResult::find_by_statement(stmt).one(conn).await?;

    Ok(result.map(|r| r.count > 0).unwrap_or(false))
}

/// Get all non-bot user telegram IDs for broadcasting.
pub async fn get_all_user_telegram_ids(conn: &DatabaseConnection) -> Result<Vec<i64>, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct UserTelegramId {
        telegram_id: i64,
    }

    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT telegram_id FROM users WHERE is_bot = false".to_string(),
    );

    let results = UserTelegramId::find_by_statement(stmt).all(conn).await?;

    Ok(results.into_iter().map(|r| r.telegram_id).collect())
}

/// Refund balance to a user and write the ledger row in ONE statement.
///
/// Used when a purchase fails after the balance was already deducted. Atomic for the
/// same reason as `deduct_balance`, and here the old two-statement form was arguably
/// worse: a failed ledger INSERT after a successful refund UPDATE reported
/// `CRITICAL: Refund failed` at every call site while the refund had actually landed —
/// inviting an operator to pay it a second time by hand.
pub async fn refund_balance(
    conn: &DatabaseConnection,
    telegram_id: i64,
    amount: f64,
    description: &str,
) -> Result<rust_decimal::Decimal, DbErr> {
    let amount_decimal = rust_decimal::Decimal::from_f64_retain(amount)
        .ok_or_else(|| DbErr::Custom("Invalid amount".to_string()))?;

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH updated AS ( \
             UPDATE users SET balance = balance + $1 \
             WHERE telegram_id = $2 \
             RETURNING id, balance \
         ) \
         INSERT INTO transactions (user_id, amount, balance_after, description, payment_id, created_at) \
         SELECT id, $1, balance, $3, NULL, TIMEZONE('Asia/Ashgabat', now()) \
         FROM updated \
         RETURNING balance_after AS balance",
        [
            Value::Decimal(Some(Box::new(amount_decimal))),
            Value::BigInt(Some(telegram_id)),
            Value::String(Some(Box::new(description.to_string()))),
        ],
    );

    BalanceUpdateResult::find_by_statement(stmt)
        .one(conn)
        .await?
        .map(|r| r.balance)
        .ok_or_else(|| DbErr::Custom("User not found for refund".to_string()))
}

/// Top up user balance atomically. Returns the new balance if successful.
/// Uses INSERT ... ON CONFLICT to prevent double-processing of the same payment.
/// Returns Err if payment_id was already processed.
pub async fn top_up_balance(
    conn: &DatabaseConnection,
    telegram_id: i64,
    amount: f64,
    description: &str,
    payment_id: &str,
) -> Result<rust_decimal::Decimal, DbErr> {
    let amount_decimal = rust_decimal::Decimal::from_f64_retain(amount)
        .ok_or_else(|| DbErr::Custom("Invalid amount".to_string()))?;

    // First, try to insert the transaction with the payment_id
    // This will fail if payment_id already exists (assuming we create an index)
    // We use a CTE to do this atomically
    // `new_balance` is Option because the final SELECT has no top-level FROM and so
    // returns exactly one row even when the telegram_id has no `users` row — in which
    // case both COALESCE arms are NULL. Decoding that into a bare `Decimal` produced
    // `DbErr::Type("A null value was encountered...")`, which matches none of the
    // caller's error tests: the poller took its CRITICAL arm and retried the same
    // invoice every 30s forever. Typing it as Option turns that into a clear
    // "User not found".
    #[derive(Debug, FromQueryResult)]
    struct TopUpResult {
        new_balance: Option<rust_decimal::Decimal>,
        inserted: bool,
    }

    // Use a single atomic query:
    // 1. Update the balance, but only if this payment_id is not already in the ledger
    // 2. Insert the transaction FROM that update's RETURNING
    // 3. Return the new balance and whether insertion happened
    //
    // The UPDATE must come first and the INSERT must read its `RETURNING balance`.
    // The reverse order (INSERT computing `user_info.balance + $2`) took the amount
    // from the statement snapshot, while the UPDATE re-read the row under lock — so a
    // concurrent `deduct_balance` committing in between left `transactions.balance_after`
    // disagreeing with `users.balance`. Measured: start 10, concurrent -3, credit +10
    // recorded 20 in the ledger where the true balance was 17. The returned value was
    // always right; only the audit trail lied, which is the column an operator reads
    // when reconstructing a balance dispute.
    let atomic_stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH user_info AS (
            SELECT id, balance FROM users WHERE telegram_id = $1
        ),
        check_existing AS (
            SELECT COUNT(*) > 0 AS exists FROM transactions WHERE payment_id = $4
        ),
        update_balance AS (
            UPDATE users
            SET balance = balance + $2
            WHERE id = (SELECT id FROM user_info)
              AND NOT (SELECT exists FROM check_existing)
            RETURNING id, balance
        ),
        insert_tx AS (
            INSERT INTO transactions (user_id, amount, balance_after, description, payment_id, created_at)
            SELECT
                id,
                $2,
                balance,
                $3,
                $4,
                TIMEZONE('Asia/Ashgabat', now())
            FROM update_balance
            RETURNING user_id
        )
        SELECT
            COALESCE(
                (SELECT balance FROM update_balance),
                (SELECT balance FROM user_info)
            ) as new_balance,
            EXISTS(SELECT 1 FROM insert_tx) as inserted",
        [
            Value::BigInt(Some(telegram_id)),
            Value::Decimal(Some(Box::new(amount_decimal))),
            Value::String(Some(Box::new(description.to_string()))),
            Value::String(Some(Box::new(payment_id.to_string()))),
        ],
    );

    let result = TopUpResult::find_by_statement(atomic_stmt)
        .one(conn)
        .await?
        .ok_or_else(|| DbErr::Custom("User not found".to_string()))?;

    match (result.inserted, result.new_balance) {
        (true, Some(balance)) => Ok(balance),
        // Nothing was inserted but the user exists: this payment_id is already in the
        // ledger. `is_already_credited` matches this string — the money is in.
        (false, Some(_)) => Err(DbErr::Custom("Payment already processed".to_string())),
        // No `users` row at all. Deliberately NOT an already-credited error: nothing
        // was written and nothing should be treated as settled.
        (_, None) => Err(DbErr::Custom("User not found".to_string())),
    }
}

/// Store a pending payment so the poller can watch it.
///
/// A plain insert, not an upsert: a user may hold several open invoices at once and
/// each is tracked independently. The old `ON CONFLICT (telegram_user_id) DO UPDATE`
/// overwrote the user's previous invoice, erasing its payment_id — money sent to that
/// invoice then had no route to a balance at all.
///
/// Conflicts on `payment_id` are ignored: the same invoice being stored twice is a
/// no-op, not an error.
pub async fn store_pending_payment(
    conn: &DatabaseConnection,
    payment_id: &str,
    telegram_user_id: i64,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO pending_payments (payment_id, telegram_user_id, created_at, expires_at) \
         VALUES ($1, $2, TIMEZONE('Asia/Ashgabat', now()), TIMEZONE('Asia/Ashgabat', now()) + INTERVAL '24 hours') \
         ON CONFLICT (payment_id) DO NOTHING",
        [
            Value::String(Some(Box::new(payment_id.to_string()))),
            Value::BigInt(Some(telegram_user_id)),
        ],
    );
    conn.execute(stmt).await?;

    Ok(())
}

/// One invoice the poller is responsible for.
#[derive(Debug, FromQueryResult)]
pub struct PendingPayment {
    pub payment_id: String,
    pub telegram_user_id: i64,
    /// True once the deadline has passed — the poller force-expires these rather than
    /// letting an invoice the provider never resolves be polled forever.
    pub is_overdue: bool,
    /// True once we've already told the user their funds were seen, so the
    /// "payment detected" message fires exactly once even across a restart.
    pub already_detected: bool,
}

/// Every invoice still being watched, oldest first.
///
/// Deliberately unfiltered by expiry: an overdue row still needs one last look (the
/// provider may have settled it) and then a force-expire. Deletion is driven by
/// terminal status, not by age.
pub async fn get_pending_payments(conn: &DatabaseConnection) -> Result<Vec<PendingPayment>, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT payment_id, telegram_user_id, \
                (expires_at <= TIMEZONE('Asia/Ashgabat', now())) AS is_overdue, \
                (detected_at IS NOT NULL) AS already_detected \
         FROM pending_payments ORDER BY created_at ASC"
            .to_string(),
    );
    PendingPayment::find_by_statement(stmt).all(conn).await
}

/// Remove one invoice once it reaches a terminal status.
///
/// Keyed on payment_id, not telegram_user_id: now that a user may hold several open
/// invoices, deleting by user would silently drop the ones still being watched.
pub async fn remove_pending_payment_by_id(
    conn: &DatabaseConnection,
    payment_id: &str,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM pending_payments WHERE payment_id = $1",
        [Value::String(Some(Box::new(payment_id.to_string())))],
    );
    conn.execute(stmt).await?;

    Ok(())
}

/// Stamp the moment funds were first seen for this invoice.
///
/// `WHERE detected_at IS NULL` makes this idempotent, so a racing second call can't
/// move the timestamp and can't cause a second notification.
pub async fn mark_payment_detected(
    conn: &DatabaseConnection,
    payment_id: &str,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE pending_payments SET detected_at = TIMEZONE('Asia/Ashgabat', now()) \
         WHERE payment_id = $1 AND detected_at IS NULL",
        [Value::String(Some(Box::new(payment_id.to_string())))],
    );
    conn.execute(stmt).await?;

    Ok(())
}

/// Retire one invoice: copy it into `reconcile_candidates` and delete it, atomically.
///
/// Force-expiry DELETEs the `pending_payments` row, and an invoice that was never
/// credited has no `transactions` row either — so a bare delete destroys the only local
/// record that the invoice existed, and with it any chance of noticing later that the
/// money arrived. `reconcile_candidates` is the right home: the poller never reads it,
/// so an archived row can only be credited after an admin has seen it in a dry run.
///
/// Archive and DELETE share one statement and one snapshot deliberately. An earlier
/// version archived best-effort and deleted anyway; that is precisely the behaviour
/// `cleanup_expired_payments` was changed to avoid, and having the two retirement paths
/// disagree about whether a failed archive may still delete was the bug waiting to
/// happen. If the INSERT fails, nothing is deleted and the next tick retries.
///
/// Copies the ORIGINAL `created_at` rather than stamping now(), so the dry-run listing
/// shows when the user actually paid. `ON CONFLICT DO NOTHING` keeps it idempotent and
/// leaves a row already resolved by a past reconcile resolved.
pub async fn archive_and_remove_pending_payment(
    conn: &DatabaseConnection,
    payment_id: &str,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "WITH doomed AS ( \
             SELECT payment_id, telegram_user_id, created_at \
             FROM pending_payments WHERE payment_id = $1 \
         ), \
         archived AS ( \
             INSERT INTO reconcile_candidates (payment_id, telegram_user_id, created_at) \
             SELECT payment_id, telegram_user_id, created_at FROM doomed \
             ON CONFLICT (payment_id) DO NOTHING \
         ) \
         DELETE FROM pending_payments WHERE payment_id IN (SELECT payment_id FROM doomed)",
        [Value::String(Some(Box::new(payment_id.to_string())))],
    );
    conn.execute(stmt).await?;

    Ok(())
}

/// Record which Telegram user owns a newly created Marzban account.
///
/// A recovery record, not the runtime ownership check — that reads the account's note.
/// The note is free text any panel reseller can edit, and before this table a cleared
/// note was an unrecoverable lockout: nothing else mapped an account to a user, since
/// `transactions.description` is a literal string that names no account.
///
/// `ON CONFLICT DO UPDATE`, not `DO NOTHING`. A Marzban username is freed when the
/// account is deleted (`DELETE /api/user/{name}`, or `USERS_AUTODELETE_DAYS`), so a name
/// can genuinely change hands: user A buys `bob`, a reseller deletes the expired account,
/// user B then passes the availability check and buys `bob`. `DO NOTHING` would leave the
/// recovery row pointing at A — worse than having no row at all, since restoring a wiped
/// note from it would hand B's account to A. The most recent create is the owner.
pub async fn record_marzban_account(
    conn: &DatabaseConnection,
    marzban_username: &str,
    telegram_id: i64,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO marzban_accounts (marzban_username, telegram_id, created_at) \
         VALUES ($1, $2, TIMEZONE('Asia/Ashgabat', now())) \
         ON CONFLICT (marzban_username) DO UPDATE \
             SET telegram_id = EXCLUDED.telegram_id, created_at = EXCLUDED.created_at",
        [
            Value::String(Some(Box::new(marzban_username.to_string()))),
            Value::BigInt(Some(telegram_id)),
        ],
    );
    conn.execute(stmt).await?;

    Ok(())
}

/// Create `marzban_accounts` if it is missing.
///
/// Applied at startup for the same reason as `ensure_subscription_tables`: CI ships only
/// the binary and never runs `migrations/`, so a table introduced with a code change that
/// depends on it would otherwise be missing on deploy. `migrations/012` carries the same
/// DDL so a schema built from `migrations/` matches one built by the binary.
pub async fn ensure_marzban_accounts_table(conn: &DatabaseConnection) -> Result<(), DbErr> {
    conn.execute(Statement::from_string(
        DbBackend::Postgres,
        "CREATE TABLE IF NOT EXISTS marzban_accounts (\
            marzban_username VARCHAR PRIMARY KEY, \
            telegram_id BIGINT NOT NULL, \
            created_at TIMESTAMP NOT NULL DEFAULT timezone('Asia/Ashgabat', now())\
        )"
        .to_string(),
    ))
    .await?;
    conn.execute(Statement::from_string(
        DbBackend::Postgres,
        "CREATE INDEX IF NOT EXISTS idx_marzban_accounts_telegram_id \
         ON marzban_accounts (telegram_id)"
            .to_string(),
    ))
    .await?;

    Ok(())
}

/// Get the user's preferred language.
pub async fn get_user_language(conn: &DatabaseConnection, telegram_id: i64) -> Result<Lang, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct LangResult {
        language: String,
    }

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT language FROM users WHERE telegram_id = $1 LIMIT 1",
        [Value::BigInt(Some(telegram_id))],
    );

    let result = LangResult::find_by_statement(stmt).one(conn).await?;

    Ok(result
        .map(|r| Lang::from_code(&r.language))
        .unwrap_or_default())
}

/// Set the user's preferred language.
pub async fn set_user_language(
    conn: &DatabaseConnection,
    telegram_id: i64,
    lang: Lang,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE users SET language = $1 WHERE telegram_id = $2",
        [
            Value::String(Some(Box::new(lang.as_code().to_string()))),
            Value::BigInt(Some(telegram_id)),
        ],
    );
    conn.execute(stmt).await?;
    Ok(())
}

/// Create subscription_types and subscriptions tables if they don't exist.
pub async fn ensure_subscription_tables(conn: &DatabaseConnection) -> Result<(), DbErr> {
    let stmt1 = Statement::from_string(
        DbBackend::Postgres,
        "CREATE TABLE IF NOT EXISTS subscription_types (\
            id SERIAL PRIMARY KEY, \
            name TEXT NOT NULL UNIQUE\
        )"
        .to_string(),
    );
    conn.execute(stmt1).await?;

    let stmt2 = Statement::from_string(
        DbBackend::Postgres,
        "CREATE TABLE IF NOT EXISTS subscriptions (\
            id SERIAL PRIMARY KEY, \
            subscription_type_id INTEGER NOT NULL REFERENCES subscription_types(id) ON DELETE CASCADE, \
            price DOUBLE PRECISION NOT NULL, \
            days INTEGER NOT NULL, \
            limit_gb DOUBLE PRECISION NOT NULL\
        )".to_string(),
    );
    conn.execute(stmt2).await?;

    // Migration: drop is_active column if it exists (from older schema)
    let migrate = Statement::from_string(
        DbBackend::Postgres,
        "ALTER TABLE subscriptions DROP COLUMN IF EXISTS is_active".to_string(),
    );
    conn.execute(migrate).await?;

    // Migration 011, applied here too because CI ships only the binary and never
    // runs migrations/. Every plan needs a device count: the Marzban fork rejects
    // a user create where client_type is set and device_limit is not (and buy.rs
    // now always sends client_type="happ"). DEFAULT 2 keeps existing rows valid.
    let device_limit = Statement::from_string(
        DbBackend::Postgres,
        "ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS device_limit INTEGER NOT NULL DEFAULT 2"
            .to_string(),
    );
    conn.execute(device_limit).await?;

    Ok(())
}

#[derive(Debug, FromQueryResult)]
pub struct SubscriptionInfo {
    pub id: i32,
    pub type_name: String,
    pub price: f64,
    pub days: i32,
    pub limit_gb: f64,
    /// Devices this plan allows. Sent to Marzban as `device_limit` alongside
    /// `client_type: "happ"` — the panel rejects one without the other.
    pub device_limit: i32,
}

/// Get all subscriptions joined with their type name.
pub async fn get_all_subscriptions(
    conn: &DatabaseConnection,
) -> Result<Vec<SubscriptionInfo>, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT s.id, st.name AS type_name, s.price, s.days, s.limit_gb, s.device_limit \
         FROM subscriptions s \
         JOIN subscription_types st ON st.id = s.subscription_type_id \
         ORDER BY s.id"
            .to_string(),
    );
    SubscriptionInfo::find_by_statement(stmt).all(conn).await
}

/// Get a single subscription by ID joined with its type name.
pub async fn get_subscription_by_id(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<Option<SubscriptionInfo>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT s.id, st.name AS type_name, s.price, s.days, s.limit_gb, s.device_limit \
         FROM subscriptions s \
         JOIN subscription_types st ON st.id = s.subscription_type_id \
         WHERE s.id = $1",
        [Value::Int(Some(id))],
    );
    SubscriptionInfo::find_by_statement(stmt).one(conn).await
}

/// Long-stop cleanup of `pending_payments`.
///
/// The payment poller owns normal deletion — a row goes away the moment the provider
/// reports a terminal status, or when the poller force-expires it past its deadline.
/// This is only here to stop unbounded growth from rows the poller can never resolve
/// (e.g. a payment_id NOWPayments no longer recognises).
///
/// The window is deliberately far wider than the 24h `expires_at`: deleting on expiry
/// would destroy the payment ids the poller still needs, which is exactly how the
/// bot's historical invoices were lost.
/// Archives before it deletes, in one statement.
///
/// `archived` is referenced by nothing and still runs: PostgreSQL executes a
/// data-modifying CTE exactly once and to completion whether or not anything selects
/// from it. Deleting that "unused" CTE, or wrapping it in a `WHERE EXISTS`, silently
/// turns this back into a bare DELETE that destroys invoices. This is now where the *doubtful*
/// invoices end up: since the poller force-expires only on a positive `waiting`, every
/// invoice we could not make sense of — a mismatched order_id, a provider status we
/// don't recognise, a lookup that kept failing — survives its deadline and dies here
/// instead. Those are exactly the rows most likely to be a real payment, so deleting
/// them bare would destroy the only local record they ever existed. Archive and DELETE
/// share one statement and one snapshot, so a crash between them is not possible.
pub async fn cleanup_expired_payments(conn: &DatabaseConnection) -> Result<u64, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "WITH doomed AS ( \
             SELECT payment_id, telegram_user_id, created_at FROM pending_payments \
             WHERE created_at <= TIMEZONE('Asia/Ashgabat', now()) - INTERVAL '30 days' \
         ), \
         archived AS ( \
             INSERT INTO reconcile_candidates (payment_id, telegram_user_id, created_at) \
             SELECT payment_id, telegram_user_id, created_at FROM doomed \
             ON CONFLICT (payment_id) DO NOTHING \
         ) \
         DELETE FROM pending_payments \
         WHERE payment_id IN (SELECT payment_id FROM doomed)"
            .to_string(),
    );
    let result = conn.execute(stmt).await?;

    Ok(result.rows_affected())
}
