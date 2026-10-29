//! Re-check historical invoices that predate the payment poller, and credit any that
//! settled but were never claimed.
//!
//! Deliberately admin-driven with a dry run, never automatic. Two reasons:
//!
//!   * It moves real money into real balances, at a scale worth looking at first.
//!   * The ledger cannot distinguish "never credited" from "compensated by hand" —
//!     admin adjustments carry `payment_id = NULL`, so they are invisible to the
//!     `transaction_exists_by_payment_id` guard. A human has to eyeball the list.
//!
//! Candidates live in their own table, not `pending_payments`, so the poller can never
//! pick them up and credit history behind the operator's back.

use std::collections::HashMap;
use std::sync::LazyLock;
use tokio::sync::Mutex;

use sea_orm::DatabaseConnection;
use teloxide::prelude::*;
use teloxide::types::{CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup};

use super::broadcast::split_message;
use super::buttons;
use super::db::{get_unresolved_candidates, resolve_candidate};
use crate::auth::nowpayments::{NowPaymentsClient, StatusError};
use crate::config::get_config;
use crate::db::{get_user_language, top_up_balance, transaction_exists_by_payment_id};
use crate::i18n::tf;
use crate::payments::poller::{classify, is_already_credited, Outcome};
use crate::payments::ratelimit::RateLimiter;
use crate::utils::escape_markdown_v2;

/// One invoice the dry run proposes to credit.
#[derive(Debug, Clone)]
struct PlannedCredit {
    payment_id: String,
    telegram_user_id: i64,
    amount: f64,
    created_at: String,
}

/// Dry-run results awaiting confirmation, keyed by admin telegram id.
static PENDING_RECONCILE: LazyLock<Mutex<HashMap<i64, Vec<PlannedCredit>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub async fn clear_pending(admin_id: i64) {
    PENDING_RECONCILE.lock().await.remove(&admin_id);
}

/// Dry run: check every unresolved candidate and report what WOULD be credited.
/// Credits nothing and resolves nothing — a cancelled run leaves the table untouched.
pub async fn handle_reconcile_callback(bot: &Bot, db: &DatabaseConnection, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let candidates = match get_unresolved_candidates(db).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Reconcile: failed to load candidates: {}", e);
            let _ = bot
                .send_message(chat_id, "❌ Failed to load reconcile candidates.")
                .await;
            return;
        }
    };

    if candidates.is_empty() {
        let _ = bot
            .send_message(
                chat_id,
                "✅ Nothing to reconcile — no unresolved historical payments.",
            )
            .reply_markup(back_keyboard())
            .await;
        return;
    }

    let _ = bot
        .send_message(
            chat_id,
            format!("🔍 Checking {} historical payment(s)…", candidates.len()),
        )
        .await;

    let client = NowPaymentsClient::new_from_config(get_config());
    let mut planned: Vec<PlannedCredit> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    // Same 429 policy as the poller. Without it a rate-limited lookup was pushed onto
    // `skipped` as "lookup failed", visually identical to a candidate we deliberately
    // passed over — so a run that silently checked nothing looked like a clean result.
    // CLAUDE.md states the invariant: a 429 is not a lookup failure.
    let mut limiter = RateLimiter::new();
    let mut unchecked = 0usize;

    for c in &candidates {
        if !limiter.is_ready() {
            unchecked += 1;
            continue;
        }
        let raw = match client.get_payment_status(&c.payment_id).await {
            Ok(v) => {
                limiter.record_success();
                v
            }
            Err(StatusError::RateLimited { retry_after }) => {
                limiter.record_429(retry_after);
                unchecked += 1;
                continue;
            }
            Err(e) => {
                skipped.push(format!("• {} — lookup failed: {}", c.payment_id, e));
                continue;
            }
        };

        let status = raw
            .get("payment_status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let get = |k: &str| -> f64 {
            raw.get(k)
                .and_then(|x| {
                    x.as_f64()
                        .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
                })
                .unwrap_or(0.0)
        };

        let amount = match classify(
            status,
            get("pay_amount"),
            get("actually_paid"),
            get("price_amount"),
        ) {
            Outcome::Credit(a) => a,
            _ => {
                skipped.push(format!("• {} — {}", c.payment_id, status));
                continue;
            }
        };

        // The decisive guard: already in the ledger means already credited.
        match transaction_exists_by_payment_id(db, &c.payment_id).await {
            Ok(true) => {
                skipped.push(format!("• {} — already credited", c.payment_id));
                continue;
            }
            Ok(false) => {}
            Err(e) => {
                skipped.push(format!("• {} — ledger check failed: {}", c.payment_id, e));
                continue;
            }
        }

        planned.push(PlannedCredit {
            payment_id: c.payment_id.clone(),
            telegram_user_id: c.telegram_user_id,
            amount,
            created_at: c.created_at.clone(),
        });
    }

    let total: f64 = planned.iter().map(|p| p.amount).sum();
    let mut report = format!(
        "🔍 Reconcile — dry run\n\nChecked: {}\nTo credit: {}\nSkipped: {}\n",
        candidates.len() - unchecked,
        planned.len(),
        skipped.len()
    );
    if unchecked > 0 {
        report.push_str(&format!(
            "⏳ Not checked (rate limited): {} — this run is INCOMPLETE, re-run to finish\n",
            unchecked
        ));
    }

    if !planned.is_empty() {
        report.push_str("\n── Would credit ──\n");
        for p in &planned {
            report.push_str(&format!(
                "• {} → TG:{}  ${:.2}  ({})\n",
                p.payment_id, p.telegram_user_id, p.amount, p.created_at
            ));
        }
        report.push_str(&format!("\n💰 Total: ${:.2}\n", total));
    }
    if !skipped.is_empty() {
        report.push_str("\n── Skipped ──\n");
        for s in skipped.iter().take(25) {
            report.push_str(s);
            report.push('\n');
        }
    }

    if planned.is_empty() {
        report.push_str("\nNothing to credit.");
        send_report(bot, chat_id, &report, back_keyboard()).await;
        return;
    }

    PENDING_RECONCILE.lock().await.insert(admin_id, planned);

    let kb = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("✅ Credit all", buttons::CB_ADMIN_RECONCILE_CONFIRM),
        InlineKeyboardButton::callback("❌ Cancel", buttons::CB_ADMIN_RECONCILE_CANCEL),
    ]]);
    send_report(bot, chat_id, &report, kb).await;
}

/// Telegram caps a message at 4096 characters and every send here is `let _ = ...`, so
/// an over-long report is rejected with a 400 that nobody sees.
///
/// That is not cosmetic for this screen: the dry run has already stored the plan in
/// `PENDING_RECONCILE` by the time it reports, so losing the message leaves the admin
/// holding a live pending reconcile with no buttons to confirm or cancel it. One line
/// per planned credit at ~65 chars means roughly 63 candidates is enough, and both
/// retirement paths now feed `reconcile_candidates`, so the list only grows.
///
/// The keyboard rides on the final chunk so the buttons always sit under the end of the
/// report.
async fn send_report(bot: &Bot, chat_id: ChatId, report: &str, kb: InlineKeyboardMarkup) {
    let chunks = split_message(report, 4000);
    let last = chunks.len().saturating_sub(1);
    for (i, chunk) in chunks.iter().enumerate() {
        if i == last {
            let _ = bot
                .send_message(chat_id, *chunk)
                .reply_markup(kb.clone())
                .await;
        } else {
            let _ = bot.send_message(chat_id, *chunk).await;
        }
    }
}

/// Apply the dry run. Safe to re-run: crediting is keyed on payment_id, so anything
/// already applied comes back as "already processed" and is counted, not double-paid.
pub async fn handle_reconcile_confirm_callback(
    bot: &Bot,
    db: &DatabaseConnection,
    cq: &CallbackQuery,
) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    let admin_id = cq.from.id.0 as i64;
    let Some(msg) = &cq.message else { return };
    let chat_id = msg.chat().id;

    let Some(planned) = PENDING_RECONCILE.lock().await.remove(&admin_id) else {
        let _ = bot
            .send_message(
                chat_id,
                "⚠️ No pending reconcile found — run the dry run again.",
            )
            .await;
        return;
    };

    let (mut credited, mut already, mut failed) = (0usize, 0usize, 0usize);
    let mut total = 0.0;
    // Reported back with the ids: an unresolved failure is re-listed by the next dry
    // run forever, so a permanently broken candidate (a deleted user row, say) would
    // otherwise reappear every time with nothing on screen saying which one it is.
    let mut failures: Vec<String> = Vec::new();

    for p in &planned {
        let description = format!("Payment top-up (ID: {})", p.payment_id);
        match top_up_balance(
            db,
            p.telegram_user_id,
            p.amount,
            &description,
            &p.payment_id,
        )
        .await
        {
            Ok(_) => {
                credited += 1;
                total += p.amount;
                let _ = resolve_candidate(db, &p.payment_id, "credited").await;

                let lang = get_user_language(db, p.telegram_user_id)
                    .await
                    .unwrap_or_default();
                let text = tf(
                    "balance_topped_up",
                    lang,
                    &[
                        ("amount", &escape_markdown_v2(&format!("{:.2}", p.amount))),
                        ("balance", &escape_markdown_v2("—")),
                        ("payment_id", &p.payment_id),
                    ],
                );
                if let Err(e) = bot
                    .send_message(ChatId(p.telegram_user_id), text)
                    .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                    .await
                {
                    eprintln!(
                        "Reconcile: failed to notify TG:{}: {}",
                        p.telegram_user_id, e
                    );
                }
            }
            // The money is already in the ledger — that IS a resolution, so stamp it.
            Err(e) if is_already_credited(&e) => {
                already += 1;
                println!("Reconcile: {} was already credited", p.payment_id);
                let _ = resolve_candidate(db, &p.payment_id, "already credited").await;
            }
            Err(e) => {
                failed += 1;
                failures.push(format!(
                    "• {} → TG:{} — {}",
                    p.payment_id, p.telegram_user_id, e
                ));
                eprintln!("Reconcile: failed to credit {}: {}", p.payment_id, e);
                // Deliberately NOT resolved. `resolve_candidate` stamps `resolved_at`,
                // and `get_unresolved_candidates` filters on `resolved_at IS NULL` — so
                // stamping a transient failure (pool timeout, deadlock, brief outage)
                // dropped the payment from every future dry run and silently wrote off
                // money we owe. Leaving it NULL means the next run re-lists it, and
                // re-running is safe because crediting is keyed on payment_id.
            }
        }
    }

    let mut report = format!(
        "✅ Reconcile complete\n\nCredited: {} (${:.2})\nAlready credited: {}\n\
         Failed: {} (left unresolved — re-run to retry)",
        credited, total, already, failed
    );
    if !failures.is_empty() {
        report.push_str("\n\n── Failed ──\n");
        for f in failures.iter().take(25) {
            report.push_str(f);
            report.push('\n');
        }
        if failures.len() > 25 {
            report.push_str(&format!("… and {} more\n", failures.len() - 25));
        }
    }

    send_report(bot, chat_id, &report, back_keyboard()).await;
}

pub async fn handle_reconcile_cancel_callback(bot: &Bot, cq: &CallbackQuery) {
    let _ = bot.answer_callback_query(cq.id.clone()).await;
    clear_pending(cq.from.id.0 as i64).await;

    if let Some(msg) = &cq.message {
        let _ = bot
            .send_message(
                msg.chat().id,
                "❌ Reconcile cancelled — nothing was credited.",
            )
            .reply_markup(back_keyboard())
            .await;
    }
}

fn back_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "⬅️ Back",
        buttons::CB_ADMIN_BACK,
    )]])
}
