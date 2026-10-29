use sea_orm::DatabaseConnection;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, InlineKeyboardButton, InlineKeyboardMarkup, Message};
use tokio::sync::Mutex;

use super::buttons;
use super::extend::is_panel_username;
use super::start::main_keyboard;
use super::subscriptions::owns_account;
use crate::auth::get_marzban_client;
use crate::auth::http_client::AuthenticatedHttpClient;
use crate::db::{
    deduct_balance, get_all_subscriptions, get_subscription_by_id, record_marzban_account,
    refund_balance, SubscriptionInfo,
};
use crate::i18n::{t, tf, Lang};
use crate::note::{ymd, Note};
use crate::utils::escape_markdown_v2;

/// Show the list of available subscription plans as inline buttons.
async fn show_subscription_list(bot: &Bot, db: &DatabaseConnection, chat_id: ChatId, lang: Lang) {
    let subs = match get_all_subscriptions(db).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("DB error fetching subscriptions: {}", e);
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
        let label = format!(
            "{} — ${:.2} | {}d | {}GB",
            sub.type_name, sub.price, sub.days, sub.limit_gb as i64
        );
        rows.push(vec![InlineKeyboardButton::callback(
            label,
            format!("{}{}", buttons::CB_BUY_SUB_PREFIX, sub.id),
        )]);
    }

    let kb = InlineKeyboardMarkup::new(rows);
    let _ = bot
        .send_message(chat_id, t("select_subscription", lang))
        .reply_markup(kb)
        .await;
}

/// Check whether OUR user exists. Used to detect cases where Marzban created the
/// account but the POST response was lost (e.g. Marzban's admin-notification call to
/// Telegram timed out under network restrictions, causing the POST to surface as an
/// error even though the user was created).
///
/// Ownership is re-checked on whatever comes back. This used to be implicitly safe:
/// the username WAS the ownership authority, so `tg_{buyer}_{now}` could only ever be
/// the buyer's account. Now that ownership lives in the note, an account sitting at
/// that name is not proof of anything — the panel has 30+ reseller admins who can
/// create accounts, and the whole point of this work is that users will choose their
/// own usernames. Without this check a pre-created account at the same name would hand
/// the buyer a `subscription_url` they do not own, AND skip their refund.
async fn verify_marzban_user(
    client: &AuthenticatedHttpClient,
    username: &str,
    telegram_user_id: i64,
) -> Option<serde_json::Value> {
    match client.get(&format!("/api/user/{}", username), None).await {
        Ok(resp) if resp.status == 200 && owns_account(&resp.data, telegram_user_id) => {
            Some(resp.data)
        }
        Ok(resp) if resp.status == 200 => {
            eprintln!(
                "CRITICAL: {} exists but is NOT owned by TG:{} — refusing to hand over \
                 its subscription_url; refunding instead",
                username, telegram_user_id
            );
            None
        }
        _ => None,
    }
}

/// Everything that must happen once an account is known to exist, whichever way we
/// found that out.
///
/// There are THREE success paths — a clean 200/201, and two "the account exists despite
/// an error response" arms — and this used to run on only the first. The other two are
/// exactly the case `verify_marzban_user` documents (Marzban creates the account, its
/// admin-notification call to Telegram times out, the POST surfaces as an error), so the
/// accounts most in need of a recovery row were the ones guaranteed not to get one.
async fn finalize_created_account(
    db: &DatabaseConnection,
    marzban_username: &str,
    telegram_user_id: i64,
    account: &serde_json::Value,
) {
    // Durable record of who owns this account. The runtime ownership check reads the
    // note, but the note is free text any panel reseller can edit — without this row a
    // cleared note is an unrecoverable lockout. Best-effort: the account exists and the
    // user has paid, so a failure here must not fail the purchase, but it must be loud.
    if let Err(e) = record_marzban_account(db, marzban_username, telegram_user_id).await {
        eprintln!(
            "CRITICAL: created {} for TG:{} but could not record the ownership row: {} \
             — a cleared note would now be unrecoverable",
            marzban_username, telegram_user_id, e
        );
    }

    // Tripwire: confirm the panel actually STORED the note. If it did not, the buyer
    // holds a working URL for an account owned by nobody. Log only — they paid and have
    // a usable subscription, so a refund would be wrong; this needs a human.
    if !owns_account(account, telegram_user_id) {
        eprintln!(
            "CRITICAL: {} was created for TG:{} but its note did not come back owned \
             — the account is ORPHANED and needs its note set by hand",
            marzban_username, telegram_user_id
        );
    }
}

/// Render and send the "subscription created" success message.
///
/// Takes the plan rather than its unpacked fields so the days/GB shown to the
/// user can't drift from the plan that was actually charged for.
async fn send_subscription_success(
    bot: &Bot,
    chat_id: ChatId,
    status_msg_id: teloxide::types::MessageId,
    user_data: &serde_json::Value,
    new_balance: rust_decimal::Decimal,
    sub: &SubscriptionInfo,
    lang: Lang,
) {
    let sub_days = sub.days as i64;
    let sub_data_limit_gb = sub.limit_gb;
    let sub_url = user_data
        .get("subscription_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let new_balance_f64 = new_balance.to_string().parse::<f64>().unwrap_or(0.0);
    let text = tf(
        "subscription_success",
        lang,
        &[
            ("url", sub_url),
            ("days", &sub_days.to_string()),
            ("data_limit", &(sub_data_limit_gb as i64).to_string()),
            (
                "balance",
                &escape_markdown_v2(&format!("{:.2}", new_balance_f64)),
            ),
        ],
    );
    let _ = bot
        .edit_message_text(chat_id, status_msg_id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await;
}

/// Users who have picked a plan and are being asked to name the subscription.
///
/// Maps telegram id -> the plan id they chose. In-memory and disposable like every other
/// input mode here; a restart just means they tap the plan again.
static PENDING_USERNAME: LazyLock<Mutex<HashMap<i64, i32>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The plan this user is naming, if any.
pub async fn pending_username_plan(telegram_id: i64) -> Option<i32> {
    PENDING_USERNAME.lock().await.get(&telegram_id).copied()
}

/// Drop the "waiting for a username" state, e.g. because a button or command cancelled it.
pub async fn cancel_username_input(telegram_id: i64) {
    PENDING_USERNAME.lock().await.remove(&telegram_id);
}

/// Is this name free on the panel?
///
/// A 404 means free. Anything else — including a transport error — is treated as TAKEN,
/// because creating on a name we could not check risks a 409 after the balance is
/// deducted. Refusing early costs the user a retry; guessing wrong costs a refund path.
async fn username_available(client: &AuthenticatedHttpClient, name: &str) -> bool {
    match client.get(&format!("/api/user/{}", name), None).await {
        Ok(resp) => resp.status == 404,
        Err(e) => {
            eprintln!("Username availability check for {:?} failed: {}", name, e);
            false
        }
    }
}

/// The keyboard offered when a chosen name turns out to be taken.
///
/// Always carries the plan id, so a button stays unambiguous even if tapped later, and
/// always offers the automatic escape hatch. Shared because both collision paths send
/// the same "…or pick one below" text, and one of them used to send it with nothing
/// below it.
async fn taken_name_keyboard(
    client: &AuthenticatedHttpClient,
    base: &str,
    plan_id: i32,
    lang: Lang,
) -> InlineKeyboardMarkup {
    let mut rows: Vec<Vec<InlineKeyboardButton>> = Vec::new();
    if let Some(sug) = suggest_username(client, base).await {
        rows.push(vec![InlineKeyboardButton::callback(
            tf("btn_username_use", lang, &[("name", &sug)]),
            format!("{}{}:{}", buttons::CB_BUY_NAME_USE_PREFIX, plan_id, sug),
        )]);
    }
    rows.push(vec![InlineKeyboardButton::callback(
        t("btn_username_auto", lang),
        format!("{}{}", buttons::CB_BUY_NAME_AUTO_PREFIX, plan_id),
    )]);
    InlineKeyboardMarkup::new(rows)
}

/// A free variant of `base`, for offering after a collision.
///
/// Appends 2, 3, … and stops at the first name the panel does not know. Bounded so a
/// popular base name cannot turn one tap into a long series of panel requests.
async fn suggest_username(client: &AuthenticatedHttpClient, base: &str) -> Option<String> {
    let trimmed: String = base.chars().take(28).collect();
    for n in 2..=6 {
        let candidate = format!("{}{}", trimmed, n);
        if is_panel_username(&candidate) && username_available(client, &candidate).await {
            return Some(candidate);
        }
    }
    None
}

/// The name used when the user asks for one to be generated.
///
/// Same shape the bot has always minted, and still unique per user per second.
fn auto_username(telegram_id: i64, now: u64) -> String {
    format!("tg_{}_{}", telegram_id, now)
}

/// Telegram user ids with a purchase already running.
///
/// Without this a double-tap inside one second charges twice for one subscription:
/// both taps deduct, both mint the same `tg_{id}_{now}` name from the same second, the
/// second POST 409s on the collision, and `verify_marzban_user` then finds the account
/// the FIRST tap created — which the buyer genuinely owns, so the ownership check
/// passes, "treating as success" fires, and no refund is issued.
///
/// `extend.rs` has carried the same guard since it was written, for the same reason.
static BUY_IN_PROGRESS: LazyLock<Mutex<HashSet<i64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// RAII guard so the lock is released on every exit path, including early returns.
struct BuyGuard(i64);

impl Drop for BuyGuard {
    fn drop(&mut self) {
        let id = self.0;
        tokio::spawn(async move {
            BUY_IN_PROGRESS.lock().await.remove(&id);
        });
    }
}

/// Take the per-user lock. `None` means a purchase is already running for this user.
async fn try_lock_buy(telegram_id: i64) -> Option<BuyGuard> {
    let mut inflight = BUY_IN_PROGRESS.lock().await;
    if inflight.insert(telegram_id) {
        Some(BuyGuard(telegram_id))
    } else {
        None
    }
}

/// The note stored on a newly created account.
///
/// `crate::note` guarantees the id is present and that no interpolated value can forge
/// a field; this function's job is only to supply the right id — the BUYER's telegram
/// id, never a chat id, which would orphan the account from the moment it was created.
fn account_note(
    telegram_user_id: i64,
    sub: &SubscriptionInfo,
    handle: Option<&str>,
    now: i64,
) -> String {
    Note {
        telegram_id: telegram_user_id,
        plan: Some(format!("{} ${:.2}", sub.type_name, sub.price)),
        date: Some(ymd(now)),
        handle: handle.map(|h| format!("@{}", h)),
    }
    .render()
}

/// Core buy subscription logic for a specific subscription plan.
#[allow(clippy::too_many_arguments)]
async fn process_buy_subscription(
    bot: &Bot,
    db: &DatabaseConnection,
    chat_id: ChatId,
    telegram_user_id: i64,
    handle: Option<&str>,
    lang: Lang,
    sub: SubscriptionInfo,
    marzban_username: String,
) {
    // Reject a second concurrent purchase rather than queueing it: the second tap is
    // virtually always a double-tap, and the failure mode is being charged twice.
    let Some(_guard) = try_lock_buy(telegram_user_id).await else {
        let _ = bot
            .send_message(chat_id, t("purchase_in_progress", lang))
            .reply_markup(main_keyboard(lang))
            .await;
        return;
    };

    let price = sub.price;
    let sub_days = sub.days as i64;
    let sub_data_limit_gb = sub.limit_gb;

    // Use the shared Marzban client throughout (reuses the cached token).
    let http_client = get_marzban_client();

    // Re-check the name is still free, HERE, immediately before any money moves.
    //
    // Three entry points reach this function — a typed name, an automatic one, and the
    // "✅ Use {name}" button — and only the typed one checked availability. That button
    // stays in the chat forever, so tapping it a second time deducted again, the create
    // 409'd, `verify_marzban_user` found the account the FIRST tap made (which the buyer
    // genuinely owns, so the ownership check passes), "treating as success" fired with
    // the same subscription_url, and no refund was issued. Charged twice, one account,
    // and the success message hid it. `BUY_IN_PROGRESS` does not help: the two taps do
    // not overlap.
    //
    // Guarding the shared path rather than each caller is the point — the bug was one
    // sibling missing a check the others had.
    if !username_available(&http_client, &marzban_username).await {
        // Put them back in the naming state so they can simply pick another — clearing
        // the other input modes first, exactly like every other arming site. Skipping
        // that here reproduced the very bug `handlers::clear_input_modes` was added to
        // prevent: a stale button could re-arm naming while a top-up was still live, and
        // the typed amount would then be read as a subscription name and rejected.
        super::clear_input_modes(telegram_user_id).await;
        PENDING_USERNAME
            .lock()
            .await
            .insert(telegram_user_id, sub.id);
        let _ = bot
            .send_message(
                chat_id,
                tf(
                    "username_taken",
                    lang,
                    &[("name", &escape_markdown_v2(&marzban_username))],
                ),
            )
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(taken_name_keyboard(&http_client, &marzban_username, sub.id, lang).await)
            .await;
        return;
    }

    // Step 1: Atomically deduct balance (prevents double-spend race condition)
    let new_balance =
        match deduct_balance(db, telegram_user_id, price, "Subscription purchase").await {
            Ok(nb) => nb,
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("Insufficient balance") || err_msg.contains("not found") {
                    let text = tf(
                        "insufficient_balance",
                        lang,
                        &[("price", &escape_markdown_v2(&format!("{:.2}", price)))],
                    );
                    let _ = bot
                        .send_message(chat_id, text)
                        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                        .reply_markup(main_keyboard(lang))
                        .await;
                } else {
                    eprintln!("DB error during deduct: {}", e);
                    let text = t("balance_error", lang);
                    let _ = bot
                        .send_message(chat_id, text)
                        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                        .reply_markup(main_keyboard(lang))
                        .await;
                }
                return;
            }
        };

    // Balance deducted successfully — now create the subscription
    let status_msg = match bot
        .send_message(chat_id, t("creating_subscription", lang))
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await
    {
        Ok(m) => m,
        Err(_) => {
            // Can't send status message — refund and bail
            if let Err(e) =
                refund_balance(db, telegram_user_id, price, "Refund: failed to send status").await
            {
                eprintln!(
                    "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                    telegram_user_id, price, e
                );
            }
            return;
        }
    };

    // Calculate expiration timestamp (now + subscription_days)
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expire_ts = now + (sub_days as u64 * 24 * 60 * 60);

    // Data limit in bytes (1 GB = 1,073,741,824 bytes)
    let data_limit = (sub_data_limit_gb * 1_073_741_824.0) as i64;

    // Fetch available inbounds from Marzban
    let inbounds_resp = match http_client.get("/api/inbounds", None).await {
        Ok(resp) if resp.status == 200 => resp.data,
        Ok(resp) => {
            eprintln!(
                "Failed to fetch inbounds: {} - {:?}",
                resp.status, resp.data
            );
            if let Err(e) = refund_balance(
                db,
                telegram_user_id,
                price,
                "Refund: failed to fetch inbounds",
            )
            .await
            {
                eprintln!(
                    "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                    telegram_user_id, price, e
                );
            }
            let text = t("subscription_failed_inbounds", lang);
            let _ = bot
                .edit_message_text(chat_id, status_msg.id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
            return;
        }
        Err(e) => {
            eprintln!("HTTP error fetching inbounds: {}", e);
            if let Err(e) = refund_balance(
                db,
                telegram_user_id,
                price,
                "Refund: inbound connection error",
            )
            .await
            {
                eprintln!(
                    "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                    telegram_user_id, price, e
                );
            }
            let text = t("subscription_failed_connection", lang);
            let _ = bot
                .edit_message_text(chat_id, status_msg.id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
            return;
        }
    };

    // Build proxies and inbounds from fetched data
    let mut proxies = json!({});
    let mut inbounds = json!({});

    if let Some(obj) = inbounds_resp.as_object() {
        for (protocol, items) in obj {
            let items_arr = items.as_array();

            // Check if any inbound for this protocol uses Reality TLS
            let has_reality = items_arr
                .map(|arr| {
                    arr.iter()
                        .any(|i| i.get("tls").and_then(|t| t.as_str()) == Some("reality"))
                })
                .unwrap_or(false);

            // Set proxy config per protocol (Marzban auto-generates UUIDs/passwords)
            match protocol.as_str() {
                "vless" => {
                    let flow = if has_reality { "xtls-rprx-vision" } else { "" };
                    proxies[protocol] = json!({"flow": flow});
                }
                "trojan" => {
                    let flow = if has_reality { "xtls-rprx-vision" } else { "" };
                    proxies[protocol] = json!({"flow": flow});
                }
                "shadowsocks" => {
                    proxies[protocol] = json!({"method": "chacha20-ietf-poly1305"});
                }
                _ => {
                    proxies[protocol] = json!({});
                }
            }

            // Collect all inbound tags for this protocol
            let tags: Vec<String> = items_arr
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            item.get("tag").and_then(|t| t.as_str()).map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();
            inbounds[protocol] = json!(tags);
        }
    }

    // Create user in Marzban with fetched proxy types and inbounds.
    //
    // `client_type` + `device_limit` bind the account to the Happ client and make
    // the panel return a `happ://` crypt5 deep link instead of a plaintext
    // subscription URL. The panel validates the two as a PAIR — UserCreate's
    // validate_device_policy rejects a create where one is set and the other is
    // null — so `device_limit` must always ride along, which is why it is a column
    // on the plan rather than a constant. Both are create-time only: UserModify
    // ignores them, so the extend flow can neither set nor clear them.
    let user_data = json!({
        "status": "active",
        "username": marzban_username,
        // The note is this bot's record of the account, and the ONLY place ownership is
        // recorded. Built by a named function so it can be tested: inline in this
        // `json!` a swapped argument would orphan every account created from then on,
        // invisibly until a buyer opened My Subscriptions.
        "note": account_note(telegram_user_id, &sub, handle, now as i64),
        "proxies": proxies,
        "data_limit": data_limit,
        "expire": expire_ts,
        "data_limit_reset_strategy": "no_reset",
        "inbounds": inbounds,
        "client_type": "happ",
        "device_limit": sub.device_limit
    });

    println!(
        "Creating Marzban user: {} with data_limit: {} bytes, expire: {}",
        marzban_username, data_limit, expire_ts
    );

    match http_client
        .post_json("/api/user", user_data.clone(), None)
        .await
    {
        Ok(response) if response.status == 200 || response.status == 201 => {
            println!(
                "User created successfully for TG:{}, response: {:?}",
                telegram_user_id, response.data
            );
            finalize_created_account(db, &marzban_username, telegram_user_id, &response.data).await;
            send_subscription_success(
                bot,
                chat_id,
                status_msg.id,
                &response.data,
                new_balance,
                &sub,
                lang,
            )
            .await;
        }
        Ok(response) => {
            eprintln!(
                "Failed to create user: {} - {:?}",
                response.status, response.data
            );
            if let Some(verified) =
                verify_marzban_user(&http_client, &marzban_username, telegram_user_id).await
            {
                println!(
                    "User {} exists despite error response — treating as success",
                    marzban_username
                );
                finalize_created_account(db, &marzban_username, telegram_user_id, &verified).await;
                send_subscription_success(
                    bot,
                    chat_id,
                    status_msg.id,
                    &verified,
                    new_balance,
                    &sub,
                    lang,
                )
                .await;
                return;
            }
            if let Err(e) = refund_balance(
                db,
                telegram_user_id,
                price,
                "Refund: Marzban user creation failed",
            )
            .await
            {
                eprintln!(
                    "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                    telegram_user_id, price, e
                );
            }
            let text = t("subscription_failed_creation", lang);
            let _ = bot
                .edit_message_text(chat_id, status_msg.id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
        Err(e) => {
            eprintln!("HTTP error creating user: {}", e);
            if let Some(verified) =
                verify_marzban_user(&http_client, &marzban_username, telegram_user_id).await
            {
                println!(
                    "User {} exists despite HTTP error — treating as success",
                    marzban_username
                );
                finalize_created_account(db, &marzban_username, telegram_user_id, &verified).await;
                send_subscription_success(
                    bot,
                    chat_id,
                    status_msg.id,
                    &verified,
                    new_balance,
                    &sub,
                    lang,
                )
                .await;
                return;
            }
            if let Err(e) = refund_balance(
                db,
                telegram_user_id,
                price,
                "Refund: Marzban connection error",
            )
            .await
            {
                eprintln!(
                    "CRITICAL: Refund failed for TG:{}, amount:{}, reason: {}",
                    telegram_user_id, price, e
                );
            }
            let text = t("subscription_failed_connection", lang);
            let _ = bot
                .edit_message_text(chat_id, status_msg.id, text)
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .await;
        }
    }
}

/// Handle buy subscription button from keyboard (message) — show plan list.
pub async fn handle_buy_subscription_message(
    bot: &Bot,
    db: &DatabaseConnection,
    msg: &Message,
    lang: Lang,
) {
    show_subscription_list(bot, db, msg.chat.id, lang).await;
}

/// Handle buy subscription callback (inline button) — show plan list.
pub async fn handle_buy_subscription_callback(
    bot: &Bot,
    cq: &CallbackQuery,
    db: &DatabaseConnection,
    lang: Lang,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    show_subscription_list(bot, db, cq.from.id.into(), lang).await;
}

/// Handle user picking a specific subscription plan (callback `buy_sub:{id}`).
pub async fn handle_buy_sub_callback(
    bot: &Bot,
    cq: &CallbackQuery,
    db: &DatabaseConnection,
    lang: Lang,
    sub_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_user_id = cq.from.id.0 as i64;

    let sub = match get_subscription_by_id(db, sub_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            let _ = bot
                .send_message(cq.from.id, t("no_subscription_available", lang))
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
        Err(e) => {
            eprintln!("DB error fetching subscription: {}", e);
            let _ = bot
                .send_message(cq.from.id, t("balance_error", lang))
                .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };

    // Ask what to call it before charging anything. The name is the account's identity
    // on the panel and cannot be changed afterwards (`app/routers/user.py:162`), so it is
    // worth one extra step.
    // Only one input mode at a time — see `handlers::clear_input_modes`. Also means
    // tapping a second plan replaces the first rather than leaving both live.
    super::clear_input_modes(telegram_user_id).await;
    PENDING_USERNAME
        .lock()
        .await
        .insert(telegram_user_id, sub.id);

    let kb = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        t("btn_username_auto", lang),
        format!("{}{}", buttons::CB_BUY_NAME_AUTO_PREFIX, sub.id),
    )]]);
    let _ = bot
        .send_message(cq.from.id, t("username_prompt", lang))
        .reply_markup(kb)
        .await;
}

/// The user typed a name for the subscription they are buying.
pub async fn handle_username_input(
    bot: &Bot,
    db: &DatabaseConnection,
    msg: &Message,
    lang: Lang,
    plan_id: i32,
) {
    let Some(user) = &msg.from else { return };
    let telegram_user_id = user.id.0 as i64;
    let chat_id = msg.chat.id;
    let Some(name) = msg.text().map(str::trim) else {
        return;
    };

    if !is_panel_username(name) {
        let _ = bot.send_message(chat_id, t("username_invalid", lang)).await;
        return; // stay in the naming state so they can just try again
    }

    let client = get_marzban_client();
    if !username_available(&client, name).await {
        // Offer a free variant they can accept with one tap, and let them type another.
        let kb = taken_name_keyboard(&client, name, plan_id, lang).await;
        let _ = bot
            .send_message(
                chat_id,
                tf(
                    "username_taken",
                    lang,
                    &[("name", &escape_markdown_v2(name))],
                ),
            )
            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
            .reply_markup(kb)
            .await;
        return;
    }

    cancel_username_input(telegram_user_id).await;
    buy_with_name(
        bot,
        db,
        chat_id,
        telegram_user_id,
        user.username.as_deref(),
        lang,
        plan_id,
        name.to_string(),
    )
    .await;
}

/// "Generate one for me".
pub async fn handle_buy_name_auto_callback(
    bot: &Bot,
    cq: &CallbackQuery,
    db: &DatabaseConnection,
    lang: Lang,
    plan_id: i32,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_user_id = cq.from.id.0 as i64;
    cancel_username_input(telegram_user_id).await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = auto_username(telegram_user_id, now);
    buy_with_name(
        bot,
        db,
        cq.from.id.into(),
        telegram_user_id,
        cq.from.username.as_deref(),
        lang,
        plan_id,
        name,
    )
    .await;
}

/// "Use the suggested name".
pub async fn handle_buy_name_use_callback(
    bot: &Bot,
    cq: &CallbackQuery,
    db: &DatabaseConnection,
    lang: Lang,
    plan_id: i32,
    name: &str,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let telegram_user_id = cq.from.id.0 as i64;
    // Re-validate: the name rides in callback data, which is user-supplied.
    if !is_panel_username(name) {
        let _ = bot
            .send_message(cq.from.id, t("username_invalid", lang))
            .await;
        return;
    }
    cancel_username_input(telegram_user_id).await;
    buy_with_name(
        bot,
        db,
        cq.from.id.into(),
        telegram_user_id,
        cq.from.username.as_deref(),
        lang,
        plan_id,
        name.to_string(),
    )
    .await;
}

/// Look the plan up, then buy under the chosen name.
#[allow(clippy::too_many_arguments)]
async fn buy_with_name(
    bot: &Bot,
    db: &DatabaseConnection,
    chat_id: ChatId,
    telegram_user_id: i64,
    handle: Option<&str>,
    lang: Lang,
    plan_id: i32,
    marzban_username: String,
) {
    let sub = match get_subscription_by_id(db, plan_id).await {
        Ok(Some(s)) => s,
        _ => {
            let _ = bot
                .send_message(chat_id, t("no_subscription_available", lang))
                .reply_markup(main_keyboard(lang))
                .await;
            return;
        }
    };
    process_buy_subscription(
        bot,
        db,
        chat_id,
        telegram_user_id,
        handle,
        lang,
        sub,
        marzban_username,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> SubscriptionInfo {
        SubscriptionInfo {
            id: 1,
            type_name: "Basic".into(),
            price: 2.5,
            days: 31,
            limit_gb: 100.0,
            device_limit: 2,
        }
    }

    /// The note is what makes an account findable and extendable. Built inline in a
    /// `json!` it was untested, and a swapped argument — a chat id instead of the
    /// buyer's id — would orphan every account created from then on, invisibly.
    #[test]
    fn the_created_note_is_owned_by_the_buyer() {
        let note = account_note(1317186982, &plan(), Some("bagtyyar"), 1_788_074_590);
        assert_eq!(note, "TG:1317186982 | Basic $2.50 | 2026-08-30 | @bagtyyar");
        assert!(owns_account(
            &serde_json::json!({ "note": note }),
            1317186982
        ));
        // And nobody else's.
        assert!(!owns_account(
            &serde_json::json!({ "note": account_note(1317186982, &plan(), None, 0) }),
            131718698
        ));
    }

    /// A user with no Telegram handle still gets an owned, parseable note.
    #[test]
    fn a_missing_handle_does_not_break_ownership() {
        let note = account_note(6378429620, &plan(), None, 1_788_074_590);
        assert_eq!(note, "TG:6378429620 | Basic $2.50 | 2026-08-30");
        assert!(owns_account(
            &serde_json::json!({ "note": note }),
            6378429620
        ));
    }

    /// A generated name must satisfy the panel's own rules, or the create 422s AFTER the
    /// balance has been deducted and the user is bounced through the refund path for no
    /// reason. `tg_` opens with three word characters, which is what the panel's
    /// `(?=\w{3,32}\b)` lookahead requires.
    #[test]
    fn an_auto_generated_name_is_a_valid_panel_username() {
        for id in [1317186982i64, 7, 999999999999] {
            for now in [0u64, 1_788_074_590, u32::MAX as u64] {
                let name = auto_username(id, now);
                assert!(
                    is_panel_username(&name),
                    "{name} is not acceptable to the panel"
                );
            }
        }
        assert_eq!(
            auto_username(1317186982, 1_788_074_590),
            "tg_1317186982_1788074590"
        );
    }

    /// The suggestion offered after a collision must itself be valid, and must stay
    /// inside the 32-character limit even when the base name is already at it.
    #[test]
    fn a_suggested_variant_stays_within_the_panel_limit() {
        for base_len in [3usize, 10, 28, 31, 32] {
            let base: String = "a".repeat(base_len);
            let trimmed: String = base.chars().take(28).collect();
            for n in 2..=6 {
                let candidate = format!("{}{}", trimmed, n);
                assert!(
                    candidate.len() <= 32,
                    "{candidate} is {} chars",
                    candidate.len()
                );
                assert!(is_panel_username(&candidate), "{candidate} invalid");
            }
        }
    }

    /// An admin-set plan name cannot forge a field or displace the id.
    #[test]
    fn a_hostile_plan_name_cannot_break_the_note() {
        let mut p = plan();
        p.type_name = "x | TG:999999999".into();
        let note = account_note(111, &p, Some("a|b"), 0);
        assert!(owns_account(&serde_json::json!({ "note": &note }), 111));
        assert!(!owns_account(
            &serde_json::json!({ "note": &note }),
            999999999
        ));
    }
}
