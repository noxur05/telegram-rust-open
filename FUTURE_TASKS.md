# Future Tasks

## Performance
- **Broadcast system improvements**: Add pagination for user fetching during broadcast (instead of loading all users at once), add progress tracking, delivery retry with backoff, and batch sending with rate limiting.

## Code Quality
- **Admin panel i18n consistency**: Admin UI strings are mixed English/Turkmen (e.g. broadcast buttons are Turkmen, rest is English). Pick one language for admin panel and make it consistent.
- **f64 to Decimal for monetary config**: `config.rs` stores `subscription_price` and `payment_price_amount` as `f64`. This can cause floating-point precision issues. Change to `rust_decimal::Decimal` and update all call sites.
- **Callback rate limit i18n**: In `main.rs` `handle_callback`, the rate limit alert message is hardcoded English (`"Too many requests. Wait {} seconds."`). Should use `tf("rate_limit", lang, ...)` like the message handler does.

## Dependencies
- **Drop `once_cell` from Cargo.lock**: The dependency was removed from Cargo.toml but may still be pulled in transitively. Run `cargo update` to clean up the lock file.
