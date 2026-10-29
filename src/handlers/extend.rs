//! Extend an existing Marzban account instead of creating another one.
//!
//! The flow is stateless — the account name and plan id ride in the callback data,
//! mirroring the `buy_sub:{id}` pattern — so there is no conversation state to lose
//! across a restart. The only shared state is `EXTEND_IN_PROGRESS`.
//!
//! Terms (settled with the operator):
//!   * the user picks any *current* plan; they always pay today's catalogue price
//!   * 60% of unused traffic carries over (a 40% cut), with no minimum threshold
//!   * remaining days are FORFEITED — `expire = now + plan.days`
//!   * usage is reset to zero so "Used: X / Y" stays meaningful per cycle

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use sea_orm::DatabaseConnection;
use serde_json::json;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup};

use super::buttons;
use super::start::main_keyboard;
use super::subscriptions::owns_account;
use crate::auth::get_marzban_client;
use crate::auth::http_client::AuthenticatedHttpClient;
use crate::db::{deduct_balance, get_all_subscriptions, get_subscription_by_id, refund_balance};
use crate::i18n::{t, tf, Lang};
use crate::note::{ymd, Note};
use crate::utils::escape_markdown_v2;

const BYTES_PER_GB: f64 = 1_073_741_824.0;

/// Fraction of unused traffic that survives an extension (40% is cut).
const CARRY_RATE: f64 = 0.60;

/// Telegram's hard limit on `callback_data`.
const CALLBACK_DATA_MAX: usize = 64;

/// Telegram user ids with an extend already running.
///
/// Extending is read-modify-write across two HTTP calls. Without this, two quick taps
/// both read `expire = E` and both write `now + days`, so the user is charged twice and
/// extended once. The bot is a single process, so an in-process set is a sufficient lock.
/// Re-entry is rejected rather than queued — the second tap is virtually always a
/// double-tap, and making the user wait on a silent queue is worse than telling them.
static EXTEND_IN_PROGRESS: LazyLock<Mutex<HashSet<i64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// RAII guard so the lock is released on every exit path, including early returns.
struct ExtendGuard(i64);

impl Drop for ExtendGuard {
    fn drop(&mut self) {
        let id = self.0;
        tokio::spawn(async move {
            EXTEND_IN_PROGRESS.lock().await.remove(&id);
        });
    }
}

/// Take the per-user lock. `None` means an extend is already running for this user.
async fn try_lock(telegram_id: i64) -> Option<ExtendGuard> {
    let mut inflight = EXTEND_IN_PROGRESS.lock().await;
    if inflight.insert(telegram_id) {
        Some(ExtendGuard(telegram_id))
    } else {
        None
    }
}

/// What an extension will grant, computed from the account's current state.
#[derive(Debug, PartialEq)]
struct ExtendPlan {
    /// Unused bytes on the current account.
    remaining: i64,
    /// Bytes carried into the new cycle (`CARRY_RATE` of `remaining`).
    carry: i64,
    /// Absolute value to write to `data_limit`.
    new_data_limit: i64,
    /// Absolute value to write to `expire`.
    new_expire: u64,
}

/// Compute an extension.
///
/// `data_limit` is an absolute cap compared against cumulative `used_traffic`, and a
/// modify never resets usage — so the new limit is `plan GB + carry`, and the caller
/// must reset usage afterwards for that to be the user's actual allowance.
///
/// `expire` is replaced, not extended: remaining days are forfeited by design.
fn compute(
    data_limit: i64,
    used_traffic: i64,
    plan_limit_gb: f64,
    plan_days: i64,
    now: u64,
) -> ExtendPlan {
    let remaining = (data_limit - used_traffic).max(0);
    let carry = (remaining as f64 * CARRY_RATE) as i64;
    ExtendPlan {
        remaining,
        carry,
        new_data_limit: (plan_limit_gb * BYTES_PER_GB) as i64 + carry,
        new_expire: now + (plan_days as u64) * 86_400,
    }
}

fn gb(bytes: i64) -> String {
    format!("{:.2}", bytes as f64 / BYTES_PER_GB)
}

/// The panel's own username charset (`USERNAME_REGEXP`: letters, digits, `-_@.`).
///
/// Used as a guard before interpolating a callback-supplied name into a request path.
pub(crate) fn is_panel_username(s: &str) -> bool {
    // `.` is in the panel's charset, so a charset test alone accepts `..` — which is
    // precisely the input that escapes the path. The panel's own regex begins
    // `(?=\w{3,32}\b)`, i.e. the name must open with 3+ word characters, which `..`
    // cannot. Mirror that, and reject any dot segment outright.
    let ok_chars = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '@' | '.'));
    let opens_wordy = s
        .chars()
        .take(3)
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count()
        == 3;
    (3..=32).contains(&s.len()) && ok_chars && opens_wordy && !s.contains("..")
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Fetch an account, but only if it belongs to `telegram_id`.
///
/// Ownership is re-checked here even though the listing already filtered: callback data
/// is user-supplied, so a crafted payload could otherwise name someone else's account.
///
/// The check now happens AFTER the fetch, because ownership lives in the account's note
/// and the username proves nothing — a user may choose their own. That costs one panel
/// GET for a bogus payload, which the per-user rate limit already bounds; the previous
/// order could not survive user-chosen usernames at all.
async fn fetch_owned(
    client: &AuthenticatedHttpClient,
    username: &str,
    telegram_id: i64,
) -> Option<serde_json::Value> {
    // The username arrives in callback data and is interpolated straight into a path.
    // `url` normalises dot segments, so `../../api/admins` would issue that request with
    // the bot's admin bearer token. Not escalatable today — the response is discarded
    // unless it carries a note the caller owns — but there is no reason to allow it.
    // This is the panel's own username charset.
    if !is_panel_username(username) {
        eprintln!(
            "Rejected extend: {:?} is not a valid panel username",
            username
        );
        return None;
    }
    let account = match client.get(&format!("/api/user/{}", username), None).await {
        Ok(resp) if resp.status == 200 => resp.data,
        Ok(resp) => {
            eprintln!("Extend: GET {} returned {}", username, resp.status);
            return None;
        }
        Err(e) => {
            eprintln!("Extend: GET {} failed: {}", username, e);
            return None;
        }
    };
    if !owns_account(&account, telegram_id) {
        eprintln!(
            "Rejected extend: TG:{} does not own '{}'",
            telegram_id, username
        );
        return None;
    }
    Some(account)
}

/// Step 1 — the user picked an account; show the plan catalogue.
pub async fn handle_extend_pick_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
    username: &str,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let chat_id: ChatId = cq.from.id.into();

    // Verify against the account itself, not the callback payload. This costs a panel
    // GET before showing the plan list, which is the price of ownership no longer being
    // derivable from the username.
    //
    // Tell the user when it fails. `fetch_owned` returns None for a forged payload AND
    // for a panel outage, a timeout, or an account an admin deleted — on `main` this
    // check was a pure string test that could not fail for a legitimate user, so
    // returning silently was fine. It is not any more: the user taps Extend, the
    // spinner clears, and nothing happens. Both sibling call sites already say so.
    let client = get_marzban_client();
    if fetch_owned(&client, username, telegram_id).await.is_none() {
        let _ = bot
            .send_message(chat_id, t("extend_failed", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    let subs = match get_all_subscriptions(db).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("DB error fetching subscriptions for extend: {}", e);
            let _ = bot
                .send_message(chat_id, t("balance_error", lang))
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };

    if subs.is_empty() {
        let _ = bot
            .send_message(chat_id, t("no_subscription_available", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    for sub in &subs {
        let data = format!("{}{}:{}", buttons::CB_EXT_PLAN_PREFIX, username, sub.id);
        // A username long enough to overflow the callback budget would make the button
        // unusable; skip it rather than let Telegram reject the whole keyboard.
        if data.len() > CALLBACK_DATA_MAX {
            eprintln!(
                "Extend: callback data too long ({} bytes), skipping",
                data.len()
            );
            continue;
        }
        rows.push(vec![InlineKeyboardButton::callback(
            format!(
                "{} — ${:.2} | {}d | {}GB",
                sub.type_name, sub.price, sub.days, sub.limit_gb as i64
            ),
            data,
        )]);
    }

    if rows.is_empty() {
        let _ = bot
            .send_message(chat_id, t("extend_failed", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    let _ = bot
        .send_message(chat_id, t("extend_select_plan", lang))
        .reply_markup(InlineKeyboardMarkup::new(rows))
        .await;
}

/// Step 2 — the user picked a plan; show exactly what they get, then ask to confirm.
pub async fn handle_extend_plan_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
    username: &str,
    plan_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let chat_id: ChatId = cq.from.id.into();

    let client = get_marzban_client();
    let Some(account) = fetch_owned(&client, username, telegram_id).await else {
        let _ = bot
            .send_message(chat_id, t("extend_failed", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    };

    let plan = match get_subscription_by_id(db, plan_id).await {
        Ok(Some(p)) => p,
        _ => {
            let _ = bot
                .send_message(chat_id, t("no_subscription_available", lang))
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };

    let Some((data_limit, used_traffic)) = eligible(&account) else {
        let _ = bot
            .send_message(chat_id, t("extend_not_eligible", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    };

    let calc = compute(
        data_limit,
        used_traffic,
        plan.limit_gb,
        plan.days as i64,
        now_ts(),
    );

    let text = tf(
        "extend_confirm",
        lang,
        &[
            ("plan", &escape_markdown_v2(&plan.type_name)),
            ("price", &escape_markdown_v2(&format!("{:.2}", plan.price))),
            ("days", &plan.days.to_string()),
            (
                "plan_gb",
                &escape_markdown_v2(&format!("{:.0}", plan.limit_gb)),
            ),
            ("carry_gb", &escape_markdown_v2(&gb(calc.carry))),
            ("total_gb", &escape_markdown_v2(&gb(calc.new_data_limit))),
        ],
    );

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback(
            t("btn_extend_confirm", lang),
            format!("{}{}:{}", buttons::CB_EXT_CONFIRM_PREFIX, username, plan_id),
        ),
        InlineKeyboardButton::callback(t("btn_extend_cancel", lang), buttons::CB_MY_SUBSCRIPTIONS),
    ]]);

    let _ = bot
        .send_message(chat_id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .reply_markup(kb)
        .await;
}

/// An account can be extended only if it has finite limits.
///
/// `0`/absent means *unlimited* for both `expire` and `data_limit`. No bot-created
/// account can be unlimited (admin plan input validates days and GB as `> 0`), so this
/// only guards manually created accounts — where extending would silently *downgrade*
/// unlimited access to a metered plan. Returns `(data_limit, used_traffic)` in bytes.
fn eligible(account: &serde_json::Value) -> Option<(i64, i64)> {
    let status = account.get("status").and_then(|v| v.as_str()).unwrap_or("");
    if status == "disabled" {
        return None;
    }
    let data_limit = account
        .get("data_limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let expire = account.get("expire").and_then(|v| v.as_i64()).unwrap_or(0);
    if data_limit <= 0 || expire <= 0 {
        return None;
    }
    let used = account
        .get("used_traffic")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some((data_limit, used))
}

/// Step 3 — charge, then apply.
pub async fn handle_extend_confirm_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
    lang: Lang,
    username: &str,
    plan_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_id = cq.from.id.0 as i64;
    let chat_id: ChatId = cq.from.id.into();

    let Some(_guard) = try_lock(telegram_id).await else {
        let _ = bot
            .send_message(chat_id, t("extend_in_progress", lang))
            .await;
        return;
    };

    let client = get_marzban_client();
    let Some(account) = fetch_owned(&client, username, telegram_id).await else {
        let _ = bot
            .send_message(chat_id, t("extend_failed", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    };

    let plan = match get_subscription_by_id(db, plan_id).await {
        Ok(Some(p)) => p,
        _ => {
            let _ = bot
                .send_message(chat_id, t("no_subscription_available", lang))
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };

    let Some((data_limit, used_traffic)) = eligible(&account) else {
        let _ = bot
            .send_message(chat_id, t("extend_not_eligible", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    };

    let calc = compute(
        data_limit,
        used_traffic,
        plan.limit_gb,
        plan.days as i64,
        now_ts(),
    );

    // Charge first — the conditional UPDATE is what prevents a negative balance.
    let description = format!("Extend {} (plan #{})", username, plan_id);
    let new_balance = match deduct_balance(db, telegram_id, plan.price, &description).await {
        Ok(b) => b,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("Insufficient balance") || msg.contains("not found") {
                let text = tf(
                    "insufficient_balance",
                    lang,
                    &[("price", &escape_markdown_v2(&format!("{:.2}", plan.price)))],
                );
                let _ = bot
                    .send_message(chat_id, text)
                    .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                    .reply_markup(main_keyboard(lang))
                    .await;
            } else {
                eprintln!("DB error during extend deduct: {}", e);
                let _ = bot
                    .send_message(chat_id, t("balance_error", lang))
                    .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                    .reply_markup(main_keyboard(lang))
                    .await;
            }
            return;
        }
    };

    // Refresh the note: it records what this account IS, and an extension changes the
    // plan, the price paid and the date. The telegram id is carried over from the note
    // the panel just returned, never from the request — the id is what ownership
    // depends on, so it must come from the account itself.
    //
    // If that id is somehow missing, the note is left ALONE. Writing a note without an
    // id would leave the account owned by nobody: `owns_account` would reject its real
    // owner, the listing would stop showing it, and no code path could restore it — a
    // user silently loses a subscription they paid for. `fetch_owned` above already
    // guarantees a parseable id, so this cannot currently fire; it is here so that the
    // guarantee is local instead of forty lines away in another function.
    // Both fields go in one PUT: the panel's expire branch only revives active/expired
    // users and its data_limit branch only revives non-expired/non-disabled ones, so
    // sending just one of them would leave some states un-revived.
    //
    // The note is refreshed too — it records what this account IS, and an extension
    // changes the plan, the price paid and the date. `telegram_id` is the authenticated
    // caller's, which `fetch_owned` has already proved equals the id in the account's
    // own note; an earlier revision re-parsed the note here instead, which only created
    // a second place for the two to disagree after a refactor. `Note::telegram_id` is
    // not an Option, so a note without an owner cannot be constructed at all.
    let body = json!({
        "expire": calc.new_expire,
        "data_limit": calc.new_data_limit,
        "note": Note {
            telegram_id,
            plan: Some(format!("{} ${:.2}", plan.type_name, plan.price)),
            date: Some(ymd(now_ts() as i64)),
            handle: cq.from.username.as_deref().map(|h| format!("@{}", h)),
        }
        .render(),
    });

    let applied = match client
        .put_json(&format!("/api/user/{}", username), body, None)
        .await
    {
        Ok(resp) if resp.status == 200 => true,
        other => {
            match &other {
                Ok(resp) => eprintln!("Extend PUT {} -> {} {:?}", username, resp.status, resp.data),
                Err(e) => eprintln!("Extend PUT {} failed: {}", username, e),
            }
            // The response can be lost even when the write landed (same failure mode
            // buy::verify_marzban_user exists for). Re-read and compare before refunding,
            // otherwise we refund an extension the user actually received.
            matches!(
                fetch_owned(&client, username, telegram_id).await,
                Some(v) if v.get("expire").and_then(|e| e.as_u64()) == Some(calc.new_expire)
            )
        }
    };

    if !applied {
        if let Err(e) = refund_balance(db, telegram_id, plan.price, "Refund: extend failed").await {
            eprintln!(
                "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                telegram_id, plan.price, e
            );
        }
        let _ = bot
            .send_message(chat_id, t("extend_failed", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    // Reset AFTER the PUT, never before: reset flips status back to active, so doing it
    // first on a `limited` account would revive it against the OLD data_limit with zero
    // usage — a free plan if the PUT then failed. This order can only under-credit.
    let mut reset_ok = false;
    for attempt in 1..=2 {
        match client
            .post_json(&format!("/api/user/{}/reset", username), json!({}), None)
            .await
        {
            Ok(resp) if resp.status == 200 => {
                reset_ok = true;
                break;
            }
            Ok(resp) => eprintln!(
                "Extend reset {} attempt {} -> {}",
                username, attempt, resp.status
            ),
            Err(e) => eprintln!(
                "Extend reset {} attempt {} failed: {}",
                username, attempt, e
            ),
        }
    }

    if !reset_ok {
        // The extension is paid for and applied, so no refund. The user simply has
        // their previous usage still counted against the new limit until this is fixed.
        eprintln!(
            "CRITICAL: extend applied but usage reset failed for {} (TG:{}) — \
             user is short {} bytes of allowance",
            username, telegram_id, used_traffic
        );
        let _ = bot
            .send_message(chat_id, t("extend_reset_pending", lang))
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    }

    println!(
        "Extended {} for TG:{}: +{}d, limit {} bytes (carry {}), balance {}",
        username, telegram_id, plan.days, calc.new_data_limit, calc.carry, new_balance
    );

    let balance_f64 = new_balance.to_string().parse::<f64>().unwrap_or(0.0);
    let text = tf(
        "extend_success",
        lang,
        &[
            ("plan", &escape_markdown_v2(&plan.type_name)),
            ("days", &plan.days.to_string()),
            ("total_gb", &escape_markdown_v2(&gb(calc.new_data_limit))),
            ("carry_gb", &escape_markdown_v2(&gb(calc.carry))),
            (
                "balance",
                &escape_markdown_v2(&format!("{:.2}", balance_f64)),
            ),
        ],
    );

    let _ = bot
        .send_message(chat_id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .reply_markup(main_keyboard(lang))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: i64 = 1_073_741_824;

    #[test]
    fn carries_sixty_percent_of_unused_traffic() {
        // 100GB plan, 50GB used -> 50GB left -> carry 30GB -> new limit 130GB
        let c = compute(100 * GB, 50 * GB, 100.0, 31, 1_000);
        assert_eq!(c.remaining, 50 * GB);
        assert_eq!(c.carry, 30 * GB);
        assert_eq!(c.new_data_limit, 130 * GB);
    }

    #[test]
    fn small_leftovers_still_carry_there_is_no_threshold() {
        let c = compute(100 * GB, 98 * GB, 100.0, 31, 1_000);
        assert_eq!(c.remaining, 2 * GB);
        assert_eq!(c.carry, (2.0 * GB as f64 * 0.6) as i64);
        assert!(c.new_data_limit > 100 * GB);
    }

    #[test]
    fn overshoot_carries_nothing_and_never_goes_negative() {
        // used_traffic can exceed data_limit between the panel's review passes.
        let c = compute(100 * GB, 120 * GB, 100.0, 31, 1_000);
        assert_eq!(c.remaining, 0);
        assert_eq!(c.carry, 0);
        assert_eq!(c.new_data_limit, 100 * GB);
    }

    #[test]
    fn expire_is_replaced_not_extended_remaining_days_are_forfeited() {
        let now = 1_000_000;
        let c = compute(100 * GB, 0, 100.0, 31, now);
        assert_eq!(c.new_expire, now + 31 * 86_400);
    }

    /// The username is interpolated into a request path, and `url` normalises dot
    /// segments — so `../../api/admins` would reach a different endpoint entirely,
    /// carrying the bot's admin token.
    #[test]
    fn a_callback_username_cannot_escape_the_path() {
        assert!(is_panel_username("tg_1317186982_1788074590"));
        assert!(is_panel_username("bagtyyar_phone"));
        assert!(is_panel_username("abc.def")); // dots allowed, but not in the first 3
                                               // The panel's own lookahead is `(?=\w{3,32}\b)`, so a name whose first three
                                               // characters are not word characters is invalid there too.
        assert!(!is_panel_username("a.b-c@d"));

        assert!(!is_panel_username("../../api/admins"));
        assert!(!is_panel_username(".."));
        assert!(!is_panel_username("a/b"));
        assert!(!is_panel_username("a%2fb"));
        assert!(!is_panel_username(""));
        assert!(!is_panel_username(&"x".repeat(33)));
        assert!(!is_panel_username("a b"));
    }

    #[test]
    fn callback_data_fits_telegram_budget() {
        // Worst realistic case: 10-digit telegram id + 10-digit timestamp, 4-digit plan.
        let username = "tg_1234567890_1712345678";
        for prefix in [buttons::CB_EXT_PLAN_PREFIX, buttons::CB_EXT_CONFIRM_PREFIX] {
            let data = format!("{}{}:{}", prefix, username, 9999);
            assert!(
                data.len() <= CALLBACK_DATA_MAX,
                "{} is {}",
                data,
                data.len()
            );
        }
        let pick = format!("{}{}", buttons::CB_EXT_PICK_PREFIX, username);
        assert!(pick.len() <= CALLBACK_DATA_MAX);
    }

    #[test]
    fn prefixes_are_unambiguous_longest_first() {
        // The router strips xsp:/xsc: before xs:; verify they can't be confused.
        let plan = format!("{}tg_1_2:3", buttons::CB_EXT_PLAN_PREFIX);
        assert!(plan.strip_prefix(buttons::CB_EXT_PICK_PREFIX).is_none());
        let confirm = format!("{}tg_1_2:3", buttons::CB_EXT_CONFIRM_PREFIX);
        assert!(confirm.strip_prefix(buttons::CB_EXT_PICK_PREFIX).is_none());
    }

    #[test]
    fn unlimited_and_disabled_accounts_are_not_eligible() {
        let unlimited_data =
            json!({"status":"active","data_limit":0,"expire":123,"used_traffic":0});
        assert_eq!(eligible(&unlimited_data), None);

        let unlimited_time =
            json!({"status":"active","data_limit":123,"expire":0,"used_traffic":0});
        assert_eq!(eligible(&unlimited_time), None);

        let disabled = json!({"status":"disabled","data_limit":123,"expire":123,"used_traffic":0});
        assert_eq!(eligible(&disabled), None);

        let ok = json!({"status":"limited","data_limit":100,"expire":123,"used_traffic":40});
        assert_eq!(eligible(&ok), Some((100, 40)));
    }
}
