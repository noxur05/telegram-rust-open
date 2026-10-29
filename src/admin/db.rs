use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement, Value,
};

/// Result struct for admin stats
#[derive(Debug, FromQueryResult)]
pub struct AdminStats {
    pub user_count: i64,
    pub transaction_count: i64,
    pub total_revenue: Option<rust_decimal::Decimal>,
    pub pending_payment_count: i64,
}

/// Get admin dashboard stats.
pub async fn get_admin_stats(conn: &DatabaseConnection) -> Result<AdminStats, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT \
            (SELECT COUNT(*) FROM users WHERE is_bot = false) AS user_count, \
            (SELECT COUNT(*) FROM transactions) AS transaction_count, \
            (SELECT COALESCE(SUM(amount), 0) FROM transactions WHERE amount > 0) AS total_revenue, \
            (SELECT COUNT(*) FROM pending_payments WHERE expires_at > TIMEZONE('Asia/Ashgabat', now())) AS pending_payment_count"
            .to_string(),
    );

    AdminStats::find_by_statement(stmt)
        .one(conn)
        .await?
        .ok_or_else(|| DbErr::Custom("Failed to fetch admin stats".to_string()))
}

/// Result struct for user lookup
#[derive(Debug, FromQueryResult)]
pub struct AdminUserInfo {
    pub id: i32,
    pub username: Option<String>,
    pub telegram_id: i64,
    pub balance: rust_decimal::Decimal,
    pub created_at: String,
}

/// Get user info by telegram_id for admin lookup.
pub async fn get_user_by_telegram_id(
    conn: &DatabaseConnection,
    telegram_id: i64,
) -> Result<Option<AdminUserInfo>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, username, telegram_id, balance, \
         TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS created_at \
         FROM users WHERE telegram_id = $1 LIMIT 1",
        [Value::BigInt(Some(telegram_id))],
    );

    AdminUserInfo::find_by_statement(stmt).one(conn).await
}

/// Result struct for user transactions
#[derive(Debug, FromQueryResult)]
pub struct AdminTransaction {
    pub id: i32,
    pub amount: rust_decimal::Decimal,
    pub description: Option<String>,
    pub created_at: String,
}

/// Get last N transactions for a user.
pub async fn get_user_transactions(
    conn: &DatabaseConnection,
    user_id: i32,
    limit: u64,
) -> Result<Vec<AdminTransaction>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, amount, description, \
         TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS created_at \
         FROM transactions WHERE user_id = $1 \
         ORDER BY created_at DESC LIMIT $2",
        [Value::Int(Some(user_id)), Value::BigInt(Some(limit as i64))],
    );

    AdminTransaction::find_by_statement(stmt).all(conn).await
}

/// Get paginated transactions for a specific user.
pub async fn get_user_transactions_paginated(
    conn: &DatabaseConnection,
    user_id: i32,
    offset: i64,
    limit: i64,
) -> Result<Vec<AdminTransaction>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, amount, description, \
         TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS created_at \
         FROM transactions WHERE user_id = $1 \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        [
            Value::Int(Some(user_id)),
            Value::BigInt(Some(limit)),
            Value::BigInt(Some(offset)),
        ],
    );

    AdminTransaction::find_by_statement(stmt).all(conn).await
}

/// Get transaction count for a specific user.
pub async fn get_user_transaction_count(
    conn: &DatabaseConnection,
    user_id: i32,
) -> Result<i64, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct CountResult {
        count: i64,
    }

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT COUNT(*) AS count FROM transactions WHERE user_id = $1",
        [Value::Int(Some(user_id))],
    );

    let result = CountResult::find_by_statement(stmt).one(conn).await?;
    Ok(result.map(|r| r.count).unwrap_or(0))
}

/// Adjust user balance atomically (can be positive or negative).
/// Prevents balance from going below zero.
/// Uses a single CTE query to ensure UPDATE + INSERT are atomic.
pub async fn admin_adjust_balance(
    conn: &DatabaseConnection,
    telegram_id: i64,
    amount: rust_decimal::Decimal,
    description: &str,
) -> Result<rust_decimal::Decimal, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct AdjustResult {
        balance: rust_decimal::Decimal,
    }

    // For negative amounts, add a balance check condition
    let balance_condition = if amount < rust_decimal::Decimal::ZERO {
        "AND balance + $1 >= 0"
    } else {
        ""
    };

    let sql = format!(
        "WITH updated AS ( \
            UPDATE users SET balance = balance + $1 \
            WHERE telegram_id = $2 {} \
            RETURNING id, balance \
        ) \
        INSERT INTO transactions (user_id, amount, balance_after, description, payment_id, created_at) \
        SELECT id, $1, balance, $3, NULL, TIMEZONE('Asia/Ashgabat', now()) FROM updated \
        RETURNING balance_after AS balance",
        balance_condition
    );

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        &sql,
        [
            Value::Decimal(Some(Box::new(amount))),
            Value::BigInt(Some(telegram_id)),
            Value::String(Some(Box::new(description.to_string()))),
        ],
    );

    let result = AdjustResult::find_by_statement(stmt).one(conn).await?;

    result
        .map(|r| r.balance)
        .ok_or_else(|| DbErr::Custom("User not found or insufficient balance".to_string()))
}

/// Get paginated user list for admin.
pub async fn get_users_paginated(
    conn: &DatabaseConnection,
    offset: i64,
    limit: i64,
) -> Result<Vec<AdminUserInfo>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, username, telegram_id, balance, \
         TO_CHAR(created_at, 'YYYY-MM-DD HH24:MI') AS created_at \
         FROM users WHERE is_bot = false \
         ORDER BY id DESC LIMIT $1 OFFSET $2",
        [Value::BigInt(Some(limit)), Value::BigInt(Some(offset))],
    );

    AdminUserInfo::find_by_statement(stmt).all(conn).await
}

/// Get total non-bot user count.
pub async fn get_user_count(conn: &DatabaseConnection) -> Result<i64, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct CountResult {
        count: i64,
    }

    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT COUNT(*) AS count FROM users WHERE is_bot = false".to_string(),
    );

    let result = CountResult::find_by_statement(stmt).one(conn).await?;
    Ok(result.map(|r| r.count).unwrap_or(0))
}

/// Get paginated transactions for admin (all users).
pub async fn get_all_transactions_paginated(
    conn: &DatabaseConnection,
    offset: i64,
    limit: i64,
) -> Result<Vec<AdminTransactionFull>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT t.id, t.amount, t.balance_after, t.description, \
         TO_CHAR(t.created_at, 'YYYY-MM-DD HH24:MI') AS created_at, \
         u.telegram_id, u.username \
         FROM transactions t \
         JOIN users u ON u.id = t.user_id \
         ORDER BY t.created_at DESC LIMIT $1 OFFSET $2",
        [Value::BigInt(Some(limit)), Value::BigInt(Some(offset))],
    );

    AdminTransactionFull::find_by_statement(stmt)
        .all(conn)
        .await
}

/// Get total transaction count.
pub async fn get_transaction_count(conn: &DatabaseConnection) -> Result<i64, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct CountResult {
        count: i64,
    }

    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT COUNT(*) AS count FROM transactions".to_string(),
    );

    let result = CountResult::find_by_statement(stmt).one(conn).await?;
    Ok(result.map(|r| r.count).unwrap_or(0))
}

/// Result struct for full transaction view (with user info).
#[derive(Debug, FromQueryResult)]
pub struct AdminTransactionFull {
    pub id: i32,
    pub amount: rust_decimal::Decimal,
    pub balance_after: rust_decimal::Decimal,
    pub description: Option<String>,
    pub created_at: String,
    pub telegram_id: i64,
    pub username: Option<String>,
}

/// Get a single transaction by ID with user info.
pub async fn get_transaction_by_id(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<Option<AdminTransactionFull>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT t.id, t.amount, t.balance_after, t.description, \
         TO_CHAR(t.created_at, 'YYYY-MM-DD HH24:MI') AS created_at, \
         u.telegram_id, u.username \
         FROM transactions t \
         JOIN users u ON u.id = t.user_id \
         WHERE t.id = $1 LIMIT 1",
        [Value::Int(Some(id))],
    );

    AdminTransactionFull::find_by_statement(stmt)
        .one(conn)
        .await
}

// ─── Subscription Types CRUD ───

pub async fn create_subscription_type(conn: &DatabaseConnection, name: &str) -> Result<i32, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct IdResult {
        id: i32,
    }

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO subscription_types (name) VALUES ($1) RETURNING id",
        [Value::String(Some(Box::new(name.to_string())))],
    );
    let result = IdResult::find_by_statement(stmt).one(conn).await?;
    Ok(result.map(|r| r.id).unwrap_or(0))
}

#[derive(Debug, FromQueryResult)]
pub struct SubscriptionType {
    pub id: i32,
    pub name: String,
}

pub async fn get_all_subscription_types(
    conn: &DatabaseConnection,
) -> Result<Vec<SubscriptionType>, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT id, name FROM subscription_types ORDER BY id".to_string(),
    );
    SubscriptionType::find_by_statement(stmt).all(conn).await
}

pub async fn get_subscription_type_by_id(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<Option<SubscriptionType>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, name FROM subscription_types WHERE id = $1",
        [Value::Int(Some(id))],
    );
    SubscriptionType::find_by_statement(stmt).one(conn).await
}

pub async fn delete_subscription_type(conn: &DatabaseConnection, id: i32) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM subscription_types WHERE id = $1",
        [Value::Int(Some(id))],
    );
    conn.execute(stmt).await?;
    Ok(())
}

// ─── Subscriptions CRUD ───

#[derive(Debug, FromQueryResult)]
pub struct Subscription {
    pub id: i32,
    pub subscription_type_id: i32,
    pub price: f64,
    pub days: i32,
    pub limit_gb: f64,
    pub device_limit: i32,
}

pub async fn create_subscription(
    conn: &DatabaseConnection,
    type_id: i32,
    price: f64,
    days: i32,
    limit_gb: f64,
    device_limit: i32,
) -> Result<i32, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct IdResult {
        id: i32,
    }

    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO subscriptions (subscription_type_id, price, days, limit_gb, device_limit) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
        [
            Value::Int(Some(type_id)),
            Value::Double(Some(price)),
            Value::Int(Some(days)),
            Value::Double(Some(limit_gb)),
            Value::Int(Some(device_limit)),
        ],
    );
    let result = IdResult::find_by_statement(stmt).one(conn).await?;
    Ok(result.map(|r| r.id).unwrap_or(0))
}

pub async fn get_subscriptions_by_type(
    conn: &DatabaseConnection,
    type_id: i32,
) -> Result<Vec<Subscription>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, subscription_type_id, price, days, limit_gb, device_limit \
         FROM subscriptions WHERE subscription_type_id = $1 ORDER BY id",
        [Value::Int(Some(type_id))],
    );
    Subscription::find_by_statement(stmt).all(conn).await
}

pub async fn get_subscription_by_id(
    conn: &DatabaseConnection,
    id: i32,
) -> Result<Option<Subscription>, DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT id, subscription_type_id, price, days, limit_gb, device_limit \
         FROM subscriptions WHERE id = $1",
        [Value::Int(Some(id))],
    );
    Subscription::find_by_statement(stmt).one(conn).await
}

pub async fn update_subscription(
    conn: &DatabaseConnection,
    id: i32,
    price: f64,
    days: i32,
    limit_gb: f64,
    device_limit: i32,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE subscriptions SET price = $1, days = $2, limit_gb = $3, device_limit = $4 \
         WHERE id = $5",
        [
            Value::Double(Some(price)),
            Value::Int(Some(days)),
            Value::Double(Some(limit_gb)),
            Value::Int(Some(device_limit)),
            Value::Int(Some(id)),
        ],
    );
    conn.execute(stmt).await?;
    Ok(())
}

pub async fn delete_subscription(conn: &DatabaseConnection, id: i32) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM subscriptions WHERE id = $1",
        [Value::Int(Some(id))],
    );
    conn.execute(stmt).await?;
    Ok(())
}

/// Get all non-bot users with usernames for broadcast delivery report.
pub async fn get_all_users_for_broadcast(
    conn: &DatabaseConnection,
) -> Result<Vec<(i64, Option<String>)>, DbErr> {
    #[derive(Debug, FromQueryResult)]
    struct UserBroadcast {
        telegram_id: i64,
        username: Option<String>,
    }

    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT telegram_id, username FROM users WHERE is_bot = false".to_string(),
    );

    let results = UserBroadcast::find_by_statement(stmt).all(conn).await?;

    Ok(results
        .into_iter()
        .map(|r| (r.telegram_id, r.username))
        .collect())
}

// ─── Historical payment reconciliation ───

/// A historical invoice that predates the payment poller and was never checked.
#[derive(Debug, FromQueryResult)]
pub struct ReconcileCandidate {
    pub payment_id: String,
    pub telegram_user_id: i64,
    pub created_at: String,
}

/// Candidates not yet examined, oldest first.
pub async fn get_unresolved_candidates(
    conn: &DatabaseConnection,
) -> Result<Vec<ReconcileCandidate>, DbErr> {
    let stmt = Statement::from_string(
        DbBackend::Postgres,
        "SELECT payment_id, telegram_user_id, \
                TO_CHAR(created_at, 'YYYY-MM-DD') AS created_at \
         FROM reconcile_candidates WHERE resolved_at IS NULL \
         ORDER BY created_at ASC"
            .to_string(),
    );
    ReconcileCandidate::find_by_statement(stmt).all(conn).await
}

/// Stamp a candidate as examined, whatever the outcome, so a later run doesn't
/// re-list it. `resolution` records what was observed (a status, or a skip reason).
pub async fn resolve_candidate(
    conn: &DatabaseConnection,
    payment_id: &str,
    resolution: &str,
) -> Result<(), DbErr> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE reconcile_candidates \
         SET resolved_at = TIMEZONE('Asia/Ashgabat', now()), resolution = $2 \
         WHERE payment_id = $1",
        [
            Value::String(Some(Box::new(payment_id.to_string()))),
            Value::String(Some(Box::new(resolution.to_string()))),
        ],
    );
    conn.execute(stmt).await?;
    Ok(())
}
