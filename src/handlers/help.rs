use sea_orm::DatabaseConnection;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message};

use super::buttons;
use crate::db::get_all_subscriptions;
use crate::i18n::{t, tf, Lang};
use crate::utils::escape_markdown_v2;

/// Build the inline keyboard for the help menu (5 section buttons).
fn help_menu_keyboard(lang: Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback(t("btn_help_pricing", lang), buttons::CB_HELP_PRICING),
            InlineKeyboardButton::callback(t("btn_help_topup", lang), buttons::CB_HELP_TOPUP),
        ],
        vec![
            InlineKeyboardButton::callback(t("btn_help_howtobuy", lang), buttons::CB_HELP_HOWTOBUY),
            InlineKeyboardButton::callback(t("btn_help_commands", lang), buttons::CB_HELP_COMMANDS),
        ],
        vec![InlineKeyboardButton::callback(
            t("btn_help_support", lang),
            buttons::CB_HELP_SUPPORT,
        )],
    ])
}

/// Build a "Back to Help" inline keyboard.
fn back_to_help_keyboard(lang: Lang) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        t("btn_back_help", lang),
        buttons::CB_HELP,
    )]])
}

/// Get the text for a help section, injecting config variables where needed.
async fn get_section_text(db: &DatabaseConnection, section: &str, lang: Lang) -> String {
    match section {
        "pricing" => {
            let subs = get_all_subscriptions(db).await.unwrap_or_default();
            if subs.is_empty() {
                // Every other branch returns pre-formatted MarkdownV2, and the caller
                // sends with ParseMode::MarkdownV2. This string is plain prose with
                // bare `.` — it must be escaped here or Telegram rejects the edit with
                // 400 and the tap silently does nothing.
                return escape_markdown_v2(t("no_subscription_available", lang));
            }
            let mut lines = Vec::new();
            for sub in &subs {
                let price = escape_markdown_v2(&format!("{:.2}", sub.price));
                lines.push(tf(
                    "help_pricing",
                    lang,
                    &[
                        ("price", &price),
                        ("days", &sub.days.to_string()),
                        ("data_limit", &(sub.limit_gb as i64).to_string()),
                    ],
                ));
            }
            lines.join("\n\n")
        }
        "topup" | "howtobuy" => {
            let subs = get_all_subscriptions(db).await.unwrap_or_default();
            let min_price = subs.iter().map(|s| s.price).fold(f64::INFINITY, f64::min);
            let price = if min_price.is_infinite() {
                escape_markdown_v2("0.00")
            } else {
                escape_markdown_v2(&format!("{:.2}", min_price))
            };
            if section == "topup" {
                tf("help_topup", lang, &[("price", &price)])
            } else {
                tf("help_howtobuy", lang, &[("price", &price)])
            }
        }
        "commands" => t("help_commands", lang).to_string(),
        "support" => t("help_support", lang).to_string(),
        _ => t("help_menu", lang).to_string(),
    }
}

/// Handle the /help command — send help menu with inline buttons.
pub async fn handle_help(bot: &Bot, _db: &DatabaseConnection, msg: &Message, lang: Lang) {
    let text = t("help_menu", lang);
    let _ = bot
        .send_message(msg.chat.id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .reply_markup(help_menu_keyboard(lang))
        .await;
}

/// Handle the "Help" callback button — send help menu with inline buttons.
pub async fn handle_help_callback(
    bot: &Bot,
    _db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    if let Some(msg) = &cq.message {
        let text = t("help_menu", lang);
        let _ = bot
            .edit_message_text(msg.chat().id, msg.id(), text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(help_menu_keyboard(lang))
            .await;
    }
}

/// Handle a help section callback — show section text + "Back to Help" button.
pub async fn handle_help_section_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
    section: &str,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let text = get_section_text(db, section, lang).await;
    if let Some(msg) = &cq.message {
        let _ = bot
            .edit_message_text(msg.chat().id, msg.id(), text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(back_to_help_keyboard(lang))
            .await;
    }
}
