use sea_orm::DatabaseConnection;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message};

use super::buttons::{CB_LANG_EN, CB_LANG_RU, CB_LANG_TK};
use super::start::send_main_menu;
use crate::db::set_user_language;
use crate::i18n::{t, Lang};

/// Show the language selection inline keyboard.
pub async fn handle_lang_button(bot: &Bot, msg: &Message, lang: Lang) {
    let keyboard = lang_inline_keyboard();
    let text = t("lang_select", lang);

    let _ = bot
        .send_message(msg.chat.id, text)
        .reply_markup(keyboard)
        .await;
}

/// Handle language selection callback.
pub async fn handle_lang_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    new_lang: Lang,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let telegram_user_id = cq.from.id.0 as i64;

    // Update language in DB
    if let Err(e) = set_user_language(db, telegram_user_id, new_lang).await {
        eprintln!("Failed to set user language: {}", e);
    }

    // Send new main menu with updated language
    if let Some(msg) = &cq.message {
        send_main_menu(bot, msg.chat().id, new_lang).await;
    }
}

fn lang_inline_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("🇹🇲 Türkmen", CB_LANG_TK),
        InlineKeyboardButton::callback("🇷🇺 Русский", CB_LANG_RU),
        InlineKeyboardButton::callback("🇬🇧 English", CB_LANG_EN),
    ]])
}
