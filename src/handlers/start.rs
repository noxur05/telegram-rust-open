use sea_orm::DatabaseConnection;
use teloxide::prelude::*;
use teloxide::types::{ChatId, KeyboardButton, KeyboardMarkup, Message};

use crate::db::{get_user_language, save_user_from_telegram};
use crate::i18n::{t, Lang};

/// Handle the /start command: ensure user in DB and send welcome message.
pub async fn handle_start(bot: &Bot, db: &DatabaseConnection, msg: &Message) {
    if let Some(from) = &msg.from {
        let _ =
            save_user_from_telegram(db, from.id.0 as i64, from.username.as_deref(), from.is_bot)
                .await;
    }
    let lang = match &msg.from {
        Some(from) => get_user_language(db, from.id.0 as i64)
            .await
            .unwrap_or_default(),
        None => Lang::default(),
    };
    send_main_menu(bot, msg.chat.id, lang).await;
}

/// Create the main reply keyboard (appears at bottom of chat)
pub fn main_keyboard(lang: Lang) -> KeyboardMarkup {
    KeyboardMarkup::new(vec![
        vec![
            KeyboardButton::new(t("btn_balance", lang)),
            KeyboardButton::new(t("btn_buy", lang)),
        ],
        vec![KeyboardButton::new(t("btn_subscriptions", lang))],
        vec![
            KeyboardButton::new(t("btn_help", lang)),
            KeyboardButton::new(t("btn_support", lang)),
            KeyboardButton::new(t("btn_lang", lang)),
        ],
    ])
    .resize_keyboard()
    .persistent()
}

/// Send the main menu with reply keyboard at bottom.
pub async fn send_main_menu(bot: &Bot, chat_id: ChatId, lang: Lang) {
    let keyboard = main_keyboard(lang);
    let text = t("welcome", lang);

    let _ = bot
        .send_message(chat_id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .reply_markup(keyboard)
        .await;
}
