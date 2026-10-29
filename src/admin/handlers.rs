use std::collections::HashMap;
use std::sync::LazyLock;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message};
use tokio::sync::Mutex;

use super::buttons;
use super::db;
use crate::config::get_config;

const PAGE_SIZE: i64 = 6;

// ─── Admin State Machine ───

// The `WaitingFor` prefix is the point: every variant names a pending text
// input the router is blocked on. Renaming them to satisfy the lint would make
// the state machine read worse, so it's silenced here rather than worked around.
#[allow(clippy::enum_variant_names)]
#[derive(Debug)]
enum AdminState {
    WaitingForBroadcastText,
    WaitingForUserLookupId,
    WaitingForBalanceUserId,
    WaitingForBalanceAmount {
        target_telegram_id: i64,
    },
    // Subscription management states
    WaitingForSubTypeName,
    WaitingForSubPrice {
        type_id: i32,
    },
    WaitingForSubDays {
        type_id: i32,
        price: f64,
    },
    WaitingForSubLimitGb {
        type_id: i32,
        price: f64,
        days: i32,
    },
    WaitingForSubDeviceLimit {
        type_id: i32,
        price: f64,
        days: i32,
        limit_gb: f64,
    },
    WaitingForSubEditPrice {
        sub_id: i32,
    },
    WaitingForSubEditDays {
        sub_id: i32,
        price: f64,
    },
    WaitingForSubEditLimitGb {
        sub_id: i32,
        price: f64,
        days: i32,
    },
    WaitingForSubEditDeviceLimit {
        sub_id: i32,
        price: f64,
        days: i32,
        limit_gb: f64,
    },
}

#[derive(Debug)]
struct PendingBalanceAdjustment {
    target_telegram_id: i64,
    amount: rust_decimal::Decimal,
}

#[derive(Debug)]
enum PendingSubAction {
    Create {
        type_id: i32,
        price: f64,
        days: i32,
        limit_gb: f64,
        device_limit: i32,
    },
    Edit {
        sub_id: i32,
        price: f64,
        days: i32,
        limit_gb: f64,
        device_limit: i32,
    },
    Delete {
        sub_id: i32,
        type_id: i32,
    },
    DeleteType {
        type_id: i32,
    },
}

static ADMIN_STATE: LazyLock<Mutex<HashMap<i64, AdminState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static PENDING_BALANCE_ADJUSTMENTS: LazyLock<Mutex<HashMap<i64, PendingBalanceAdjustment>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static PENDING_SUB_ACTIONS: LazyLock<Mutex<HashMap<i64, PendingSubAction>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// ─── Helpers ───

pub fn is_admin(telegram_id: i64) -> bool {
    get_config().admin_telegram_id == Some(telegram_id)
}

pub async fn clear_admin_state(telegram_id: i64) {
    super::reconcile::clear_pending(telegram_id).await;
    ADMIN_STATE.lock().await.remove(&telegram_id);
    PENDING_BALANCE_ADJUSTMENTS
        .lock()
        .await
        .remove(&telegram_id);
    PENDING_SUB_ACTIONS.lock().await.remove(&telegram_id);
}

fn admin_menu_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("📊 Stats", buttons::CB_ADMIN_STATS),
            InlineKeyboardButton::callback("📢 Broadcast", buttons::CB_ADMIN_BROADCAST),
        ],
        vec![
            InlineKeyboardButton::callback("👥 Users", buttons::CB_ADMIN_USERS_LIST),
            InlineKeyboardButton::callback("📜 Transactions", buttons::CB_ADMIN_TXNS_LIST),
        ],
        vec![
            InlineKeyboardButton::callback("🔍 User Lookup", buttons::CB_ADMIN_USER_LOOKUP),
            InlineKeyboardButton::callback("💰 Balance Mgmt", buttons::CB_ADMIN_BALANCE_MGMT),
        ],
        vec![InlineKeyboardButton::callback(
            "⚙️ Subscriptions",
            buttons::CB_ADMIN_SUBSCRIPTIONS,
        )],
        vec![InlineKeyboardButton::callback(
            "🔍 Reconcile payments",
            buttons::CB_ADMIN_RECONCILE,
        )],
    ])
}

fn back_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "⬅️ Back",
        buttons::CB_ADMIN_BACK,
    )]])
}

fn pagination_rows(prefix: &str, page: i64, total_pages: i64) -> Vec<Vec<InlineKeyboardButton>> {
    let mut nav_row = Vec::new();
    if page > 0 {
        nav_row.push(InlineKeyboardButton::callback(
            "⬅️ Prev",
            format!("{}{}", prefix, page - 1),
        ));
    }
    nav_row.push(InlineKeyboardButton::callback(
        format!("{}/{}", page + 1, total_pages),
        "noop".to_string(),
    ));
    if page < total_pages - 1 {
        nav_row.push(InlineKeyboardButton::callback(
            "➡️ Next",
            format!("{}{}", prefix, page + 1),
        ));
    }

    vec![
        nav_row,
        vec![InlineKeyboardButton::callback(
            "⬅️ Back to Menu",
            buttons::CB_ADMIN_BACK,
        )],
    ]
}

// ─── Command Handler ───

pub async fn handle_admin_command(bot: &Bot, msg: &Message) {
    let Some(user) = msg.from.as_ref() else {
        return;
    };
    let telegram_id = user.id.0 as i64;

    clear_admin_state(telegram_id).await;

    let _ = bot
        .send_message(msg.chat.id, "🔧 Admin Panel\n\nSelect an action:")
        .reply_markup(admin_menu_keyboard())
        .await;
}

// ─── Text Input Router ───

pub async fn handle_admin_text_input(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    msg: &Message,
) -> bool {
    let Some(user) = msg.from.as_ref() else {
        return false;
    };
    let telegram_id = user.id.0 as i64;

    if !is_admin(telegram_id) {
        return false;
    }

    let Some(state) = ADMIN_STATE.lock().await.remove(&telegram_id) else {
        return false;
    };

    let Some(text) = msg.text() else {
        ADMIN_STATE.lock().await.insert(telegram_id, state);
        return false;
    };

    match state {
        AdminState::WaitingForBroadcastText => {
            super::broadcast::handle_admin_broadcast(bot, db, msg).await;
            true
        }
        AdminState::WaitingForUserLookupId => {
            handle_user_lookup_input(bot, db, msg, text).await;
            true
        }
        AdminState::WaitingForBalanceUserId => {
            handle_balance_user_id_input(bot, db, msg, text).await;
            true
        }
        AdminState::WaitingForBalanceAmount { target_telegram_id } => {
            handle_balance_amount_input(bot, db, msg, text, telegram_id, target_telegram_id).await;
            true
        }
        AdminState::WaitingForSubTypeName => {
            handle_sub_type_name_input(bot, db, msg, text, telegram_id).await;
            true
        }
        AdminState::WaitingForSubPrice { type_id } => {
            handle_sub_price_input(bot, msg, text, telegram_id, type_id).await;
            true
        }
        AdminState::WaitingForSubDays { type_id, price } => {
            handle_sub_days_input(bot, msg, text, telegram_id, type_id, price).await;
            true
        }
        AdminState::WaitingForSubLimitGb {
            type_id,
            price,
            days,
        } => {
            handle_sub_limit_input(bot, msg, text, telegram_id, type_id, price, days).await;
            true
        }
        AdminState::WaitingForSubDeviceLimit {
            type_id,
            price,
            days,
            limit_gb,
        } => {
            handle_sub_device_limit_input(
                bot,
                msg,
                text,
                telegram_id,
                type_id,
                price,
                days,
                limit_gb,
            )
            .await;
            true
        }
        AdminState::WaitingForSubEditPrice { sub_id } => {
            handle_sub_edit_price_input(bot, msg, text, telegram_id, sub_id).await;
            true
        }
        AdminState::WaitingForSubEditDays { sub_id, price } => {
            handle_sub_edit_days_input(bot, msg, text, telegram_id, sub_id, price).await;
            true
        }
        AdminState::WaitingForSubEditLimitGb {
            sub_id,
            price,
            days,
        } => {
            handle_sub_edit_limit_input(bot, msg, text, telegram_id, sub_id, price, days).await;
            true
        }
        AdminState::WaitingForSubEditDeviceLimit {
            sub_id,
            price,
            days,
            limit_gb,
        } => {
            handle_sub_edit_device_limit_input(
                bot,
                msg,
                text,
                telegram_id,
                sub_id,
                price,
                days,
                limit_gb,
            )
            .await;
            true
        }
    }
}

// ─── Callback Handlers ───

pub async fn handle_stats_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    match self::db::get_admin_stats(db).await {
        Ok(stats) => {
            let revenue = stats.total_revenue.unwrap_or_default();
            let text = format!(
                "📊 Stats\n\n\
                 👥 Users: {}\n\
                 💸 Transactions: {}\n\
                 💰 Total revenue: {}\n\
                 ⏳ Pending payments: {}",
                stats.user_count, stats.transaction_count, revenue, stats.pending_payment_count
            );
            let _ = bot
                .send_message(chat_id, text)
                .reply_markup(back_keyboard())
                .await;
        }
        Err(e) => {
            eprintln!("Admin stats error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Failed to fetch stats.").await;
        }
    }
}

// ─── Paginated Users List ───

pub async fn handle_users_list_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    page: i64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(cq_msg) = &cq.message else { return };
    let chat_id = cq_msg.chat().id;

    let total = match self::db::get_user_count(db).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("User count error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Failed to fetch users.").await;
            return;
        }
    };

    let total_pages = ((total as f64) / (PAGE_SIZE as f64)).ceil() as i64;
    let total_pages = total_pages.max(1);
    let page = page.clamp(0, total_pages - 1);
    let offset = page * PAGE_SIZE;

    let users = match self::db::get_users_paginated(db, offset, PAGE_SIZE).await {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Users list error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Failed to fetch users.").await;
            return;
        }
    };

    let text = format!(
        "👥 Users ({} total) — Page {}/{}",
        total,
        page + 1,
        total_pages
    );

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    if users.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback(
            "No users found",
            "noop",
        )]);
    } else {
        for u in &users {
            let label = match &u.username {
                Some(name) => format!("@{} | {:.2} USD", name, u.balance),
                None => format!("{} | {:.2} USD", u.telegram_id, u.balance),
            };
            let cb = format!(
                "{}{}:{}",
                buttons::CB_ADMIN_USER_DETAIL_PREFIX,
                u.telegram_id,
                page
            );
            rows.push(vec![InlineKeyboardButton::callback(label, cb)]);
        }
    }
    rows.extend(pagination_rows(
        buttons::CB_ADMIN_USERS_PAGE_PREFIX,
        page,
        total_pages,
    ));

    let kb = InlineKeyboardMarkup::new(rows);

    let res = bot
        .edit_message_text(chat_id, cq_msg.id(), &text)
        .reply_markup(kb.clone())
        .await;
    if res.is_err() {
        let _ = bot.send_message(chat_id, &text).reply_markup(kb).await;
    }
}

// ─── Paginated Transactions List ───

pub async fn handle_txns_list_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    page: i64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(cq_msg) = &cq.message else { return };
    let chat_id = cq_msg.chat().id;

    let total = match self::db::get_transaction_count(db).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Transaction count error: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Failed to fetch transactions.")
                .await;
            return;
        }
    };

    let total_pages = ((total as f64) / (PAGE_SIZE as f64)).ceil() as i64;
    let total_pages = total_pages.max(1);
    let page = page.clamp(0, total_pages - 1);
    let offset = page * PAGE_SIZE;

    let txns = match self::db::get_all_transactions_paginated(db, offset, PAGE_SIZE).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Transactions list error: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Failed to fetch transactions.")
                .await;
            return;
        }
    };

    let text = format!(
        "📜 Transactions ({} total) — Page {}/{}",
        total,
        page + 1,
        total_pages
    );

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    if txns.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback(
            "No transactions found",
            "noop",
        )]);
    } else {
        for tx in &txns {
            let user_label = match &tx.username {
                Some(name) => format!("@{}", name),
                None => format!("{}", tx.telegram_id),
            };
            let date_short = if tx.created_at.len() >= 10 {
                &tx.created_at[..10]
            } else {
                &tx.created_at
            };
            let label = format!("{:.2} | {} | {}", tx.amount, user_label, date_short);
            let cb = format!("{}{}:{}", buttons::CB_ADMIN_TXN_DETAIL_PREFIX, tx.id, page);
            rows.push(vec![InlineKeyboardButton::callback(label, cb)]);
        }
    }
    rows.extend(pagination_rows(
        buttons::CB_ADMIN_TXNS_PAGE_PREFIX,
        page,
        total_pages,
    ));

    let kb = InlineKeyboardMarkup::new(rows);

    let res = bot
        .edit_message_text(chat_id, cq_msg.id(), &text)
        .reply_markup(kb.clone())
        .await;
    if res.is_err() {
        let _ = bot.send_message(chat_id, &text).reply_markup(kb).await;
    }
}

// ─── User Detail (with paginated transactions) ───

pub async fn handle_user_detail_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    telegram_id: i64,
    from_page: i64,
) {
    handle_user_detail_with_txn_page(bot, db, cq, telegram_id, from_page, 0).await;
}

pub async fn handle_user_txn_page_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    telegram_id: i64,
    txn_page: i64,
    from_page: i64,
) {
    handle_user_detail_with_txn_page(bot, db, cq, telegram_id, from_page, txn_page).await;
}

async fn handle_user_detail_with_txn_page(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    telegram_id: i64,
    from_page: i64,
    txn_page: i64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(cq_msg) = &cq.message else { return };
    let chat_id = cq_msg.chat().id;

    let user = match self::db::get_user_by_telegram_id(db, telegram_id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            let _ = bot.send_message(chat_id, "⚠️ User not found.").await;
            return;
        }
        Err(e) => {
            eprintln!("User detail error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Database error.").await;
            return;
        }
    };

    let text = format!(
        "👤 User Detail\n\n\
         ID: {}\n\
         Username: @{}\n\
         Telegram ID: {}\n\
         Balance: {:.2} USD\n\
         Joined: {}",
        user.id,
        user.username.as_deref().unwrap_or("N/A"),
        user.telegram_id,
        user.balance,
        user.created_at,
    );

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    // Fetch paginated transactions
    let txn_total = self::db::get_user_transaction_count(db, user.id)
        .await
        .unwrap_or(0);
    let txn_total_pages = ((txn_total as f64) / (PAGE_SIZE as f64)).ceil() as i64;
    let txn_total_pages = txn_total_pages.max(1);
    let txn_page = txn_page.clamp(0, txn_total_pages - 1);
    let txn_offset = txn_page * PAGE_SIZE;

    match self::db::get_user_transactions_paginated(db, user.id, txn_offset, PAGE_SIZE).await {
        Ok(txns) if !txns.is_empty() => {
            for tx in &txns {
                let date_short = if tx.created_at.len() >= 10 {
                    &tx.created_at[..10]
                } else {
                    &tx.created_at
                };
                let desc = tx.description.as_deref().unwrap_or("-");
                let desc_short = if desc.len() > 16 { &desc[..16] } else { desc };
                let label = format!("{:.2} | {} | {}", tx.amount, date_short, desc_short);
                let cb = format!(
                    "{}{}:{}:{}",
                    buttons::CB_ADMIN_USER_TXN_DETAIL_PREFIX,
                    tx.id,
                    telegram_id,
                    from_page
                );
                rows.push(vec![InlineKeyboardButton::callback(label, cb)]);
            }

            // Transaction pagination row
            if txn_total_pages > 1 {
                let mut nav_row = Vec::new();
                if txn_page > 0 {
                    nav_row.push(InlineKeyboardButton::callback(
                        "⬅️ Prev",
                        format!(
                            "{}{}:{}:{}",
                            buttons::CB_ADMIN_USER_TXN_PAGE_PREFIX,
                            telegram_id,
                            txn_page - 1,
                            from_page
                        ),
                    ));
                }
                nav_row.push(InlineKeyboardButton::callback(
                    format!("{}/{}", txn_page + 1, txn_total_pages),
                    "noop".to_string(),
                ));
                if txn_page < txn_total_pages - 1 {
                    nav_row.push(InlineKeyboardButton::callback(
                        "➡️ Next",
                        format!(
                            "{}{}:{}:{}",
                            buttons::CB_ADMIN_USER_TXN_PAGE_PREFIX,
                            telegram_id,
                            txn_page + 1,
                            from_page
                        ),
                    ));
                }
                rows.push(nav_row);
            }
        }
        Ok(_) => {
            rows.push(vec![InlineKeyboardButton::callback(
                "No transactions",
                "noop",
            )]);
        }
        Err(e) => {
            eprintln!("Failed to fetch transactions: {}", e);
            rows.push(vec![InlineKeyboardButton::callback(
                "⚠️ Error loading txns",
                "noop",
            )]);
        }
    }

    rows.push(vec![InlineKeyboardButton::callback(
        "⬅️ Back to Users",
        format!("{}{}", buttons::CB_ADMIN_USERS_PAGE_PREFIX, from_page),
    )]);

    let kb = InlineKeyboardMarkup::new(rows);

    let res = bot
        .edit_message_text(chat_id, cq_msg.id(), &text)
        .reply_markup(kb.clone())
        .await;
    if res.is_err() {
        let _ = bot.send_message(chat_id, &text).reply_markup(kb).await;
    }
}

// ─── User Transaction Detail ───

pub async fn handle_user_txn_detail_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    txn_id: i32,
    user_telegram_id: i64,
    from_users_page: i64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(cq_msg) = &cq.message else { return };
    let chat_id = cq_msg.chat().id;

    let tx = match self::db::get_transaction_by_id(db, txn_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            let _ = bot.send_message(chat_id, "⚠️ Transaction not found.").await;
            return;
        }
        Err(e) => {
            eprintln!("Transaction detail error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Database error.").await;
            return;
        }
    };

    let user_label = match &tx.username {
        Some(name) => format!("@{} ({})", name, tx.telegram_id),
        None => format!("{}", tx.telegram_id),
    };

    let text = format!(
        "📜 Transaction Detail\n\n\
         ID: {}\n\
         Amount: {:.2} USD\n\
         Balance After: {:.2} USD\n\
         User: {}\n\
         Description: {}\n\
         Date: {}",
        tx.id,
        tx.amount,
        tx.balance_after,
        user_label,
        tx.description.as_deref().unwrap_or("-"),
        tx.created_at,
    );

    // Back goes to user detail (page 0 of txns)
    let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "⬅️ Back to User",
        format!(
            "{}{}:{}",
            buttons::CB_ADMIN_USER_DETAIL_PREFIX,
            user_telegram_id,
            from_users_page
        ),
    )]]);

    let res = bot
        .edit_message_text(chat_id, cq_msg.id(), &text)
        .reply_markup(kb.clone())
        .await;
    if res.is_err() {
        let _ = bot.send_message(chat_id, &text).reply_markup(kb).await;
    }
}

// ─── Transaction Detail ───

pub async fn handle_txn_detail_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    txn_id: i32,
    from_page: i64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(cq_msg) = &cq.message else { return };
    let chat_id = cq_msg.chat().id;

    let tx = match self::db::get_transaction_by_id(db, txn_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            let _ = bot.send_message(chat_id, "⚠️ Transaction not found.").await;
            return;
        }
        Err(e) => {
            eprintln!("Transaction detail error: {}", e);
            let _ = bot.send_message(chat_id, "❌ Database error.").await;
            return;
        }
    };

    let user_label = match &tx.username {
        Some(name) => format!("@{} ({})", name, tx.telegram_id),
        None => format!("{}", tx.telegram_id),
    };

    let text = format!(
        "📜 Transaction Detail\n\n\
         ID: {}\n\
         Amount: {:.2} USD\n\
         Balance After: {:.2} USD\n\
         User: {}\n\
         Description: {}\n\
         Date: {}",
        tx.id,
        tx.amount,
        tx.balance_after,
        user_label,
        tx.description.as_deref().unwrap_or("-"),
        tx.created_at,
    );

    let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "⬅️ Back to Transactions",
        format!("{}{}", buttons::CB_ADMIN_TXNS_PAGE_PREFIX, from_page),
    )]]);

    let res = bot
        .edit_message_text(chat_id, cq_msg.id(), &text)
        .reply_markup(kb.clone())
        .await;
    if res.is_err() {
        let _ = bot.send_message(chat_id, &text).reply_markup(kb).await;
    }
}

// ─── Broadcast ───

pub async fn handle_broadcast_start_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    ADMIN_STATE
        .lock()
        .await
        .insert(telegram_id, AdminState::WaitingForBroadcastText);

    let _ = bot
        .send_message(
            chat_id,
            "📢 Type the broadcast message to send to all users.\n\nType /admin to cancel.",
        )
        .reply_markup(back_keyboard())
        .await;
}

// ─── User Lookup ───

pub async fn handle_user_lookup_start_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    ADMIN_STATE
        .lock()
        .await
        .insert(telegram_id, AdminState::WaitingForUserLookupId);

    let _ = bot
        .send_message(
            chat_id,
            "🔍 Enter the user's Telegram ID.\n\nType /admin to cancel.",
        )
        .reply_markup(back_keyboard())
        .await;
}

// ─── Balance Mgmt ───

pub async fn handle_balance_mgmt_start_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    ADMIN_STATE
        .lock()
        .await
        .insert(telegram_id, AdminState::WaitingForBalanceUserId);

    let _ = bot
        .send_message(
            chat_id,
            "💰 Enter the user's Telegram ID for balance adjustment.\n\nType /admin to cancel.",
        )
        .reply_markup(back_keyboard())
        .await;
}

pub async fn handle_balance_confirm_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let Some(pending) = PENDING_BALANCE_ADJUSTMENTS.lock().await.remove(&admin_id) else {
        let _ = bot
            .send_message(chat_id, "⚠️ No pending balance adjustment found.")
            .await;
        return;
    };

    let description = format!("Admin adjustment by {}", admin_id);
    match self::db::admin_adjust_balance(
        db,
        pending.target_telegram_id,
        pending.amount,
        &description,
    )
    .await
    {
        Ok(new_balance) => {
            let text = format!(
                "✅ Balance adjusted!\n\n\
                 User: {}\n\
                 Amount: {}\n\
                 New balance: {}",
                pending.target_telegram_id, pending.amount, new_balance
            );
            let _ = bot
                .send_message(chat_id, text)
                .reply_markup(back_keyboard())
                .await;
        }
        Err(e) => {
            let _ = bot
                .send_message(chat_id, format!("❌ Failed: {}", e))
                .reply_markup(back_keyboard())
                .await;
        }
    }
}

pub async fn handle_balance_cancel_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    PENDING_BALANCE_ADJUSTMENTS.lock().await.remove(&admin_id);

    if let Some(msg) = &cq.message {
        let _ = bot
            .send_message(msg.chat().id, "❌ Balance adjustment cancelled.")
            .reply_markup(back_keyboard())
            .await;
    }
}

pub async fn handle_admin_back_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    clear_admin_state(telegram_id).await;

    if let Some(msg) = &cq.message {
        let _ = bot
            .send_message(msg.chat().id, "🔧 Admin Panel\n\nSelect an action:")
            .reply_markup(admin_menu_keyboard())
            .await;
    }
}

// ─── Text Input Handlers ───

async fn handle_user_lookup_input(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    msg: &Message,
    text: &str,
) {
    let admin_id = msg.from.as_ref().unwrap().id.0 as i64;

    let target_id: i64 = match text.trim().parse() {
        Ok(id) => id,
        Err(_) => {
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForUserLookupId);
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid Telegram ID. Please enter a numeric ID.",
                )
                .await;
            return;
        }
    };

    match self::db::get_user_by_telegram_id(db, target_id).await {
        Ok(Some(user)) => {
            let mut text = format!(
                "🔍 User Info\n\n\
                 ID: {}\n\
                 Username: @{}\n\
                 Telegram ID: {}\n\
                 Balance: {}\n\
                 Joined: {}",
                user.id,
                user.username.as_deref().unwrap_or("N/A"),
                user.telegram_id,
                user.balance,
                user.created_at,
            );

            match self::db::get_user_transactions(db, user.id, 10).await {
                Ok(txns) if !txns.is_empty() => {
                    text.push_str("\n\n📜 Last 10 transactions:\n");
                    for tx in &txns {
                        text.push_str(&format!(
                            "\n  {} | {} | {}",
                            tx.created_at,
                            tx.amount,
                            tx.description.as_deref().unwrap_or("-"),
                        ));
                    }
                }
                Ok(_) => text.push_str("\n\n📜 No transactions found."),
                Err(e) => {
                    eprintln!("Failed to fetch transactions: {}", e);
                    text.push_str("\n\n⚠️ Could not load transactions.");
                }
            }

            let _ = bot
                .send_message(msg.chat.id, text)
                .reply_markup(back_keyboard())
                .await;
        }
        Ok(None) => {
            let _ = bot
                .send_message(msg.chat.id, "⚠️ User not found.")
                .reply_markup(back_keyboard())
                .await;
        }
        Err(e) => {
            eprintln!("User lookup error: {}", e);
            let _ = bot
                .send_message(msg.chat.id, "❌ Database error.")
                .reply_markup(back_keyboard())
                .await;
        }
    }
}

async fn handle_balance_user_id_input(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    msg: &Message,
    text: &str,
) {
    let admin_id = msg.from.as_ref().unwrap().id.0 as i64;

    let target_id: i64 = match text.trim().parse() {
        Ok(id) => id,
        Err(_) => {
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForBalanceUserId);
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid Telegram ID. Please enter a numeric ID.",
                )
                .await;
            return;
        }
    };

    // Fetch user info to show current balance
    let balance_info = match self::db::get_user_by_telegram_id(db, target_id).await {
        Ok(Some(user)) => format!(
            "👤 User: {}\n💰 Current balance: {:.2} USD\n\n",
            user.username.as_deref().unwrap_or("unknown"),
            user.balance
        ),
        Ok(None) => {
            let _ = bot
                .send_message(msg.chat.id, "⚠️ User not found with this Telegram ID.")
                .await;
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForBalanceUserId);
            return;
        }
        Err(_) => String::new(),
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForBalanceAmount {
            target_telegram_id: target_id,
        },
    );

    let _ = bot
        .send_message(
            msg.chat.id,
            format!(
                "{}Enter the amount to adjust.\n\
             Use positive number to add, negative to subtract.\n\
             Example: 5.00 or -2.50\n\n\
             Type /admin to cancel.",
                balance_info
            ),
        )
        .await;
}

async fn handle_balance_amount_input(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    msg: &Message,
    text: &str,
    admin_id: i64,
    target_telegram_id: i64,
) {
    let amount: rust_decimal::Decimal = match text.trim().parse() {
        Ok(a) => a,
        Err(_) => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForBalanceAmount { target_telegram_id },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid amount. Enter a number like 5.00 or -2.50.",
                )
                .await;
            return;
        }
    };

    if amount == rust_decimal::Decimal::ZERO {
        ADMIN_STATE.lock().await.insert(
            admin_id,
            AdminState::WaitingForBalanceAmount { target_telegram_id },
        );
        let _ = bot
            .send_message(msg.chat.id, "⚠️ Amount cannot be zero.")
            .await;
        return;
    }

    // Fetch current balance for confirmation display
    let (current_balance, username) =
        match self::db::get_user_by_telegram_id(db, target_telegram_id).await {
            Ok(Some(user)) => (
                user.balance,
                user.username.unwrap_or_else(|| "unknown".to_string()),
            ),
            _ => (rust_decimal::Decimal::ZERO, "unknown".to_string()),
        };
    let new_balance = current_balance + amount;

    PENDING_BALANCE_ADJUSTMENTS.lock().await.insert(
        admin_id,
        PendingBalanceAdjustment {
            target_telegram_id,
            amount,
        },
    );

    let action = if amount > rust_decimal::Decimal::ZERO {
        "Add"
    } else {
        "Subtract"
    };

    let confirm_keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", buttons::CB_ADMIN_BALANCE_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_BALANCE_CANCEL),
    ]]);

    let _ = bot
        .send_message(
            msg.chat.id,
            format!(
                "⚠️ Confirm balance adjustment:\n\n\
             👤 User: {} ({})\n\
             💰 Current balance: {:.2} USD\n\
             Action: {} {:.2} USD\n\
             💰 New balance: {:.2} USD\n\n\
             Are you sure?",
                username,
                target_telegram_id,
                current_balance,
                action,
                amount.abs(),
                new_balance
            ),
        )
        .reply_markup(confirm_keyboard)
        .await;
}

// ─── Subscription Management Handlers ───

/// List all subscription types with Add/Back buttons
pub async fn handle_subscriptions_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let types = match self::db::get_all_subscription_types(db).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to fetch subscription types: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Failed to load subscription types.")
                .await;
            return;
        }
    };

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    if types.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback("No types yet", "noop")]);
    } else {
        for st in &types {
            rows.push(vec![InlineKeyboardButton::callback(
                format!("📦 {}", st.name),
                format!("{}{}", buttons::CB_ADMIN_SUB_TYPE_PREFIX, st.id),
            )]);
        }
    }

    rows.push(vec![InlineKeyboardButton::callback(
        "➕ Add Type",
        buttons::CB_ADMIN_SUB_TYPE_ADD,
    )]);
    rows.push(vec![InlineKeyboardButton::callback(
        "⬅️ Back",
        buttons::CB_ADMIN_BACK,
    )]);

    let kb = InlineKeyboardMarkup::new(rows);
    let _ = bot
        .send_message(chat_id, "⚙️ Subscription Types\n\nSelect a type to manage:")
        .reply_markup(kb)
        .await;
}

/// Prompt admin to enter a new type name
pub async fn handle_sub_type_add_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };

    ADMIN_STATE
        .lock()
        .await
        .insert(telegram_id, AdminState::WaitingForSubTypeName);

    let _ = bot.send_message(
        msg.chat().id,
        "📝 Enter the name for the new subscription type (e.g. \"simple\", \"vip\").\n\nType /admin to cancel.",
    ).reply_markup(back_keyboard()).await;
}

/// Show a type's subscriptions + Add/Delete type buttons
pub async fn handle_sub_type_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    type_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let type_info = match self::db::get_subscription_type_by_id(db, type_id).await {
        Ok(Some(t)) => t,
        _ => {
            let _ = bot
                .send_message(chat_id, "⚠️ Subscription type not found.")
                .await;
            return;
        }
    };

    let subs = match self::db::get_subscriptions_by_type(db, type_id).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to fetch subscriptions: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Failed to load subscriptions.")
                .await;
            return;
        }
    };

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    if subs.is_empty() {
        rows.push(vec![InlineKeyboardButton::callback(
            "No subscriptions yet",
            "noop",
        )]);
    } else {
        for sub in &subs {
            let label = format!(
                "${:.2} | {}d | {}GB | {}📱",
                sub.price, sub.days, sub.limit_gb, sub.device_limit
            );
            rows.push(vec![InlineKeyboardButton::callback(
                label,
                format!("{}{}", buttons::CB_ADMIN_SUB_DETAIL_PREFIX, sub.id),
            )]);
        }
    }

    rows.push(vec![InlineKeyboardButton::callback(
        "➕ Add Subscription",
        format!("{}{}", buttons::CB_ADMIN_SUB_ADD_PREFIX, type_id),
    )]);
    rows.push(vec![InlineKeyboardButton::callback(
        "🗑 Delete Type",
        format!("{}{}", buttons::CB_ADMIN_SUB_TYPE_DELETE_PREFIX, type_id),
    )]);
    rows.push(vec![InlineKeyboardButton::callback(
        "⬅️ Back to Types",
        buttons::CB_ADMIN_SUBSCRIPTIONS.to_string(),
    )]);

    let kb = InlineKeyboardMarkup::new(rows);
    let text = format!("📦 Type: {}\n\nSubscriptions:", type_info.name);
    let _ = bot.send_message(chat_id, text).reply_markup(kb).await;
}

/// Store pending DeleteType and show confirm
pub async fn handle_sub_type_delete_callback(bot: &Bot, cq: &CallbackQuery, type_id: i32) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };

    PENDING_SUB_ACTIONS
        .lock()
        .await
        .insert(admin_id, PendingSubAction::DeleteType { type_id });

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", buttons::CB_ADMIN_SUB_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_SUB_CANCEL),
    ]]);
    let _ = bot
        .send_message(
            msg.chat().id,
            "⚠️ Delete this subscription type and ALL its subscriptions?\n\nThis cannot be undone.",
        )
        .reply_markup(kb)
        .await;
}

/// Start adding a subscription: ask for price
pub async fn handle_sub_add_callback(bot: &Bot, cq: &CallbackQuery, type_id: i32) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };

    ADMIN_STATE
        .lock()
        .await
        .insert(admin_id, AdminState::WaitingForSubPrice { type_id });

    let _ = bot.send_message(
        msg.chat().id,
        "💵 Enter the price (USD) for the new subscription (e.g. 2.50).\n\nType /admin to cancel.",
    ).reply_markup(back_keyboard()).await;
}

/// Show subscription detail + Edit/Delete/Activate buttons
pub async fn handle_sub_detail_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    sub_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let sub = match self::db::get_subscription_by_id(db, sub_id).await {
        Ok(Some(s)) => s,
        _ => {
            let _ = bot
                .send_message(chat_id, "⚠️ Subscription not found.")
                .await;
            return;
        }
    };

    let text = format!(
        "📋 Subscription Detail\n\n\
         ID: {}\n\
         💵 Price: {:.2} USD\n\
         📅 Days: {}\n\
         📊 Data Limit: {:.1} GB\n\
         📱 Devices: {}",
        sub.id, sub.price, sub.days, sub.limit_gb, sub.device_limit
    );

    let mut rows: Vec<Vec<InlineKeyboardButton>> = vec![vec![
        InlineKeyboardButton::callback(
            "✏️ Edit",
            format!("{}{}", buttons::CB_ADMIN_SUB_EDIT_PREFIX, sub.id),
        ),
        InlineKeyboardButton::callback(
            "🗑 Delete",
            format!("{}{}", buttons::CB_ADMIN_SUB_DELETE_PREFIX, sub.id),
        ),
    ]];

    rows.push(vec![InlineKeyboardButton::callback(
        "⬅️ Back to Type",
        format!(
            "{}{}",
            buttons::CB_ADMIN_SUB_TYPE_PREFIX,
            sub.subscription_type_id
        ),
    )]);

    let kb = InlineKeyboardMarkup::new(rows);
    let _ = bot.send_message(chat_id, text).reply_markup(kb).await;
}

/// Start editing: ask for new price
pub async fn handle_sub_edit_callback(bot: &Bot, cq: &CallbackQuery, sub_id: i32) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };

    ADMIN_STATE
        .lock()
        .await
        .insert(admin_id, AdminState::WaitingForSubEditPrice { sub_id });

    let _ = bot
        .send_message(
            msg.chat().id,
            "💵 Enter the new price (USD) (e.g. 3.50).\n\nType /admin to cancel.",
        )
        .reply_markup(back_keyboard())
        .await;
}

/// Store pending Delete and show confirm
pub async fn handle_sub_delete_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
    sub_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };

    let type_id = match self::db::get_subscription_by_id(db, sub_id).await {
        Ok(Some(s)) => s.subscription_type_id,
        _ => 0,
    };

    PENDING_SUB_ACTIONS
        .lock()
        .await
        .insert(admin_id, PendingSubAction::Delete { sub_id, type_id });

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", buttons::CB_ADMIN_SUB_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_SUB_CANCEL),
    ]]);
    let _ = bot
        .send_message(
            msg.chat().id,
            "⚠️ Delete this subscription?\n\nThis cannot be undone.",
        )
        .reply_markup(kb)
        .await;
}

/// Execute pending subscription action
pub async fn handle_sub_confirm_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let Some(action) = PENDING_SUB_ACTIONS.lock().await.remove(&admin_id) else {
        let _ = bot
            .send_message(chat_id, "⚠️ No pending action found.")
            .await;
        return;
    };

    match action {
        PendingSubAction::Create {
            type_id,
            price,
            days,
            limit_gb,
            device_limit,
        } => match self::db::create_subscription(db, type_id, price, days, limit_gb, device_limit)
            .await
        {
            Ok(id) => {
                let _ = bot
                    .send_message(chat_id, format!("✅ Subscription #{} created!", id))
                    .reply_markup(back_keyboard())
                    .await;
            }
            Err(e) => {
                let _ = bot
                    .send_message(chat_id, format!("❌ Failed: {}", e))
                    .reply_markup(back_keyboard())
                    .await;
            }
        },
        PendingSubAction::Edit {
            sub_id,
            price,
            days,
            limit_gb,
            device_limit,
        } => match self::db::update_subscription(db, sub_id, price, days, limit_gb, device_limit)
            .await
        {
            Ok(()) => {
                let _ = bot
                    .send_message(chat_id, format!("✅ Subscription #{} updated!", sub_id))
                    .reply_markup(back_keyboard())
                    .await;
            }
            Err(e) => {
                let _ = bot
                    .send_message(chat_id, format!("❌ Failed: {}", e))
                    .reply_markup(back_keyboard())
                    .await;
            }
        },
        PendingSubAction::Delete { sub_id, type_id } => {
            match self::db::delete_subscription(db, sub_id).await {
                Ok(()) => {
                    let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                        "⬅️ Back to Type",
                        format!("{}{}", buttons::CB_ADMIN_SUB_TYPE_PREFIX, type_id),
                    )]]);
                    let _ = bot
                        .send_message(chat_id, "✅ Subscription deleted.")
                        .reply_markup(kb)
                        .await;
                }
                Err(e) => {
                    let _ = bot
                        .send_message(chat_id, format!("❌ Failed: {}", e))
                        .reply_markup(back_keyboard())
                        .await;
                }
            }
        }
        PendingSubAction::DeleteType { type_id } => {
            match self::db::delete_subscription_type(db, type_id).await {
                Ok(()) => {
                    let _ = bot
                        .send_message(chat_id, "✅ Subscription type deleted.")
                        .reply_markup(back_keyboard())
                        .await;
                }
                Err(e) => {
                    let _ = bot
                        .send_message(chat_id, format!("❌ Failed: {}", e))
                        .reply_markup(back_keyboard())
                        .await;
                }
            }
        }
    }
}

/// Cancel pending subscription action
pub async fn handle_sub_cancel_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    PENDING_SUB_ACTIONS.lock().await.remove(&admin_id);

    if let Some(msg) = &cq.message {
        let _ = bot
            .send_message(msg.chat().id, "❌ Action cancelled.")
            .reply_markup(back_keyboard())
            .await;
    }
}

// ─── Subscription Text Input Handlers ───

async fn handle_sub_type_name_input(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    msg: &Message,
    text: &str,
    admin_id: i64,
) {
    let name = text.trim();
    if name.is_empty() {
        ADMIN_STATE
            .lock()
            .await
            .insert(admin_id, AdminState::WaitingForSubTypeName);
        let _ = bot
            .send_message(msg.chat.id, "⚠️ Name cannot be empty. Try again.")
            .await;
        return;
    }

    match self::db::create_subscription_type(db, name).await {
        Ok(id) => {
            let _ = bot
                .send_message(
                    msg.chat.id,
                    format!("✅ Type \"{}\" created (ID: {}).", name, id),
                )
                .reply_markup(back_keyboard())
                .await;
        }
        Err(e) => {
            let err = e.to_string();
            if err.contains("unique") || err.contains("duplicate") {
                let _ = bot
                    .send_message(msg.chat.id, "⚠️ A type with this name already exists.")
                    .reply_markup(back_keyboard())
                    .await;
            } else {
                let _ = bot
                    .send_message(msg.chat.id, format!("❌ Failed: {}", e))
                    .reply_markup(back_keyboard())
                    .await;
            }
        }
    }
}

async fn handle_sub_price_input(bot: &Bot, msg: &Message, text: &str, admin_id: i64, type_id: i32) {
    let price: f64 = match text.trim().parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForSubPrice { type_id });
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid price. Enter a positive number like 2.50.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE
        .lock()
        .await
        .insert(admin_id, AdminState::WaitingForSubDays { type_id, price });
    let _ = bot
        .send_message(msg.chat.id, "📅 Enter the number of days (e.g. 30).")
        .await;
}

async fn handle_sub_days_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    type_id: i32,
    price: f64,
) {
    let days: i32 = match text.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => {
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForSubDays { type_id, price });
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid days. Enter a positive integer like 30.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForSubLimitGb {
            type_id,
            price,
            days,
        },
    );
    let _ = bot
        .send_message(msg.chat.id, "📊 Enter the data limit in GB (e.g. 100).")
        .await;
}

async fn handle_sub_limit_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    type_id: i32,
    price: f64,
    days: i32,
) {
    let limit_gb: f64 = match text.trim().parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForSubLimitGb {
                    type_id,
                    price,
                    days,
                },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid data limit. Enter a positive number like 100.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForSubDeviceLimit {
            type_id,
            price,
            days,
            limit_gb,
        },
    );
    let _ = bot
        .send_message(
            msg.chat.id,
            "📱 Enter the device limit (e.g. 2).\n\nHow many devices may use this plan at once.",
        )
        .await;
}

/// Final step of plan creation. Marzban's Happ policy makes this mandatory:
/// a user create carrying `client_type` without a `device_limit` is rejected.
///
/// Every handler in this state machine takes the accumulated draft as loose
/// positional args (its sibling `handle_sub_limit_input` sits at the 7 limit);
/// adding a field pushed these two to 8. Bundling only these into a struct would
/// make them read differently from every other step for no gain, so the lint is
/// silenced rather than worked around — same call as `AdminState`'s above.
#[allow(clippy::too_many_arguments)]
async fn handle_sub_device_limit_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    type_id: i32,
    price: f64,
    days: i32,
    limit_gb: f64,
) {
    let device_limit: i32 = match text.trim().parse() {
        Ok(v) if v >= 1 => v,
        _ => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForSubDeviceLimit {
                    type_id,
                    price,
                    days,
                    limit_gb,
                },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid device limit. Enter a whole number of 1 or more, like 2.",
                )
                .await;
            return;
        }
    };

    // Store pending and ask for confirmation
    PENDING_SUB_ACTIONS.lock().await.insert(
        admin_id,
        PendingSubAction::Create {
            type_id,
            price,
            days,
            limit_gb,
            device_limit,
        },
    );

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", buttons::CB_ADMIN_SUB_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_SUB_CANCEL),
    ]]);

    let _ = bot
        .send_message(
            msg.chat.id,
            format!(
                "⚠️ Create new subscription?\n\n\
             💵 Price: {:.2} USD\n\
             📅 Days: {}\n\
             📊 Data Limit: {:.1} GB\n\
             📱 Devices: {}\n\n\
             Confirm?",
                price, days, limit_gb, device_limit
            ),
        )
        .reply_markup(kb)
        .await;
}

async fn handle_sub_edit_price_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    sub_id: i32,
) {
    let price: f64 = match text.trim().parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            ADMIN_STATE
                .lock()
                .await
                .insert(admin_id, AdminState::WaitingForSubEditPrice { sub_id });
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid price. Enter a positive number like 3.50.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForSubEditDays { sub_id, price },
    );
    let _ = bot
        .send_message(msg.chat.id, "📅 Enter the new number of days (e.g. 30).")
        .await;
}

async fn handle_sub_edit_days_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    sub_id: i32,
    price: f64,
) {
    let days: i32 = match text.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForSubEditDays { sub_id, price },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid days. Enter a positive integer like 30.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForSubEditLimitGb {
            sub_id,
            price,
            days,
        },
    );
    let _ = bot
        .send_message(msg.chat.id, "📊 Enter the new data limit in GB (e.g. 100).")
        .await;
}

async fn handle_sub_edit_limit_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    sub_id: i32,
    price: f64,
    days: i32,
) {
    let limit_gb: f64 = match text.trim().parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForSubEditLimitGb {
                    sub_id,
                    price,
                    days,
                },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid data limit. Enter a positive number like 100.",
                )
                .await;
            return;
        }
    };

    ADMIN_STATE.lock().await.insert(
        admin_id,
        AdminState::WaitingForSubEditDeviceLimit {
            sub_id,
            price,
            days,
            limit_gb,
        },
    );
    let _ = bot
        .send_message(msg.chat.id, "📱 Enter the new device limit (e.g. 2).")
        .await;
}

/// Edit-flow counterpart of `handle_sub_device_limit_input`; same argument-count
/// reasoning applies.
#[allow(clippy::too_many_arguments)]
async fn handle_sub_edit_device_limit_input(
    bot: &Bot,
    msg: &Message,
    text: &str,
    admin_id: i64,
    sub_id: i32,
    price: f64,
    days: i32,
    limit_gb: f64,
) {
    let device_limit: i32 = match text.trim().parse() {
        Ok(v) if v >= 1 => v,
        _ => {
            ADMIN_STATE.lock().await.insert(
                admin_id,
                AdminState::WaitingForSubEditDeviceLimit {
                    sub_id,
                    price,
                    days,
                    limit_gb,
                },
            );
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "⚠️ Invalid device limit. Enter a whole number of 1 or more, like 2.",
                )
                .await;
            return;
        }
    };

    PENDING_SUB_ACTIONS.lock().await.insert(
        admin_id,
        PendingSubAction::Edit {
            sub_id,
            price,
            days,
            limit_gb,
            device_limit,
        },
    );

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Confirm", buttons::CB_ADMIN_SUB_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_SUB_CANCEL),
    ]]);

    let _ = bot
        .send_message(
            msg.chat.id,
            format!(
                "⚠️ Update subscription #{}?\n\n\
             💵 Price: {:.2} USD\n\
             📅 Days: {}\n\
             📊 Data Limit: {:.1} GB\n\
             📱 Devices: {}\n\n\
             Confirm?",
                sub_id, price, days, limit_gb, device_limit
            ),
        )
        .reply_markup(kb)
        .await;
}
