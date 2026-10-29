mod admin;
mod auth;
mod commands;
mod config;
mod db;
mod handlers;
mod i18n;
mod note;
mod payments;
mod rate_limit;
mod utils;

use commands::{parse_command, Command};
use config::get_config;
use db::{
    cleanup_expired_payments, ensure_marzban_accounts_table, ensure_subscription_tables,
    establish_connection, get_user_language, save_user_from_telegram,
};
use handlers::{balance, buttons, buy, extend, help, lang, start, subscriptions, support};
use i18n::{match_button, tf, ButtonAction, Lang};
use rate_limit::check_rate_limit;

use teloxide::dispatching::Dispatcher;
use teloxide::dptree;
use teloxide::net::default_reqwest_settings;
use teloxide::prelude::*;
use teloxide::types::{BotCommand, CallbackQuery, Message, Update};

/// Middleware filter to check if the user is a bot
/// Returns true if the user is NOT a bot (allowed to proceed)
fn is_not_bot_message(msg: Message) -> bool {
    msg.from.as_ref().map(|user| !user.is_bot).unwrap_or(true)
}

/// Middleware filter to check if the callback query is from a bot
/// Returns true if the user is NOT a bot (allowed to proceed)
fn is_not_bot_callback(cq: CallbackQuery) -> bool {
    !cq.from.is_bot
}

/// How often the long-stop sweep of `pending_payments` runs.
const PAYMENT_REAP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Long-stop retirement of `pending_payments`.
///
/// The payment poller owns normal deletion — rows go the moment the provider reports a
/// terminal status, or when it force-expires one whose provider positively said nothing
/// arrived. What reaches this sweep is therefore the *doubtful* set: invoices the poller
/// could never make sense of (a mismatched order_id, an unrecognised status, a lookup
/// that kept failing). Those are the ones most likely to be a real payment, so
/// `cleanup_expired_payments` archives each into `reconcile_candidates` in the same
/// statement that deletes it — this is not a bare cleanup.
async fn reap_expired_payments(db: sea_orm::DatabaseConnection) {
    loop {
        match cleanup_expired_payments(&db).await {
            Ok(deleted) if deleted > 0 => {
                println!("🧹 Reaped {} expired pending payment(s)", deleted);
            }
            Ok(_) => {}
            Err(e) => eprintln!("⚠️ Failed to reap expired payments: {}", e),
        }
        tokio::time::sleep(PAYMENT_REAP_INTERVAL).await;
    }
}

#[tokio::main]
async fn main() {
    // Initialize config once at startup
    let config = get_config();
    let bot = if let Some(proxy_url) = &config.teloxide_proxy {
        let proxy = reqwest::Proxy::all(proxy_url).expect("Invalid TELOXIDE_PROXY URL");
        let client = default_reqwest_settings()
            .proxy(proxy)
            .build()
            .expect("Failed to build reqwest client with proxy");
        Bot::with_client(&config.telegram_bot_api, client)
    } else {
        Bot::new(&config.telegram_bot_api)
    };

    let db = match establish_connection(config).await {
        Ok(conn) => {
            println!("✅ Connected to database");
            conn
        }
        Err(e) => {
            eprintln!("❌ Database connection failed: {}", e);
            return;
        }
    };

    // Ensure subscription tables exist
    if let Err(e) = ensure_subscription_tables(&db).await {
        eprintln!("⚠️ Failed to create subscription tables: {}", e);
    }
    if let Err(e) = ensure_marzban_accounts_table(&db).await {
        eprintln!("⚠️ Failed to create marzban_accounts table: {}", e);
    }

    // Set bot menu commands (visible in the ☰ menu button next to chat input)
    let commands = vec![
        BotCommand::new("start", "Boty başlatmak"),
        BotCommand::new("help", "Kömek"),
        BotCommand::new("support", "Goldaw"),
    ];
    if let Err(e) = bot.set_my_commands(commands).await {
        eprintln!("⚠️ Failed to set bot commands: {}", e);
    }

    // Watch every open invoice and credit it automatically. This is the only path by
    // which a balance grows — the manual /check command and button were removed.
    //
    // Supervised, because the failure mode of dropping this JoinHandle is silent and
    // expensive: `run_payment_poller` loops forever, so the only way it can finish is a
    // panic, and a panicked task leaves the process perfectly healthy — systemd reports
    // `active`, the deploy health check passes, messages are still answered — while
    // nobody is ever credited again.
    //
    // Respawn rather than exit the process. The unit is `Restart=always` / `RestartSec=5`
    // (verified on the host, 2026-08-30 — the file lives at
    // /etc/systemd/system/telegram-bot.service and is not in this repo), so exiting WOULD
    // come back. It is still the worse option: the poller's panic surface is per-invoice
    // and deterministic — `i18n::t` panics on a missing key — so one poisoned row would
    // panic-exit-restart-panic every 5s and take the whole bot down with it each time,
    // and an exit could land inside `buy.rs`'s deduct→Marzban→refund window and lose a
    // refund. Respawning isolates the failure to the poller, keeps the rest of the bot
    // serving, and recovers fully from a transient panic — the poller rebuilds its whole
    // worklist from Postgres on every tick, so a fresh task loses nothing.
    tokio::spawn({
        let (bot, db) = (bot.clone(), db.clone());
        async move {
            /// Backoff after a panic, so a permanently poisoned invoice cannot spin.
            const RESPAWN_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
            loop {
                let handle = tokio::spawn(payments::poller::run_payment_poller(
                    bot.clone(),
                    db.clone(),
                ));
                let outcome = handle.await;
                eprintln!(
                    "CRITICAL: payment poller stopped ({:?}) — no balance is being \
                     credited; respawning in {}s",
                    outcome,
                    RESPAWN_DELAY.as_secs()
                );
                tokio::time::sleep(RESPAWN_DELAY).await;
            }
        }
    });

    // Long-stop cleanup for rows the poller can never resolve (hourly)
    tokio::spawn(reap_expired_payments(db.clone()));

    println!("🤖 Bot is running...");

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                .filter(is_not_bot_message)
                .endpoint(handle_message),
        )
        .branch(
            Update::filter_callback_query()
                .filter(is_not_bot_callback)
                .endpoint(handle_callback),
        );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![db])
        .build()
        .dispatch()
        .await;
}

async fn handle_message(
    bot: Bot,
    msg: Message,
    db: sea_orm::DatabaseConnection,
) -> Result<(), teloxide::RequestError> {
    // Rate limit check
    if let Some(from) = &msg.from {
        if let Err(wait_secs) = check_rate_limit(from.id.0 as i64).await {
            let lang = get_user_language(&db, from.id.0 as i64)
                .await
                .unwrap_or_default();
            let text = tf("rate_limit", lang, &[("seconds", &wait_secs.to_string())]);
            let _ = bot.send_message(msg.chat.id, text).await;
            return Ok(());
        }
        // Save user to DB
        let _ =
            save_user_from_telegram(&db, from.id.0 as i64, from.username.as_deref(), from.is_bot)
                .await;
    }

    // Handle replies in the support group
    if let Some(support_chat_id) = get_config().support_chat_id {
        if msg.chat.id.0 == support_chat_id && msg.reply_to_message().is_some() {
            support::handle_support_reply(&bot, &db, &msg).await;
            return Ok(());
        }
    }

    // Get user language
    let lang = match &msg.from {
        Some(from) => get_user_language(&db, from.id.0 as i64)
            .await
            .unwrap_or_default(),
        None => Lang::default(),
    };

    // Support attachments must be handled BEFORE the `msg.text()` gate below, which
    // drops every non-text update on the floor — that gate is why a user in support mode
    // could send a screenshot and have absolutely nothing happen. A photo's words live in
    // `caption`, not `text`, so a captioned photo reaches here with `text() == None` too.
    if let Some(user) = &msg.from {
        if msg.text().is_none() && support::should_accept_attachment(&msg, user.id.0 as i64).await {
            support::handle_support_attachment(&bot, &msg, lang).await;
            return Ok(());
        }
    }

    let Some(text) = msg.text() else {
        return Ok(());
    };

    // First check if it's a keyboard button press (matches all languages)
    if let Some(action) = match_button(text) {
        // Cancel support/topup modes if active
        if let Some(user) = &msg.from {
            let was_waiting = support::is_waiting_for_support(user.id.0 as i64).await;
            // Unconditional, and deliberately outside the guard: it also clears the album
            // key, and a user who has one is BY CONSTRUCTION no longer in support mode —
            // the attachment path clears the waiting flag in the same call that records
            // the album. Behind the guard this never ran for any entry that existed, so
            // the map only ever grew.
            //
            // Before the send, not after: the flag is keyed by USER id while teloxide
            // distributes by CHAT id, so leaving it set across a network round-trip lets
            // the same user's message in another chat still read as "in support mode".
            support::cancel_support(user.id.0 as i64).await;
            balance::cancel_topup_input(user.id.0 as i64).await;
            buy::cancel_username_input(user.id.0 as i64).await;
            if was_waiting {
                let _ = bot
                    .send_message(msg.chat.id, i18n::t("support_cancelled", lang))
                    .await;
            }
        }
        match action {
            ButtonAction::Balance => {
                balance::handle_balance_message(&bot, &db, &msg, lang).await;
            }
            ButtonAction::Buy => {
                buy::handle_buy_subscription_message(&bot, &db, &msg, lang).await;
            }
            ButtonAction::Subscriptions => {
                subscriptions::handle_my_subscriptions_message(&bot, &msg, lang).await;
            }
            ButtonAction::Help => {
                help::handle_help(&bot, &db, &msg, lang).await;
            }
            ButtonAction::Support => {
                support::handle_support_button(&bot, &msg, lang).await;
            }
            ButtonAction::Lang => {
                lang::handle_lang_button(&bot, &msg, lang).await;
            }
        }
        return Ok(());
    }

    // Check if it's a slash command — if so, cancel support/topup modes
    let cmd = parse_command(text);
    if cmd.is_some() {
        if let Some(user) = &msg.from {
            let was_waiting = support::is_waiting_for_support(user.id.0 as i64).await;
            // Unconditional, and deliberately outside the guard: it also clears the album
            // key, and a user who has one is BY CONSTRUCTION no longer in support mode —
            // the attachment path clears the waiting flag in the same call that records
            // the album. Behind the guard this never ran for any entry that existed, so
            // the map only ever grew.
            //
            // Before the send, not after: the flag is keyed by USER id while teloxide
            // distributes by CHAT id, so leaving it set across a network round-trip lets
            // the same user's message in another chat still read as "in support mode".
            support::cancel_support(user.id.0 as i64).await;
            balance::cancel_topup_input(user.id.0 as i64).await;
            buy::cancel_username_input(user.id.0 as i64).await;
            if was_waiting {
                let _ = bot
                    .send_message(msg.chat.id, i18n::t("support_cancelled", lang))
                    .await;
            }
        }
    }

    // Naming a subscription they are part-way through buying.
    if let Some(user) = &msg.from {
        if let Some(plan_id) = buy::pending_username_plan(user.id.0 as i64).await {
            buy::handle_username_input(&bot, &db, &msg, lang, plan_id).await;
            return Ok(());
        }
    }

    // Check if user is waiting to type a custom top-up amount
    if let Some(user) = &msg.from {
        if balance::is_waiting_for_topup(user.id.0 as i64).await {
            balance::handle_topup_text_input(&bot, &db, &msg, lang).await;
            return Ok(());
        }
    }

    // Check if user is waiting to type a support message (plain text goes to support)
    if let Some(user) = &msg.from {
        if support::is_waiting_for_support(user.id.0 as i64).await {
            support::handle_support_message(&bot, &msg, lang).await;
            return Ok(());
        }
    }

    // Clear admin state for all commands except /admin, and plain text (input flows)
    if cmd.is_some() && !matches!(cmd, Some(Command::Admin)) {
        if let Some(u) = msg.from.as_ref() {
            admin::clear_admin_state(u.id.0 as i64).await;
        }
    }

    match cmd {
        Some(Command::Start) => start::handle_start(&bot, &db, &msg).await,
        Some(Command::Help) => help::handle_help(&bot, &db, &msg, lang).await,
        Some(Command::Support) => support::handle_support_button(&bot, &msg, lang).await,
        Some(Command::Admin) => {
            if let Some(u) = msg.from.as_ref() {
                if admin::is_admin(u.id.0 as i64) {
                    admin::handle_admin_command(&bot, &msg).await;
                }
            }
        }
        Some(Command::Unknown(_)) | None => {
            // Routes to the admin input state machine; no-op when no state is active.
            admin::handle_admin_text_input(&bot, &db, &msg).await;
        }
    }

    Ok(())
}

async fn handle_callback(
    bot: Bot,
    cq: CallbackQuery,
    db: sea_orm::DatabaseConnection,
) -> Result<(), teloxide::RequestError> {
    // Rate limit check
    if let Err(wait_secs) = check_rate_limit(cq.from.id.0 as i64).await {
        let _ = bot
            .answer_callback_query(cq.id.clone())
            .text(format!("⏳ Too many requests. Wait {} seconds.", wait_secs))
            .show_alert(true)
            .await;
        return Ok(());
    }

    let Some(data) = &cq.data else { return Ok(()) };

    // Get user language for callbacks
    let user_lang = get_user_language(&db, cq.from.id.0 as i64)
        .await
        .unwrap_or_default();
    let is_admin = admin::is_admin(cq.from.id.0 as i64);

    match data.as_str() {
        buttons::CB_HELP => help::handle_help_callback(&bot, &db, &cq, user_lang).await,
        buttons::CB_HELP_PRICING => {
            help::handle_help_section_callback(&bot, &db, &cq, user_lang, "pricing").await
        }
        buttons::CB_HELP_TOPUP => {
            help::handle_help_section_callback(&bot, &db, &cq, user_lang, "topup").await
        }
        buttons::CB_HELP_HOWTOBUY => {
            help::handle_help_section_callback(&bot, &db, &cq, user_lang, "howtobuy").await
        }
        buttons::CB_HELP_COMMANDS => {
            help::handle_help_section_callback(&bot, &db, &cq, user_lang, "commands").await
        }
        buttons::CB_HELP_SUPPORT => {
            help::handle_help_section_callback(&bot, &db, &cq, user_lang, "support").await
        }
        buttons::CB_BALANCE => {
            balance::handle_show_balance_callback(&bot, &db, &cq, user_lang).await
        }
        buttons::CB_TOP_UP => balance::handle_balance_callback(&bot, &db, &cq, user_lang).await,
        buttons::CB_TOPUP_5 => {
            balance::handle_topup_amount_callback(&bot, &db, &cq, user_lang, 5.0).await
        }
        buttons::CB_TOPUP_10 => {
            balance::handle_topup_amount_callback(&bot, &db, &cq, user_lang, 10.0).await
        }
        buttons::CB_TOPUP_15 => {
            balance::handle_topup_amount_callback(&bot, &db, &cq, user_lang, 15.0).await
        }
        buttons::CB_TOPUP_CUSTOM => {
            balance::handle_topup_custom_callback(&bot, &cq, user_lang).await
        }
        buttons::CB_BUY_SUBSCRIPTION => {
            buy::handle_buy_subscription_callback(&bot, &cq, &db, user_lang).await
        }
        buttons::CB_MY_SUBSCRIPTIONS => {
            subscriptions::handle_my_subscriptions_callback(&bot, &cq, user_lang).await
        }
        buttons::CB_MAIN_MENU => {
            let _ = bot.answer_callback_query(cq.id.clone()).await;
            if let Some(msg) = &cq.message {
                start::send_main_menu(&bot, msg.chat().id, user_lang).await;
            }
        }
        // Language selection callbacks
        buttons::CB_LANG_TK => lang::handle_lang_callback(&bot, &db, &cq, Lang::Tk).await,
        buttons::CB_LANG_RU => lang::handle_lang_callback(&bot, &db, &cq, Lang::Ru).await,
        buttons::CB_LANG_EN => lang::handle_lang_callback(&bot, &db, &cq, Lang::En).await,
        // Admin callbacks
        admin::buttons::CB_BROADCAST_CONFIRM if is_admin => {
            admin::handle_broadcast_confirm_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_BROADCAST_CANCEL if is_admin => {
            admin::handle_broadcast_cancel_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_STATS if is_admin => {
            admin::handle_stats_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_BROADCAST if is_admin => {
            admin::handle_broadcast_start_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_USER_LOOKUP if is_admin => {
            admin::handle_user_lookup_start_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_BALANCE_MGMT if is_admin => {
            admin::handle_balance_mgmt_start_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_BACK if is_admin => {
            admin::handle_admin_back_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_BALANCE_CONFIRM if is_admin => {
            admin::handle_balance_confirm_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_BALANCE_CANCEL if is_admin => {
            admin::handle_balance_cancel_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_USERS_LIST if is_admin => {
            admin::handle_users_list_callback(&bot, &db, &cq, 0).await
        }
        admin::buttons::CB_ADMIN_TXNS_LIST if is_admin => {
            admin::handle_txns_list_callback(&bot, &db, &cq, 0).await
        }
        // Subscription management
        admin::buttons::CB_ADMIN_SUBSCRIPTIONS if is_admin => {
            admin::handle_subscriptions_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_SUB_TYPE_ADD if is_admin => {
            admin::handle_sub_type_add_callback(&bot, &cq).await
        }
        admin::buttons::CB_ADMIN_SUB_CONFIRM if is_admin => {
            admin::handle_sub_confirm_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_SUB_CANCEL if is_admin => {
            admin::handle_sub_cancel_callback(&bot, &cq).await
        }
        // Historical payment reconciliation (dry run, then confirm)
        admin::buttons::CB_ADMIN_RECONCILE if is_admin => {
            admin::handle_reconcile_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_RECONCILE_CONFIRM if is_admin => {
            admin::handle_reconcile_confirm_callback(&bot, &db, &cq).await
        }
        admin::buttons::CB_ADMIN_RECONCILE_CANCEL if is_admin => {
            admin::handle_reconcile_cancel_callback(&bot, &cq).await
        }
        other => {
            // User prefix-based callbacks (available to all users).
            // NOTE: longer extend prefixes ("xsp:"/"xsc:") are checked before "xs:".
            // Every extend handler re-verifies account ownership — callback data is
            // user-supplied and must never be trusted for authorization.
            if let Some(rest) = other.strip_prefix(buttons::CB_BUY_NAME_USE_PREFIX) {
                // "bns:{plan}:{username}" — accept the suggested name after a collision.
                if let Some((plan, name)) = rest.split_once(':') {
                    if let Ok(plan_id) = plan.parse::<i32>() {
                        buy::handle_buy_name_use_callback(&bot, &cq, &db, user_lang, plan_id, name)
                            .await;
                    }
                }
            } else if let Some(id_str) = other.strip_prefix(buttons::CB_BUY_NAME_AUTO_PREFIX) {
                // "bna:{plan}" — generate a name instead of typing one.
                if let Ok(plan_id) = id_str.parse::<i32>() {
                    buy::handle_buy_name_auto_callback(&bot, &cq, &db, user_lang, plan_id).await;
                }
            } else if let Some(id_str) = other.strip_prefix(buttons::CB_BUY_SUB_PREFIX) {
                if let Ok(sub_id) = id_str.parse::<i32>() {
                    buy::handle_buy_sub_callback(&bot, &cq, &db, user_lang, sub_id).await;
                }
            } else if let Some(rest) = other.strip_prefix(buttons::CB_EXT_PLAN_PREFIX) {
                if let Some((username, plan)) = rest.rsplit_once(':') {
                    if let Ok(plan_id) = plan.parse::<i32>() {
                        extend::handle_extend_plan_callback(
                            &bot, &db, &cq, user_lang, username, plan_id,
                        )
                        .await;
                    }
                }
            } else if let Some(rest) = other.strip_prefix(buttons::CB_EXT_CONFIRM_PREFIX) {
                if let Some((username, plan)) = rest.rsplit_once(':') {
                    if let Ok(plan_id) = plan.parse::<i32>() {
                        extend::handle_extend_confirm_callback(
                            &bot, &db, &cq, user_lang, username, plan_id,
                        )
                        .await;
                    }
                }
            } else if let Some(username) = other.strip_prefix(buttons::CB_EXT_PICK_PREFIX) {
                extend::handle_extend_pick_callback(&bot, &db, &cq, user_lang, username).await;
            }
            // Admin prefix-based callbacks (owner-only)
            else if is_admin {
                if let Some(page_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_USERS_PAGE_PREFIX)
                {
                    if let Ok(page) = page_str.parse::<i64>() {
                        admin::handle_users_list_callback(&bot, &db, &cq, page).await;
                    }
                } else if let Some(page_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_TXNS_PAGE_PREFIX)
                {
                    if let Ok(page) = page_str.parse::<i64>() {
                        admin::handle_txns_list_callback(&bot, &db, &cq, page).await;
                    }
                } else if let Some(rest) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_USER_DETAIL_PREFIX)
                {
                    if let Some((tid_str, page_str)) = rest.split_once(':') {
                        if let (Ok(tid), Ok(page)) =
                            (tid_str.parse::<i64>(), page_str.parse::<i64>())
                        {
                            admin::handle_user_detail_callback(&bot, &db, &cq, tid, page).await;
                        }
                    }
                } else if let Some(rest) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_USER_TXN_DETAIL_PREFIX)
                {
                    let parts: Vec<&str> = rest.splitn(3, ':').collect();
                    if parts.len() == 3 {
                        if let (Ok(txn_id), Ok(tid), Ok(upage)) = (
                            parts[0].parse::<i32>(),
                            parts[1].parse::<i64>(),
                            parts[2].parse::<i64>(),
                        ) {
                            admin::handle_user_txn_detail_callback(
                                &bot, &db, &cq, txn_id, tid, upage,
                            )
                            .await;
                        }
                    }
                } else if let Some(rest) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_USER_TXN_PAGE_PREFIX)
                {
                    let parts: Vec<&str> = rest.splitn(3, ':').collect();
                    if parts.len() == 3 {
                        if let (Ok(tid), Ok(txn_page), Ok(upage)) = (
                            parts[0].parse::<i64>(),
                            parts[1].parse::<i64>(),
                            parts[2].parse::<i64>(),
                        ) {
                            admin::handle_user_txn_page_callback(
                                &bot, &db, &cq, tid, txn_page, upage,
                            )
                            .await;
                        }
                    }
                } else if let Some(rest) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_TXN_DETAIL_PREFIX)
                {
                    if let Some((id_str, page_str)) = rest.split_once(':') {
                        if let (Ok(id), Ok(page)) = (id_str.parse::<i32>(), page_str.parse::<i64>())
                        {
                            admin::handle_txn_detail_callback(&bot, &db, &cq, id, page).await;
                        }
                    }
                }
                // Subscription prefix-based callbacks
                // NOTE: longer prefixes MUST be checked before shorter ones to avoid collisions
                // e.g. "astd:" before "ast:", "asdel:" before "asd:"
                else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_TYPE_DELETE_PREFIX)
                {
                    if let Ok(type_id) = id_str.parse::<i32>() {
                        admin::handle_sub_type_delete_callback(&bot, &cq, type_id).await;
                    }
                } else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_TYPE_PREFIX)
                {
                    if let Ok(type_id) = id_str.parse::<i32>() {
                        admin::handle_sub_type_callback(&bot, &db, &cq, type_id).await;
                    }
                } else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_ADD_PREFIX)
                {
                    if let Ok(type_id) = id_str.parse::<i32>() {
                        admin::handle_sub_add_callback(&bot, &cq, type_id).await;
                    }
                } else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_DELETE_PREFIX)
                {
                    if let Ok(sub_id) = id_str.parse::<i32>() {
                        admin::handle_sub_delete_callback(&bot, &db, &cq, sub_id).await;
                    }
                } else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_DETAIL_PREFIX)
                {
                    if let Ok(sub_id) = id_str.parse::<i32>() {
                        admin::handle_sub_detail_callback(&bot, &db, &cq, sub_id).await;
                    }
                } else if let Some(id_str) =
                    other.strip_prefix(admin::buttons::CB_ADMIN_SUB_EDIT_PREFIX)
                {
                    if let Ok(sub_id) = id_str.parse::<i32>() {
                        admin::handle_sub_edit_callback(&bot, &cq, sub_id).await;
                    }
                }
            }
        }
    }

    Ok(())
}
