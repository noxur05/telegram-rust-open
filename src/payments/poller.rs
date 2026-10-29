//! Watches every open invoice until it settles or expires, and credits it.
//!
//! This is the ONLY path by which a balance grows — the manual `/check` command and
//! its inline button were removed when this landed, so there is exactly one writer.
//!
//! Shape borrowed from `gifthub-rust-payments` (see
//! `gifthub/docs/payment-reconciliation-design.md`): the poller holds no durable state
//! of its own and rebuilds its worklist from `pending_payments` on every tick, so a
//! restart loses nothing and anything already resolved simply isn't in the next query.
//!
//! Ordering rules that matter:
//!   * A tick runs to completion **sequentially** before sleeping, so two ticks can
//!     never work the same invoice at once.
//!   * A 429 is not a lookup failure. Reporting it as one would let a rate-limit storm
//!     trip the force-expire fallback and kill invoices that were merely unreachable.
//!   * An unrecognised status never credits. Money only moves for statuses we know.

use std::time::Duration;

use sea_orm::{DatabaseConnection, DbErr};
use teloxide::prelude::*;
use teloxide::types::ChatId;

use crate::auth::nowpayments::{NowPaymentsClient, StatusError};
use crate::config::get_config;
use crate::db::{
    archive_and_remove_pending_payment, get_pending_payments, get_user_language,
    mark_payment_detected, remove_pending_payment_by_id, top_up_balance, PendingPayment,
};
use crate::i18n::{t, tf};
use crate::utils::escape_markdown_v2;

use super::ratelimit::RateLimiter;

/// How often the worklist is rebuilt and re-checked.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// What the provider's status means for us.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    /// Terminal and paid — credit this much USD.
    Credit(f64),
    /// Funds seen but not final. Tell the user once, keep watching.
    Detected,
    /// Nothing has happened yet. Stay quiet, keep watching.
    Pending,
    /// Terminal and unpaid. Tell the user and stop watching.
    Dead(&'static str),
}

/// Map a NOWPayments status onto an action.
///
/// Pure so the whole decision table is testable without a network. The statuses are
/// the nine documented in NOWPayments' create_payment reference; `confirmed` in
/// particular used to fall through to "unknown status" in the old manual check.
pub(crate) fn classify(
    status: &str,
    pay_amount: f64,
    actually_paid: f64,
    price_amount: f64,
) -> Outcome {
    match status {
        "finished" => Outcome::Credit(price_amount),
        // The user underpaid. Credit exactly the proportion that arrived — never the
        // full invoice. Guarded against a zero/absent pay_amount so this can't divide
        // by zero and hand out an infinite or NaN balance.
        "partially_paid" => {
            if pay_amount > 0.0 && actually_paid > 0.0 {
                Outcome::Credit((actually_paid / pay_amount) * price_amount)
            } else {
                // `partially_paid` means money DID arrive; we just can't work out how
                // much because the provider sent no usable amounts. This is our own
                // divide-by-zero guard firing, NOT a provider verdict, so it must not
                // masquerade as one: `Dead("payment_failed")` told the user their
                // payment failed and deleted the invoice with nothing archived.
                //
                // `Pending` instead. `looked_for` maps it to `Unknown` (the status is
                // not "waiting"), so it is never force-expired, survives to the 30-day
                // reaper, and is archived atomically into `reconcile_candidates` where
                // an admin will see it. Never `Credit` — the amount is unknown.
                eprintln!(
                    "CRITICAL: partially_paid with unusable amounts \
                     (pay_amount={pay_amount}, actually_paid={actually_paid}) — \
                     money arrived but cannot be sized; leaving the invoice open"
                );
                Outcome::Pending
            }
        }
        "confirming" | "confirmed" | "sending" => Outcome::Detected,
        "waiting" => Outcome::Pending,
        "expired" => Outcome::Dead("payment_expired"),
        "refunded" => Outcome::Dead("payment_refunded"),
        "failed" => Outcome::Dead("payment_failed"),
        // Deliberately NOT terminal and deliberately never a credit: an unrecognised
        // status is a provider change we haven't reviewed, so keep watching and let a
        // human notice the log line.
        other => {
            eprintln!(
                "⚠️ Unrecognised NOWPayments status {:?} — leaving invoice open",
                other
            );
            Outcome::Pending
        }
    }
}

/// Whether this error means the payment was already credited by an earlier attempt.
///
/// Two shapes reach us: `top_up_balance`'s own "already processed" (its CTE saw an
/// existing ledger row), and — once migration 008 is applied — a unique-violation from
/// the partial index on `transactions(payment_id)` when two writers race. Both mean
/// the money is already in, so both are success, not failure.
///
/// Matched on the CONSTRAINT NAME, never on the generic "duplicate key" / "unique
/// constraint" prefix. Postgres emits that same prefix for every unique index on
/// `transactions`, including `transactions_pkey` — and a primary-key collision means a
/// desynced `transactions_id_seq` (exactly what a dump restore without `setval` leaves
/// behind; `migrations/010` records that this database was seeded from such a dump),
/// where the whole CTE rolls back and **no money moved at all**. Reading that as
/// "already credited" told the user their balance was topped up, deleted the
/// `pending_payments` row, and lost the payment.
///
/// The narrow direction is the safe one: a MISSED already-credited error costs one
/// retry, and the partial unique index still prevents a double credit. A FALSE one
/// writes off real money.
pub(crate) fn is_already_credited(e: &DbErr) -> bool {
    let msg = e.to_string().to_lowercase();
    msg.contains("already processed") || msg.contains("transactions_payment_id_unique")
}

/// Extract a numeric field that the provider may send as either a number or a string.
fn num(v: &serde_json::Value, key: &str) -> f64 {
    v.get(key)
        .and_then(|x| {
            x.as_f64()
                .or_else(|| x.as_str().and_then(|s| s.parse::<f64>().ok()))
        })
        .unwrap_or(0.0)
}

/// Order ids are minted as `tg_{telegram_id}_{millis}` when the invoice is created.
///
/// Carried over from the removed manual check, where it was the authorization gate.
/// Here the user is already known from our own row, so this is a consistency
/// assertion: a mismatch means the invoice isn't the one we think it is, and we must
/// not credit anyone on that basis.
fn order_id_matches(order_id: &str, telegram_user_id: i64) -> bool {
    order_id.starts_with(&format!("tg_{}_", telegram_user_id))
}

/// Send a message to a user's private chat, logging rather than propagating failures —
/// a blocked bot must never stop us from crediting or from moving to the next invoice.
async fn notify(
    bot: &Bot,
    db: &DatabaseConnection,
    telegram_user_id: i64,
    render: impl FnOnce(crate::i18n::Lang) -> String,
) {
    let lang = get_user_language(db, telegram_user_id)
        .await
        .unwrap_or_default();
    let text = render(lang);
    if let Err(e) = bot
        .send_message(ChatId(telegram_user_id), text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await
    {
        eprintln!("Failed to notify TG:{}: {}", telegram_user_id, e);
    }
}

/// What one look at an invoice told us, for the purpose of the deadline fallback.
///
/// The distinction that matters is **not** "did we act" but "do we know". Force-expiry
/// deletes the `pending_payments` row, and an uncredited invoice writes no
/// `transactions` row, so the delete erases the only local record the invoice ever
/// existed. That may only happen when the provider positively told us nothing has
/// arrived — never when we simply failed to find out.
#[derive(Debug, PartialEq)]
pub(crate) enum Looked {
    /// The provider reported a real state: paid, in flight, or dead. Never force-expire.
    Reported,
    /// The provider says nothing has arrived (`waiting`). The deadline may retire it.
    Silent,
    /// We could not tell — unrecognised or missing status, a mismatched order_id, or a
    /// failed lookup. Never force-expire; the 30-day reaper in `main.rs` is the backstop
    /// for an invoice the provider will genuinely never resolve.
    Unknown,
}

/// What a status alone tells us, before any side effects.
///
/// `settle` returns exactly this, so the deadline gate in `run_payment_poller` and this
/// table cannot drift apart. Split out as a pure fn because `settle` needs a `Bot` and a
/// live `DatabaseConnection` and so cannot be unit-tested directly.
pub(crate) fn looked_for(status: &str, outcome: &Outcome) -> Looked {
    match outcome {
        // `classify` folds two very different things into `Pending`: a provider that
        // said `waiting`, and a status (or a missing `payment_status` field, which
        // `settle` reads as the literal "unknown") that we don't recognise. Only the
        // first is evidence that nothing arrived.
        Outcome::Pending if status == "waiting" => Looked::Silent,
        Outcome::Pending => Looked::Unknown,
        Outcome::Credit(_) | Outcome::Detected | Outcome::Dead(_) => Looked::Reported,
    }
}

/// Handle one invoice's observed status.
async fn settle(
    bot: &Bot,
    db: &DatabaseConnection,
    row: &PendingPayment,
    raw: &serde_json::Value,
) -> Looked {
    let status = raw
        .get("payment_status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let order_id = raw.get("order_id").and_then(|v| v.as_str()).unwrap_or("");

    if !order_id.is_empty() && !order_id_matches(order_id, row.telegram_user_id) {
        eprintln!(
            "🚨 Refusing to credit {}: order_id {:?} does not belong to TG:{}",
            row.payment_id, order_id, row.telegram_user_id
        );
        // Unknown, not Silent: this invoice may be perfectly real and settled — we just
        // can't match it to this row. Force-expiring would delete our only record of it.
        return Looked::Unknown;
    }

    let outcome = classify(
        status,
        num(raw, "pay_amount"),
        num(raw, "actually_paid"),
        num(raw, "price_amount"),
    );

    // Decided up front from the status alone; the arms below only run side effects, and
    // every early return hands back this same value — failing to write our own
    // bookkeeping never changes what the PROVIDER told us.
    let looked = looked_for(status, &outcome);

    match outcome {
        Outcome::Pending => {}

        Outcome::Detected => {
            if !row.already_detected {
                if let Err(e) = mark_payment_detected(db, &row.payment_id).await {
                    eprintln!("Failed to stamp detected_at for {}: {}", row.payment_id, e);
                    // Don't notify if we couldn't record it — we'd repeat every tick.
                    // Still `Reported`: funds ARE in flight, so this invoice must not be
                    // force-expired just because our own bookkeeping write failed.
                    return looked;
                }
                notify(bot, db, row.telegram_user_id, |lang| {
                    t("payment_detected", lang).to_string()
                })
                .await;
            }
        }

        Outcome::Credit(amount) => {
            let description = format!("Payment top-up (ID: {})", row.payment_id);
            let credited = match top_up_balance(
                db,
                row.telegram_user_id,
                amount,
                &description,
                &row.payment_id,
            )
            .await
            {
                Ok(new_balance) => Some(new_balance.to_string().parse::<f64>().unwrap_or(0.0)),
                Err(e) if is_already_credited(&e) => {
                    // A previous tick credited it and then crashed before deleting the
                    // row. The money is in; fall through to cleanup. We still notify —
                    // a duplicate message is harmless, a silently missing one is not.
                    println!(
                        "Payment {} was already credited; cleaning up",
                        row.payment_id
                    );
                    None
                }
                Err(e) => {
                    eprintln!(
                        "CRITICAL: failed to credit {} for TG:{}: {}",
                        row.payment_id, row.telegram_user_id, e
                    );
                    // Leave the row — the next tick retries, and `looked` is `Reported`
                    // so an overdue invoice that is genuinely PAID is never force-expired
                    // out of the worklist just because our credit write failed.
                    return looked;
                }
            };

            println!(
                "💰 Credited ${:.2} to TG:{} for payment {}",
                amount, row.telegram_user_id, row.payment_id
            );

            notify(bot, db, row.telegram_user_id, |lang| {
                tf(
                    "balance_topped_up",
                    lang,
                    &[
                        ("amount", &escape_markdown_v2(&format!("{:.2}", amount))),
                        (
                            "balance",
                            &escape_markdown_v2(&match credited {
                                Some(b) => format!("{:.2}", b),
                                None => "—".to_string(),
                            }),
                        ),
                        ("payment_id", &row.payment_id),
                    ],
                )
            })
            .await;

            remove(db, &row.payment_id).await;
        }

        Outcome::Dead(key) => {
            println!("Payment {} ended as {}", row.payment_id, status);
            notify(bot, db, row.telegram_user_id, |lang| {
                t(key, lang).to_string()
            })
            .await;
            remove(db, &row.payment_id).await;
        }
    }

    looked
}

async fn remove(db: &DatabaseConnection, payment_id: &str) {
    if let Err(e) = remove_pending_payment_by_id(db, payment_id).await {
        eprintln!("Failed to remove pending payment {}: {}", payment_id, e);
    }
}

/// Poll every open invoice forever.
pub async fn run_payment_poller(bot: Bot, db: DatabaseConnection) {
    let client = NowPaymentsClient::new_from_config(get_config());
    let mut limiter = RateLimiter::new();

    println!(
        "💳 Payment poller started (every {}s)",
        POLL_INTERVAL.as_secs()
    );

    loop {
        let rows = match get_pending_payments(&db).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Payment poller: failed to load worklist: {}", e);
                tokio::time::sleep(POLL_INTERVAL).await;
                continue;
            }
        };

        for row in &rows {
            if !limiter.is_ready() {
                break; // still backing off — the whole worklist waits for the next tick
            }

            // What this look told us. `row.is_overdue` is a snapshot column computed
            // back in the worklist query and `settle` cannot change it, so without this
            // the force-expire below fired even on invoices that had just been settled:
            // a `finished` one got credited AND told "payment expired", and — the money
            // bug — an invoice still `confirming` past the 24h deadline was announced as
            // expired and had its row DELETED, so when it later settled there was no
            // worklist row left and it was never credited.
            let looked = match client.get_payment_status(&row.payment_id).await {
                Ok(raw) => {
                    limiter.record_success();
                    settle(&bot, &db, row, &raw).await
                }
                Err(StatusError::RateLimited { retry_after }) => {
                    limiter.record_429(retry_after);
                    break;
                }
                Err(e) => {
                    // A failed lookup is not a verdict, and it is not evidence that
                    // nothing arrived either — so it must not retire the invoice. A
                    // provider outage lasting longer than the deadline would otherwise
                    // mass-expire every overdue invoice at once. The 30-day reaper in
                    // `main.rs` is what eventually clears an id that never resolves.
                    eprintln!("Payment poller: {} lookup failed: {}", row.payment_id, e);
                    Looked::Unknown
                }
            };

            // Past its deadline and the provider positively reports that nothing has
            // arrived: it is never going to resolve. Retire it locally rather than
            // polling it forever (gifthub's force-close, same reasoning).
            if looked == Looked::Silent && row.is_overdue {
                println!(
                    "Payment {} passed its deadline — force-expiring",
                    row.payment_id
                );
                // Archive and delete atomically, and tell the user only once that has
                // succeeded. An uncredited invoice writes no `transactions` row, so a
                // bare DELETE would erase the only local record it ever existed — and
                // `reconcile_candidates` was itself seeded from a dump of this table
                // precisely because that had already happened once. On failure the row
                // simply stays on the worklist and the next tick retries.
                match archive_and_remove_pending_payment(&db, &row.payment_id).await {
                    Ok(()) => {
                        notify(&bot, &db, row.telegram_user_id, |lang| {
                            t("payment_expired", lang).to_string()
                        })
                        .await;
                    }
                    Err(e) => eprintln!(
                        "CRITICAL: could not retire {}: {} — left on the worklist",
                        row.payment_id, e
                    ),
                }
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_credits_the_invoice_price() {
        assert_eq!(classify("finished", 10.0, 10.0, 5.0), Outcome::Credit(5.0));
    }

    #[test]
    fn partially_paid_credits_only_what_arrived() {
        // Half the crypto arrived -> half the USD value.
        assert_eq!(
            classify("partially_paid", 10.0, 5.0, 20.0),
            Outcome::Credit(10.0)
        );
    }

    /// Never divides by zero, and — just as important — never reports a *failure*.
    /// `partially_paid` means money arrived; unusable amounts are our own guard firing,
    /// so the invoice must stay open rather than be declared dead and deleted.
    #[test]
    fn partially_paid_with_no_amounts_never_divides_by_zero() {
        for (pay, paid) in [(0.0, 0.0), (0.0, 5.0), (5.0, 0.0)] {
            let out = classify("partially_paid", pay, paid, 20.0);
            assert_eq!(out, Outcome::Pending, "pay={pay} paid={paid}");
            assert!(!matches!(out, Outcome::Credit(_)));
        }
    }

    #[test]
    fn in_flight_statuses_announce_once_and_keep_watching() {
        for s in ["confirming", "confirmed", "sending"] {
            assert_eq!(classify(s, 1.0, 1.0, 1.0), Outcome::Detected, "{}", s);
        }
    }

    #[test]
    fn waiting_is_silent() {
        assert_eq!(classify("waiting", 1.0, 0.0, 1.0), Outcome::Pending);
    }

    #[test]
    fn unpaid_terminal_statuses_are_reported_and_retired() {
        assert_eq!(
            classify("expired", 1.0, 0.0, 1.0),
            Outcome::Dead("payment_expired")
        );
        assert_eq!(
            classify("refunded", 1.0, 1.0, 1.0),
            Outcome::Dead("payment_refunded")
        );
        assert_eq!(
            classify("failed", 1.0, 0.0, 1.0),
            Outcome::Dead("payment_failed")
        );
    }

    #[test]
    fn an_unknown_status_never_credits() {
        // The important property: a provider adding a status must not move money.
        let out = classify("some_new_status", 10.0, 10.0, 5.0);
        assert_eq!(out, Outcome::Pending);
        assert!(!matches!(out, Outcome::Credit(_)));
    }

    #[test]
    fn order_id_must_belong_to_the_row_owner() {
        assert!(order_id_matches("tg_123_1712345678", 123));
        assert!(!order_id_matches("tg_1234_1712345678", 123));
        assert!(!order_id_matches("tg_12_1712345678", 123));
        assert!(!order_id_matches("", 123));
    }

    #[test]
    fn numeric_fields_parse_from_both_json_shapes() {
        // NOWPayments sends some amounts as numbers and some as strings.
        let v = serde_json::json!({"a": 1.5, "b": "2.5", "c": null});
        assert_eq!(num(&v, "a"), 1.5);
        assert_eq!(num(&v, "b"), 2.5);
        assert_eq!(num(&v, "c"), 0.0);
        assert_eq!(num(&v, "missing"), 0.0);
    }

    /// The deadline fallback in `run_payment_poller` fires only on `Looked::Silent`, and
    /// `settle` returns exactly `looked_for(...)` — so this table IS the gate.
    ///
    /// Force-expiry DELETEs the `pending_payments` row, and an uncredited invoice has no
    /// `transactions` row either, so it destroys the only local record the invoice
    /// existed. The money case is `confirming` past the 24h deadline: funds are on-chain
    /// but not final, and retiring it there means it can never be credited.
    #[test]
    fn only_a_waiting_invoice_may_be_force_expired() {
        let cases: &[(&str, Looked)] = &[
            // The provider positively says nothing has arrived — the ONLY retirable case.
            ("waiting", Looked::Silent),
            // Real reports: never retire.
            ("finished", Looked::Reported),
            ("confirming", Looked::Reported),
            ("confirmed", Looked::Reported),
            ("sending", Looked::Reported),
            // With usable amounts this credits, so it is a report. The degenerate case
            // (amounts missing/zero) is Unknown instead — see
            // `a_degenerate_partial_payment_is_never_force_expired`.
            ("partially_paid", Looked::Reported),
            ("expired", Looked::Reported),
            ("refunded", Looked::Reported),
            ("failed", Looked::Reported),
            // We can't tell: a provider rename, or a response with no `payment_status`
            // at all (which `settle` reads as the literal "unknown"). Retiring these was
            // the bug — a single upstream rename would have expired every open invoice.
            ("some_new_status", Looked::Unknown),
            ("unknown", Looked::Unknown),
        ];
        for (status, want) in cases {
            let outcome = classify(status, 10.0, 10.0, 5.0);
            assert_eq!(&looked_for(status, &outcome), want, "status {:?}", status);
        }
    }

    /// A `partially_paid` we cannot size must be `Unknown`, never `Silent`: money
    /// arrived, so the deadline must not delete the invoice. It survives to the reaper,
    /// which archives it into `reconcile_candidates` for an admin to settle by hand.
    #[test]
    fn a_degenerate_partial_payment_is_never_force_expired() {
        let outcome = classify("partially_paid", 0.0, 0.0, 20.0);
        assert_eq!(looked_for("partially_paid", &outcome), Looked::Unknown);
        assert_ne!(looked_for("partially_paid", &outcome), Looked::Silent);
    }

    #[test]
    fn already_credited_is_recognised_in_both_forms() {
        assert!(is_already_credited(&DbErr::Custom(
            "Payment already processed".into()
        )));
        assert!(is_already_credited(&DbErr::Custom(
            "duplicate key value violates unique constraint \"transactions_payment_id_unique\""
                .into()
        )));
        assert!(!is_already_credited(&DbErr::Custom(
            "connection closed".into()
        )));
    }

    /// Postgres emits the identical "duplicate key value violates unique constraint"
    /// prefix for EVERY unique index on `transactions` — both strings below were
    /// captured from a real server. Only the constraint name separates "the money is
    /// already in" from "the whole statement rolled back and nothing happened".
    ///
    /// A `transactions_pkey` collision means `transactions_id_seq` is behind
    /// `max(id)`, which is what a dump restore without `setval` leaves behind — and
    /// `migrations/010` records that this database was seeded from exactly such a dump.
    /// Treating it as already-credited told the user their balance was topped up,
    /// deleted the `pending_payments` row, and lost the payment.
    #[test]
    fn a_primary_key_collision_is_not_an_already_credited_payment() {
        assert!(!is_already_credited(&DbErr::Custom(
            "duplicate key value violates unique constraint \"transactions_pkey\"".into()
        )));
        // Any other unique index that might later be added to the table, too.
        assert!(!is_already_credited(&DbErr::Custom(
            "duplicate key value violates unique constraint \"transactions_some_new_idx\"".into()
        )));
    }
}
