pub mod balance;
pub mod buttons;
pub mod buy;
pub mod extend;
pub mod help;
pub mod lang;
pub mod start;
pub mod subscriptions;
pub mod support;

/// Arm one text-input mode by first clearing every other.
///
/// Three flows wait for the user to type something — a top-up amount, a support message,
/// and a subscription name — and each is an independent map keyed by telegram id. The
/// router checks them in a fixed order, so arming a second while a first is still live
/// silently hands every typed message to whichever is checked earlier: tap a plan, then
/// "Top up -> Custom", then type `5`, and the `5` is read as a subscription name (too
/// short, rejected) while the top-up never happens.
///
/// A user can only be typing one thing at a time, so arming any of them clears the rest.
pub async fn clear_input_modes(telegram_id: i64) {
    balance::cancel_topup_input(telegram_id).await;
    support::cancel_support(telegram_id).await;
    buy::cancel_username_input(telegram_id).await;
}
