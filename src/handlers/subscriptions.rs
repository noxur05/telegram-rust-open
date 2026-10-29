use std::time::{Duration, SystemTime, UNIX_EPOCH};
use teloxide::prelude::*;
use teloxide::types::{
    CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, KeyboardMarkup, Message,
};

use super::buttons;
use super::start::main_keyboard;
use crate::auth::get_marzban_client;
use crate::i18n::{t, tf, Lang};
use crate::utils::{escape_markdown_v2, escape_markdown_v2_code};
use tokio::time::sleep;

/// Does this Marzban account belong to this Telegram user?
///
/// **The only authority on ownership**, and it reads the account's `note`, never its
/// username. `subscription_url` is a bearer credential, so this is the one check
/// standing between one user and another user's subscription.
///
/// It used to match the username prefix `tg_{telegram_id}_`, which worked only while
/// the bot chose every username. It cannot survive users picking their own: the panel's
/// `USERNAME_REGEXP` admits `tg_1317186982_9999999999`, so a username-based check would
/// hand an account to whoever asked for the right name. The note is written solely by
/// this bot and never echoed back from user input, which is what makes it trustworthy.
///
/// The 32 accounts created before this carry `TG:{id} Balance`, which parses correctly,
/// so they were never in a window where ownership was undecidable.
///
/// Neither of the two things that still LOOK like ownership checks is one:
///
/// 1. The panel's `?search=` builds `username ILIKE '%<term>%' OR note ILIKE '%<term>%'`
///    with no LIKE escaping (`app/db/crud.py:412`), so `_` is a single-character
///    wildcard and `TG:123` also matches `TG:1234`. A prefilter, nothing more —
///    `note::parse_telegram_id` requires a delimiter after the digits for this reason.
/// 2. Callback data is user-supplied. A crafted callback can name any account, so every
///    extend handler re-checks against the account the panel actually returned.
pub(crate) fn owns(note: &str, telegram_id: i64) -> bool {
    crate::note::parse_telegram_id(note) == Some(telegram_id)
}

/// Ownership check against an account object as the panel returned it.
pub(crate) fn owns_account(account: &serde_json::Value, telegram_id: i64) -> bool {
    account
        .get("note")
        .and_then(|v| v.as_str())
        .is_some_and(|n| owns(n, telegram_id))
}

/// Hand the user exactly the subscription the panel issued.
///
/// The panel owns the domain — `build_subscription_url` resolves it from the owning
/// admin's `subscription_url`, falling back to `XRAY_SUBSCRIPTION_URL_PREFIX` — and
/// Happ accounts get a `happ://crypt5/…` deep link that has no host to rewrite at all.
///
/// This used to prepend a `SUBSCRIPTION_URL` from the bot's own `.env`, gated on
/// `starts_with("http")`. That was wrong twice over: it pinned every link to one
/// domain even though users are spread across several, and `happ://` failed the
/// "http" test, so those links reached the user as `https://<base>happ://crypt5/…`
/// and could not be opened. The bot no longer has an opinion about the domain.
fn resolve_sub_url(sub_url: &str) -> String {
    sub_url.to_string()
}

/// Map Marzban status to emoji
fn status_emoji(status: &str) -> &'static str {
    match status {
        "active" => "🟢",
        "expired" => "🔴",
        "limited" => "🟡",
        "disabled" => "⚫",
        "on_hold" => "🟠",
        _ => "⚪",
    }
}

/// Map Marzban status to localized text
fn status_text(status: &str, lang: Lang) -> &'static str {
    match status {
        "active" => t("status_active", lang),
        "expired" => t("status_expired", lang),
        "limited" => t("status_limited", lang),
        "disabled" => t("status_disabled", lang),
        "on_hold" => t("status_on_hold", lang),
        _ => t("status_unknown", lang),
    }
}

/// Convert bytes to GB string with 2 decimal places
fn bytes_to_gb(bytes: f64) -> String {
    format!("{:.2}", bytes / 1_073_741_824.0)
}

/// Calculate time remaining from a unix timestamp, returns human-readable localized string
fn time_remaining(expire_ts: u64, lang: Lang) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if expire_ts <= now {
        return t("time_expired", lang).to_string();
    }

    let remaining_secs = expire_ts - now;
    let days = remaining_secs / 86400;
    let hours = (remaining_secs % 86400) / 3600;

    if days > 0 {
        tf(
            "time_days_hours",
            lang,
            &[("days", &days.to_string()), ("hours", &hours.to_string())],
        )
    } else {
        tf("time_hours", lang, &[("hours", &hours.to_string())])
    }
}

/// Core logic: fetch and display all subscriptions for a Telegram user
async fn process_my_subscriptions(bot: &Bot, chat_id: ChatId, telegram_user_id: i64, lang: Lang) {
    let http_client = get_marzban_client();

    // Search the NOTE, since that is where identity lives now. Still only a prefilter:
    // this is a wildcarded ILIKE, so it also returns `TG:{id}<more digits>` — which is
    // exactly why `owns_account` re-checks with a delimiter-aware parse.
    let api_url = format!("/api/users?search=TG:{}&limit=50", telegram_user_id);

    let response = match http_client.get(&api_url, None).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error fetching subscriptions: {}", e);
            let text = t("subscriptions_error", lang);
            let _ = bot
                .send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };

    if response.status != 200 {
        eprintln!(
            "Marzban API error: {} - {:?}",
            response.status, response.data
        );
        let text = t("subscriptions_error", lang);
        let _ = bot
            .send_message(chat_id, text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    // `search=` is only a coarse prefilter — it is a wildcarded ILIKE over username AND
    // note, so it can return other users' accounts. `owns_account` is what decides, and
    // it reads the note: the username may be the user's own choice and proves nothing.
    let users: Vec<&serde_json::Value> = response
        .data
        .get("users")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|u| owns_account(u, telegram_user_id))
                .collect()
        })
        .unwrap_or_default();

    if users.is_empty() {
        let text = t("no_subscriptions", lang);
        let _ = bot
            .send_message(chat_id, text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    // Each account renders as one self-contained block; they are batched across several
    // messages rather than concatenated into one. A Happ `subscription_url` is ~915
    // characters, so from about the fourth account the single message blew past
    // Telegram's 4096-unit limit and came back 400 — and every send here is
    // `let _ = ...`, so the user tapped "My Subscriptions" and nothing happened at all.
    // Measured against production on 2026-08-30: one user with 11 accounts produced an
    // 11,725-unit message, and 3 of the 9 users who have ever bought were over the line.
    //
    // Batching is on WHOLE blocks deliberately. Cutting at a character offset could land
    // inside the backtick code span holding the URL, leaving an unbalanced entity and a
    // 400 for a different reason.
    let mut blocks: Vec<String> = Vec::new();
    let mut extend_rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();

    for (i, user) in users.iter().enumerate() {
        let status = user
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let used_traffic = user
            .get("used_traffic")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let data_limit = user
            .get("data_limit")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let expire = user.get("expire").and_then(|v| v.as_u64()).unwrap_or(0);
        let sub_url = user
            .get("subscription_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let emoji = status_emoji(status);
        let status_tk = status_text(status, lang);

        let used_gb = bytes_to_gb(used_traffic);
        let total_gb = if data_limit > 0.0 {
            bytes_to_gb(data_limit)
        } else {
            "♾️".to_string()
        };

        let time_left = if expire > 0 {
            time_remaining(expire, lang)
        } else {
            t("time_unlimited", lang).to_string()
        };

        // The account's own name on the panel. Worth showing now that it is becoming the
        // user's own choice rather than a generated `tg_{id}_{ts}` — with several
        // subscriptions the number alone says nothing about which is which.
        //
        // Rendered inside a code span, so it is tap-to-copy and needs only the code-span
        // escape: the panel's username charset (`a-zA-Z0-9-_@.`) is full of MarkdownV2
        // specials that would otherwise each need escaping, and `escape_markdown_v2`
        // would render literal backslashes inside the span.
        let username = user.get("username").and_then(|v| v.as_str()).unwrap_or("—");

        let item = tf(
            "subscription_item",
            lang,
            &[
                ("emoji", emoji),
                ("num", &(i + 1).to_string()),
                ("username", &escape_markdown_v2_code(username)),
                ("status", &escape_markdown_v2(status_tk)),
                ("used", &escape_markdown_v2(&used_gb)),
                ("total", &escape_markdown_v2(&total_gb)),
                ("time_left", &escape_markdown_v2(&time_left)),
            ],
        );
        let mut block = item;

        let full_url = resolve_sub_url(sub_url);

        if !full_url.is_empty() {
            // Escaped for a code span, not with the general escaper: inside backticks
            // only `\`` and `\\` are special. Unescaped, one backtick in a panel-issued
            // URL would close the span early and 400 the whole message — and the
            // batching below assumes every block's span is balanced.
            block.push_str(&tf(
                "subscription_url",
                lang,
                &[("url", &escape_markdown_v2_code(&full_url))],
            ));
        }

        block.push('\n');
        blocks.push(block);

        // One Extend button per listed account. `disabled` accounts are excluded —
        // extending cannot revive them (neither the expire nor the data_limit branch
        // of the panel's update_user reactivates a disabled user).
        if let Some(name) = user.get("username").and_then(|v| v.as_str()) {
            if status != "disabled" {
                extend_rows.push(vec![InlineKeyboardButton::callback(
                    tf("btn_extend", lang, &[("num", &(i + 1).to_string())]),
                    format!("{}{}", buttons::CB_EXT_PICK_PREFIX, name),
                )]);
            }
        }
    }

    let total_line = tf(
        "subscriptions_total",
        lang,
        &[("count", &escape_markdown_v2(&users.len().to_string()))],
    );

    let messages = batch_blocks(t("my_subscriptions_title", lang), &blocks, &total_line);

    // The reply keyboard rides on the last message only, so it ends up under the end of
    // the listing rather than in the middle of it.
    let last = messages.len().saturating_sub(1);
    for (i, text) in messages.iter().enumerate() {
        if i > 0 {
            // Telegram's per-chat guidance is about one message a second, and this loop
            // can now emit several in a row. Firing them back-to-back invites a 429 that
            // would drop one message out of the middle of the listing — leaving the user
            // a listing with a hole while the Extend keyboard below still offers buttons
            // for blocks that never rendered. `admin/broadcast.rs` already paces itself
            // for the same reason.
            sleep(INTER_MESSAGE_PAUSE).await;
        }
        let kb = if i == last {
            Some(main_keyboard(lang))
        } else {
            None
        };
        send_listing_chunk(bot, chat_id, text, kb).await;
    }

    // Sent as a second message: the listing above carries the reply keyboard, and a
    // single message cannot hold both a reply keyboard and an inline keyboard.
    if !extend_rows.is_empty() {
        let _ = bot
            .send_message(chat_id, t("extend_pick_subscription", lang))
            .reply_markup(InlineKeyboardMarkup::new(extend_rows))
            .await;
    }
}

/// Pause between the messages of one listing. Matches `admin/broadcast.rs`'s pacing.
const INTER_MESSAGE_PAUSE: Duration = Duration::from_millis(1100);

/// Retries for a 429 on one chunk.
const SEND_RETRIES: u32 = 2;

/// Send one chunk of the listing, honouring `RetryAfter` and logging anything else.
///
/// The rest of this file sends with `let _ = ...`, which is what made the original bug
/// invisible. Here it matters more than usual: a dropped chunk leaves a *partial*
/// listing, which looks correct and is not — so a failure has to reach the journal even
/// though there is nothing useful to show the user.
async fn send_listing_chunk(bot: &Bot, chat_id: ChatId, text: &str, kb: Option<KeyboardMarkup>) {
    for attempt in 0..=SEND_RETRIES {
        let mut req = bot
            .send_message(chat_id, text)
            .parse_mode(teloxide::types::ParseMode::MarkdownV2);
        if let Some(k) = kb.clone() {
            req = req.reply_markup(k);
        }
        match req.await {
            Ok(_) => return,
            Err(teloxide::RequestError::RetryAfter(secs)) if attempt < SEND_RETRIES => {
                eprintln!(
                    "Subscriptions listing rate limited for chat {}, retrying in {:?}",
                    chat_id.0,
                    secs.duration()
                );
                sleep(secs.duration()).await;
            }
            Err(e) => {
                eprintln!(
                    "Failed to send a subscriptions listing chunk to chat {}: {} \
                     — the user is seeing an INCOMPLETE list",
                    chat_id.0, e
                );
                return;
            }
        }
    }
}

/// Telegram rejects a message over 4096 UTF-16 code units. Leave headroom for the title,
/// the total line, and any entity overhead.
const TELEGRAM_MAX: usize = 4096;
const MESSAGE_BUDGET: usize = 3500;

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Pack whole account blocks into as few messages as fit under `MESSAGE_BUDGET`.
///
/// The title opens the first message and the total closes the last, so the listing still
/// reads as one thing. A block is never split: each one ends with a closed code span, so
/// keeping them intact is what guarantees every message is valid MarkdownV2. An
/// oversized single block is still emitted on its own — truncating a subscription URL
/// would be worse than a message Telegram might refuse.
fn batch_blocks(title: &str, blocks: &[String], total_line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = title.to_string();
    let mut cur_has_block = false;

    for b in blocks {
        // `cur_has_block` guards the flush: a message must never be emitted holding only
        // the title, which is what would happen if the very first block is oversized.
        if cur_has_block && utf16_len(&cur) + utf16_len(b) > MESSAGE_BUDGET {
            out.push(std::mem::take(&mut cur));
        }
        cur.push_str(b);
        cur_has_block = true;
    }

    // The total line is appended AFTER the budget check, so on its own `MESSAGE_BUDGET`
    // does not bound the final message — the real invariant is
    // `MESSAGE_BUDGET + len(total_line) <= TELEGRAM_MAX`. Rather than leave that implicit
    // (and quietly violable by anyone raising the budget later), enforce it here.
    if utf16_len(&cur) + utf16_len(total_line) > TELEGRAM_MAX {
        out.push(std::mem::take(&mut cur));
        cur.push_str(total_line);
    } else {
        cur.push_str(total_line);
    }
    out.push(cur);
    out
}

/// Handle "My Subscriptions" keyboard button press
pub async fn handle_my_subscriptions_message(bot: &Bot, msg: &Message, lang: Lang) {
    let Some(user) = &msg.from else { return };
    let telegram_user_id = user.id.0 as i64;
    process_my_subscriptions(bot, msg.chat.id, telegram_user_id, lang).await;
}

/// Handle "My Subscriptions" inline callback
pub async fn handle_my_subscriptions_callback(bot: &Bot, cq: &CallbackQuery, lang: Lang) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;

    let telegram_user_id = cq.from.id.0 as i64;
    if let Some(msg) = &cq.message {
        process_my_subscriptions(bot, msg.chat().id, telegram_user_id, lang).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        batch_blocks, owns, owns_account, resolve_sub_url, utf16_len, MESSAGE_BUDGET, TELEGRAM_MAX,
    };
    use crate::i18n::{t, Lang};
    use crate::utils::escape_markdown_v2_code;

    /// The panel owns the domain; the bot must pass its value through untouched.
    ///
    /// Prefixing used to pin every link to one `SUBSCRIPTION_URL` even though users
    /// are spread across several panel domains, and it mangled `happ://` deep links
    /// into `https://<base>happ://…`. Every send site is `let _ = ...`, so neither
    /// failure produced a visible error — the user just got a link that didn't work.
    #[test]
    fn the_panels_url_is_passed_through_untouched() {
        // Happ deep link — no host to rewrite.
        assert_eq!(
            resolve_sub_url("happ://crypt5/AbC123"),
            "happ://crypt5/AbC123"
        );
        // Plaintext link on a domain that is NOT the bot's old configured one.
        assert_eq!(
            resolve_sub_url("https://example-b.com/sub/tok"),
            "https://example-b.com/sub/tok"
        );
        // Missing subscription_url stays empty so the caller can skip the line.
        assert_eq!(resolve_sub_url(""), "");
    }

    /// A Happ `subscription_url` measured on production is ~915 characters, so a block
    /// is ~1060 units and roughly three fit in a message. Before batching, a user with
    /// 11 accounts produced an 11,725-unit message: Telegram answered 400, the
    /// `let _ = ...` send discarded it, and "My Subscriptions" silently did nothing.
    /// 3 of the 9 users who had ever bought were over the limit.
    #[test]
    fn every_batched_message_fits_telegrams_limit() {
        let block = format!(
            "🟢 *1* — active\n🔗 *URL:*\n`happ://crypt5/{}`\n\n",
            "A".repeat(900)
        );
        for n in [0usize, 1, 2, 3, 4, 11, 50] {
            let blocks: Vec<String> = (0..n).map(|_| block.clone()).collect();
            let msgs = batch_blocks("📋 *Subscriptions*\n\n", &blocks, "\n*Total:* 1");
            assert!(!msgs.is_empty(), "n={n} produced no message");
            for (i, m) in msgs.iter().enumerate() {
                assert!(
                    utf16_len(m) <= 4096,
                    "n={n} message {i} is {} units",
                    utf16_len(m)
                );
            }
        }
    }

    /// Nothing may be dropped or duplicated: every block must appear exactly once
    /// across the batch, and blocks must never be cut apart (a split inside the URL's
    /// code span would unbalance the backticks and 400 for a different reason).
    #[test]
    fn batching_preserves_every_block_intact() {
        let blocks: Vec<String> = (0..11)
            .map(|i| format!("BLOCK{i}[`{}`]\n", "x".repeat(900)))
            .collect();
        let msgs = batch_blocks("TITLE", &blocks, "TOTAL");
        let joined = msgs.concat();
        for b in &blocks {
            assert_eq!(joined.matches(b.as_str()).count(), 1, "block lost or split");
        }
        assert!(
            msgs.len() > 1,
            "11 x 900-char blocks must span several messages"
        );
        assert!(msgs[0].starts_with("TITLE"));
        assert!(msgs.last().unwrap().ends_with("TOTAL"));
        // Backticks balanced in every message — the property that keeps MarkdownV2 valid.
        for m in &msgs {
            assert_eq!(m.matches('`').count() % 2, 0, "unbalanced code span");
        }
    }

    /// An account with no subscriptions still gets the title and the total.
    #[test]
    fn an_empty_listing_is_still_one_message() {
        let msgs = batch_blocks("TITLE", &[], "TOTAL");
        assert_eq!(msgs, vec!["TITLETOTAL".to_string()]);
    }

    /// A single block larger than the budget is emitted rather than dropped — truncating
    /// a subscription URL would be worse than a message Telegram may refuse. It may now
    /// land in its own message with the total moved after it, so the property to assert
    /// is that the block survives exactly once, not that there is exactly one message.
    #[test]
    fn an_oversized_block_is_not_silently_discarded() {
        let huge = format!("`{}`\n", "y".repeat(MESSAGE_BUDGET * 2));
        let msgs = batch_blocks("T", std::slice::from_ref(&huge), "E");
        let joined = msgs.concat();
        assert_eq!(
            joined.matches(huge.as_str()).count(),
            1,
            "block was dropped or split"
        );
        assert!(joined.starts_with('T'));
        assert!(joined.ends_with('E'));
    }

    /// The review's point: `MESSAGE_BUDGET` alone does not bound the final message,
    /// because `total_line` is appended after the budget check. The real invariant is
    /// `MESSAGE_BUDGET + len(total_line) <= TELEGRAM_MAX`, and it must hold for the
    /// LONGEST total line across every locale — otherwise raising the budget later
    /// silently 400s the last message while the packing tests still pass.
    #[test]
    fn the_budget_leaves_room_for_the_longest_total_line() {
        let longest = [Lang::Tk, Lang::Ru, Lang::En]
            .iter()
            .map(|l| utf16_len(t("subscriptions_total", *l)) + 8) // +8 for the count
            .max()
            .unwrap();
        assert!(
            MESSAGE_BUDGET + longest <= TELEGRAM_MAX,
            "MESSAGE_BUDGET ({}) + longest total line ({}) exceeds {}",
            MESSAGE_BUDGET,
            longest,
            TELEGRAM_MAX
        );
    }

    /// Belt-and-braces for the same thing: even with a pathologically long total line,
    /// `batch_blocks` must never emit a message over the hard limit.
    #[test]
    fn an_oversized_total_line_is_moved_to_its_own_message() {
        let block = format!("`{}`\n", "z".repeat(1000));
        let blocks: Vec<String> = (0..3).map(|_| block.clone()).collect();
        let huge_total = "T".repeat(2000);
        let msgs = batch_blocks("TITLE", &blocks, &huge_total);
        for m in &msgs {
            assert!(
                utf16_len(m) <= TELEGRAM_MAX,
                "message is {} units",
                utf16_len(m)
            );
        }
        assert!(msgs.last().unwrap().ends_with(&huge_total));
    }

    /// A backtick in a panel-issued URL would close the code span early and 400 the
    /// whole message. Inside a code span only `` ` `` and `\` are special, so the
    /// general escaper would be wrong here — it would render literal backslashes.
    #[test]
    fn a_url_is_escaped_for_its_code_span_and_keeps_the_span_balanced() {
        let nasty = "happ://crypt5/AbC`def\\ghi";
        let escaped = escape_markdown_v2_code(nasty);
        let block = format!("🔗 *URL:*\n`{}`\n", escaped);
        assert_eq!(
            block.matches('`').count() - block.matches("\\`").count(),
            2,
            "code span must be balanced: {block}"
        );
        // Ordinary MarkdownV2 specials are left alone — they are literal inside a span.
        assert_eq!(escape_markdown_v2_code("a.b-c_d"), "a.b-c_d");
    }

    /// The wildcard-collision cases the panel's ILIKE would wrongly return: a search
    /// for `TG:123` also matches `TG:1234`, so a prefix test would leak one user's
    /// subscription_url to another.
    #[test]
    fn owns_matches_only_the_exact_id() {
        assert!(owns("TG:123 | Basic $2.50 | 2026-08-30 | @bob", 123));
        assert!(owns("TG:123 Balance", 123)); // the legacy format, unrewritten
        assert!(owns("TG:123", 123));

        assert!(!owns("TG:1234 | x", 123));
        assert!(!owns("TG:12 | x", 123));
        assert!(!owns("", 123));
        assert!(!owns("Yhhy", 123)); // a real reseller note from this panel

        // The username is now user-chosen and proves NOTHING. A user who picks the name
        // `tg_123_1712345678` must not thereby own user 123's account.
        assert!(!owns("tg_123_1712345678", 123));
    }

    /// Ownership reads the note off the account the panel returned, never the username
    /// in the callback payload.
    #[test]
    fn owns_account_reads_the_note_not_the_username() {
        let victim = serde_json::json!({
            "username": "tg_123_1712345678",
            "note": "TG:123 | Basic $2.50"
        });
        let impostor = serde_json::json!({
            "username": "tg_123_9999999999", // legal under the panel's own regex
            "note": "TG:999 | Basic $2.50"
        });
        assert!(owns_account(&victim, 123));
        assert!(
            !owns_account(&impostor, 123),
            "username must not confer ownership"
        );
        assert!(owns_account(&impostor, 999));
        // No note at all -> owned by nobody.
        assert!(!owns_account(
            &serde_json::json!({"username": "tg_123_1"}),
            123
        ));
    }
}
