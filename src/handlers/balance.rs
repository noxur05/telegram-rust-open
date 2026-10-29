use sea_orm::DatabaseConnection;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message};
use tokio::sync::Mutex;

use super::start::main_keyboard;
use crate::auth::nowpayments::NowPaymentsClient;
use crate::config::get_config;
use crate::db::{get_user_balance, store_pending_payment};
use crate::i18n::{t, tf, Lang};
use crate::utils::escape_markdown_v2;

/// Set of user IDs currently waiting to type a custom top-up amount.
static WAITING_FOR_TOPUP_AMOUNT: LazyLock<Mutex<HashSet<i64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub async fn is_waiting_for_topup(user_id: i64) -> bool {
    WAITING_FOR_TOPUP_AMOUNT.lock().await.contains(&user_id)
}

pub async fn cancel_topup_input(user_id: i64) {
    WAITING_FOR_TOPUP_AMOUNT.lock().await.remove(&user_id);
}

/// Inline keyboard for selecting the top-up amount.
fn topup_amount_keyboard(lang: Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("$5", super::buttons::CB_TOPUP_5),
            InlineKeyboardButton::callback("$10", super::buttons::CB_TOPUP_10),
            InlineKeyboardButton::callback("$15", super::buttons::CB_TOPUP_15),
        ],
        vec![InlineKeyboardButton::callback(
            t("btn_topup_custom", lang),
            super::buttons::CB_TOPUP_CUSTOM,
        )],
    ])
}

/// Core balance/top-up logic - creates a NowPayments payment.
async fn create_balance_payment(
    bot: &Bot,
    db: &DatabaseConnection,
    chat_id: ChatId,
    telegram_user_id: i64,
    lang: Lang,
    amount: f64,
) {
    let cfg = get_config();
    let nowpayments = NowPaymentsClient::new_from_config(cfg);

    let price_currency = &cfg.payment_price_currency;
    let pay_currency = &cfg.payment_pay_currency;
    let description = &cfg.payment_description;

    match nowpayments
        .create_payment(
            amount,
            price_currency,
            pay_currency,
            telegram_user_id,
            description,
        )
        .await
    {
        Ok(json) => {
            let payment = parse_payment_response(&json, price_currency, pay_currency, amount);

            // Store payment mapping in DB
            if !payment.id.is_empty() {
                if let Err(e) = store_pending_payment(db, &payment.id, telegram_user_id).await {
                    eprintln!("Failed to store pending payment: {}", e);
                }
            }

            // No action button: the payment poller watches this invoice and credits it
            // automatically, so there is nothing for the user to press.
            let text = format_payment_message(&payment, lang);

            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
        Err(e) => {
            eprintln!("Payment creation failed: {}", e);
            let text = t("payment_error", lang);

            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(main_keyboard(lang))
                .await;
        }
    }
}

fn top_up_keyboard(lang: Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        t("btn_top_up_inline", lang),
        super::buttons::CB_TOP_UP,
    )]])
}

/// Show the user's current balance with an inline "Top Up" button.
async fn show_balance(
    bot: &Bot,
    db: &DatabaseConnection,
    chat_id: ChatId,
    telegram_user_id: i64,
    lang: Lang,
) {
    match get_user_balance(db, telegram_user_id).await {
        Ok(Some(user)) => {
            let text = tf(
                "balance_info",
                lang,
                &[("balance", &escape_markdown_v2(&user.balance.to_string()))],
            );
            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(top_up_keyboard(lang))
                .await;
        }
        Ok(None) => {
            let text = tf("balance_info", lang, &[("balance", "0")]);
            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(top_up_keyboard(lang))
                .await;
        }
        Err(_) => {
            let text = t("balance_error", lang);
            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(main_keyboard(lang))
                .await;
        }
    }
}

/// Handle balance button from keyboard (message) — shows balance.
pub async fn handle_balance_message(bot: &Bot, db: &DatabaseConnection, msg: &Message, lang: Lang) {
    let Some(user) = &msg.from else { return };
    let telegram_user_id = user.id.0 as i64;
    show_balance(bot, db, msg.chat.id, telegram_user_id, lang).await;
}

/// Handle balance callback — shows balance (from inline button).
pub async fn handle_show_balance_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_user_id = cq.from.id.0 as i64;
    if let Some(msg) = &cq.message {
        show_balance(bot, db, msg.chat().id, telegram_user_id, lang).await;
    }
}

/// Handle top-up callback — show amount selection keyboard instead of creating payment directly.
pub async fn handle_balance_callback(
    bot: &Bot,
    _db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    if let Some(msg) = &cq.message {
        let text = t("topup_select_amount", lang);
        let _ = bot
            .send_message(msg.chat().id, text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(topup_amount_keyboard(lang))
            .await;
    }
}

/// Handle preset amount callback ($5, $10, $15).
pub async fn handle_topup_amount_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
    amount: f64,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let telegram_user_id = cq.from.id.0 as i64;
    create_balance_payment(bot, db, cq.from.id.into(), telegram_user_id, lang, amount).await;
}

/// Handle custom amount callback — prompt user to type amount.
pub async fn handle_topup_custom_callback(bot: &Bot, cq: &CallbackQuery, lang: Lang) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let telegram_user_id = cq.from.id.0 as i64;
    // Only one input mode at a time — see `handlers::clear_input_modes`.
    super::clear_input_modes(telegram_user_id).await;
    WAITING_FOR_TOPUP_AMOUNT
        .lock()
        .await
        .insert(telegram_user_id);

    if let Some(msg) = &cq.message {
        let text = t("topup_enter_amount", lang);
        let _ = bot
            .send_message(msg.chat().id, text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .await;
    }
}

/// Handle text input when user is typing a custom top-up amount.
pub async fn handle_topup_text_input(
    bot: &Bot,
    db: &DatabaseConnection,
    msg: &Message,
    lang: Lang,
) {
    let Some(user) = &msg.from else { return };
    let telegram_user_id = user.id.0 as i64;

    // Remove from waiting set
    WAITING_FOR_TOPUP_AMOUNT
        .lock()
        .await
        .remove(&telegram_user_id);

    let Some(text) = msg.text() else { return };

    let amount: f64 = match text.trim().parse::<f64>() {
        Ok(v) if (1.0..=100.0).contains(&v) => v,
        _ => {
            let err = t("topup_invalid_amount", lang);
            let _ = bot
                .send_message(msg.chat.id, err)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
            return;
        }
    };

    create_balance_payment(bot, db, msg.chat.id, telegram_user_id, lang, amount).await;
}

struct PaymentInfo {
    id: String,
    address: String,
    amount: String,
    currency: String,
    price_amount: String,
    price_currency: String,
}

fn parse_payment_response(
    json: &Value,
    default_price_currency: &str,
    default_pay_currency: &str,
    default_price: f64,
) -> PaymentInfo {
    let get_str = |key: &str| -> String {
        json.get(key)
            .and_then(|v| {
                v.as_str()
                    .map(String::from)
                    .or_else(|| v.as_f64().map(|n| n.to_string()))
            })
            .unwrap_or_default()
    };

    PaymentInfo {
        id: get_str("payment_id"),
        address: get_str("pay_address"),
        amount: get_str("pay_amount"),
        currency: json
            .get("pay_currency")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| default_pay_currency.into()),
        price_amount: json
            .get("price_amount")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| default_price.to_string()),
        price_currency: json
            .get("price_currency")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| default_price_currency.into()),
    }
}

fn format_payment_message(p: &PaymentInfo, lang: Lang) -> String {
    tf(
        "payment_created",
        lang,
        &[
            ("price_amount", &escape_markdown_v2(&p.price_amount)),
            ("price_currency", &escape_markdown_v2(&p.price_currency)),
            ("pay_amount", &p.amount),
            ("pay_currency", &escape_markdown_v2(&p.currency)),
            ("address", &p.address),
            ("payment_id", &p.id),
        ],
    )
}
