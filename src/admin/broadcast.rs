use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageId,
};
use teloxide::RequestError;
use tokio::sync::Mutex;
use tokio::time::sleep;

use super::buttons;
use super::db::get_all_users_for_broadcast;
use crate::db;

/// Pending broadcast messages keyed by admin chat_id
static PENDING_BROADCASTS: LazyLock<Mutex<HashMap<i64, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Batch size: 25 messages per batch (under Telegram's 30 msgs/sec limit)
const BROADCAST_BATCH_SIZE: usize = 25;
/// Pause between batches
const BATCH_PAUSE: Duration = Duration::from_millis(1100);
/// Max retries on 429 RetryAfter
const MAX_RETRIES: u32 = 3;
/// Update admin progress every N users
const PROGRESS_REPORT_INTERVAL: u32 = 50;

fn broadcast_confirm_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("📨 Ibermek", buttons::CB_BROADCAST_CONFIRM),
        InlineKeyboardButton::callback("❌ Goýbolsun", buttons::CB_BROADCAST_CANCEL),
    ]])
}

/// Send a single message with retry on 429 RetryAfter errors.
async fn send_with_retry(bot: &Bot, chat_id: ChatId, text: &str) -> Result<(), RequestError> {
    for attempt in 0..=MAX_RETRIES {
        match bot.send_message(chat_id, text).await {
            Ok(_) => return Ok(()),
            Err(RequestError::RetryAfter(seconds)) => {
                if attempt < MAX_RETRIES {
                    let wait = seconds.duration();
                    eprintln!(
                        "Rate limited, retrying after {:?} (attempt {}/{})",
                        wait,
                        attempt + 1,
                        MAX_RETRIES
                    );
                    sleep(wait).await;
                } else {
                    return Err(RequestError::RetryAfter(seconds));
                }
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// Handle an admin text message — store it and ask for confirmation.
pub async fn handle_admin_broadcast(bot: &Bot, db: &sea_orm::DatabaseConnection, msg: &Message) {
    let text = match msg.text() {
        Some(t) => t.to_string(),
        None => return,
    };

    let chat_id = msg.chat.id;
    // Key the pending broadcast by the *admin's user id*, not the chat id: the
    // confirm/cancel callbacks look it up by `cq.from.id`. Those coincide in a
    // private chat but diverge if /admin is used in a group, which silently lost
    // the message ("Ibermek üçin habar tapylmady") at confirm time.
    let Some(admin_id) = msg.from.as_ref().map(|u| u.id.0 as i64) else {
        return;
    };

    let user_count = match db::get_all_user_telegram_ids(db).await {
        Ok(ids) => ids.len(),
        Err(e) => {
            eprintln!("Failed to get user count for broadcast: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Maglumat bazasyndan ulanyjylary alyp bolmady.")
                .await;
            return;
        }
    };

    PENDING_BROADCASTS
        .lock()
        .await
        .insert(admin_id, text.clone());

    let confirm_msg = format!(
        "📢 Bu habary {} ulanyjynyň hemmesine ibermek isleýärsiňizmi?\n\n\
         ━━━━━━━━━━━━━━━━━━━━━━\n\
         {}\n\
         ━━━━━━━━━━━━━━━━━━━━━━",
        user_count, text
    );

    let _ = bot
        .send_message(chat_id, confirm_msg)
        .reply_markup(broadcast_confirm_keyboard())
        .await;
}

/// Handle the "broadcast_confirm" callback — send message to all users.
pub async fn handle_broadcast_confirm_callback(
    bot: &Bot,
    db: &sea_orm::DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let chat_id = cq.from.id.0 as i64;

    let Some(msg) = &cq.message else { return };
    let admin_chat_id = msg.chat().id;
    let status_msg_id = msg.id();

    let Some(text) = PENDING_BROADCASTS.lock().await.remove(&chat_id) else {
        let _ = bot
            .send_message(admin_chat_id, "⚠️ Ibermek üçin habar tapylmady.")
            .await;
        return;
    };

    let _ = bot
        .edit_message_text(admin_chat_id, status_msg_id, "⏳ Habarlar iberilýär...")
        .await;

    // Fetch all users with usernames for detailed reporting
    let users = match get_all_users_for_broadcast(db).await {
        Ok(u) => u,
        Err(e) => {
            eprintln!("Failed to get users for broadcast: {}", e);
            let _ = bot
                .send_message(admin_chat_id, "❌ Ulanyjylary alyp bolmady.")
                .await;
            return;
        }
    };

    let bot = bot.clone();
    let total = users.len();
    tokio::spawn(async move {
        broadcast_loop(&bot, &users, &text, admin_chat_id, status_msg_id, total).await;
    });
}

/// Background broadcast loop with detailed per-user delivery report.
async fn broadcast_loop(
    bot: &Bot,
    users: &[(i64, Option<String>)],
    text: &str,
    admin_chat_id: ChatId,
    status_msg_id: MessageId,
    total: usize,
) {
    let mut delivered: Vec<(i64, Option<String>)> = Vec::new();
    let mut failed: Vec<(i64, Option<String>, String)> = Vec::new();
    let mut last_reported = 0u32;

    for (i, chunk) in users.chunks(BROADCAST_BATCH_SIZE).enumerate() {
        if i > 0 {
            sleep(BATCH_PAUSE).await;
        }

        for (user_id, username) in chunk {
            let target = ChatId(*user_id);
            match send_with_retry(bot, target, text).await {
                Ok(()) => delivered.push((*user_id, username.clone())),
                Err(e) => failed.push((*user_id, username.clone(), format!("{}", e))),
            }
        }

        let processed = (delivered.len() + failed.len()) as u32;
        if processed >= last_reported + PROGRESS_REPORT_INTERVAL {
            last_reported = processed;
            let progress = format!("⏳ Iberilýär... {}/{}", processed, total);
            let _ = bot
                .edit_message_text(admin_chat_id, status_msg_id, &progress)
                .await;
        }
    }

    // Build detailed report
    let mut report = format!(
        "✅ Broadcast complete!\n\n\
         📨 Delivered: {}\n\
         ❌ Failed: {}\n\
         👥 Total: {}\n",
        delivered.len(),
        failed.len(),
        total
    );

    // Show delivered list
    if !delivered.is_empty() {
        report.push_str("\n── Delivered ──\n");
        for (uid, uname) in &delivered {
            let name = uname.as_deref().unwrap_or("-");
            report.push_str(&format!("✅ {} (@{})\n", uid, name));
        }
    }

    // Show failed list
    if !failed.is_empty() {
        report.push_str("\n── Failed ──\n");
        for (uid, uname, err) in &failed {
            let name = uname.as_deref().unwrap_or("-");
            report.push_str(&format!("❌ {} (@{}) — {}\n", uid, name, err));
        }
    }

    // Telegram messages have a 4096 char limit — split if needed
    let _ = bot
        .edit_message_text(admin_chat_id, status_msg_id, "📋 Broadcast report below...")
        .await;

    for chunk in split_message(&report, 4000) {
        let _ = bot.send_message(admin_chat_id, chunk).await;
    }
}

/// Split a long message into chunks that fit within Telegram's limit.
pub(super) fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();

    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // Find last newline before end to avoid splitting mid-line
        let split_at = if end == text.len() {
            end
        } else {
            match bytes[start..end].iter().rposition(|&b| b == b'\n') {
                Some(pos) => start + pos + 1,
                // No newline in this window, so we have to cut mid-line. `end` is a raw
                // byte offset and the report is full of emoji, so slicing there would
                // panic on a char boundary.
                None => cut_point(text, start, end),
            }
        };
        chunks.push(&text[start..split_at]);
        start = split_at;
    }

    chunks
}

/// Where to cut `text` when a chunk has to be split mid-line: the largest UTF-8 char
/// boundary `<= end`, but never `start` itself.
///
/// Both halves matter. Cutting at a raw byte offset panics on a multi-byte character,
/// and the broadcast report is built from emoji-prefixed lines. But walking back is not
/// sufficient on its own: if the window is narrower than the first character, the floor
/// lands back on `start`, the chunk is empty, `start` never advances, and the loop spins
/// forever — trading a panic for a hang, which is worse because it would be inside a
/// detached broadcast task where nothing would report it. So when the window cannot fit
/// even one character, emit that whole character and overshoot `max_len`.
///
/// (`str::floor_char_boundary` is still unstable, hence the hand-rolled walk.)
fn cut_point(text: &str, start: usize, end: usize) -> usize {
    let mut i = end;
    while i > start && !text.is_char_boundary(i) {
        i -= 1;
    }
    if i > start {
        return i;
    }
    match text[start..].chars().next() {
        Some(c) => start + c.len_utf8(),
        None => text.len(),
    }
}

/// Handle the "broadcast_cancel" callback — clear stored message.
pub async fn handle_broadcast_cancel_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let chat_id = cq.from.id.0 as i64;
    PENDING_BROADCASTS.lock().await.remove(&chat_id);

    if let Some(msg) = &cq.message {
        let _ = bot
            .edit_message_text(msg.chat().id, msg.id(), "❌ Habar iberilmesi ýatyryldy.")
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_newlines_and_loses_nothing() {
        let text = "alpha\nbeta\ngamma\n";
        let chunks = split_message(text, 8);
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|c| !c.is_empty()));
    }

    /// The report is built from emoji-prefixed lines. A window with no newline forces a
    /// mid-line cut, and cutting at a raw byte offset panics on a char boundary.
    #[test]
    fn a_long_multibyte_run_with_no_newline_does_not_panic() {
        let text = "✅".repeat(500); // 1500 bytes, zero newlines
        let chunks = split_message(&text, 7); // 7 is not a multiple of 3 — lands mid-emoji
        assert_eq!(chunks.concat(), text);
        assert!(chunks.iter().all(|c| !c.is_empty()));
    }

    /// Every chunk must always advance. Walking back to a char boundary can land on
    /// `start` when the window is narrower than the first character, which would make
    /// the caller's loop spin forever on an empty chunk — a hang instead of a panic.
    /// Widths 1 and 2 are below the 3-byte '✅' and 4-byte '🆔', so they pin that case.
    #[test]
    fn narrow_windows_still_make_progress() {
        for text in ["✅✅✅", "🆔🆔🆔", "a🆔b✅c"] {
            for max in 1usize..=6 {
                let chunks = split_message(text, max);
                assert!(
                    chunks.iter().all(|c| !c.is_empty()),
                    "empty chunk => infinite loop: {:?} @ max_len={}",
                    text,
                    max
                );
                assert_eq!(chunks.concat(), text, "{:?} @ max_len={}", text, max);
            }
        }
    }

    #[test]
    fn every_chunk_is_valid_utf8_and_reassembles() {
        let text = format!("{}\n{}", "🆔 12345 (@user) — ok".repeat(300), "tail");
        for max in [7usize, 13, 64, 4000] {
            let chunks = split_message(&text, max);
            assert_eq!(chunks.concat(), text, "max_len={}", max);
        }
    }
}
