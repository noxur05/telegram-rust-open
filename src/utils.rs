use std::time::Duration;

/// Short — fail fast on network blackholes rather than parking a task on a
/// socket that will never open.
pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Generous — the Marzban panel can be slow under load, and a slow answer is
/// still an answer.
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Build a `reqwest::Client` with sane timeouts so a hung upstream can't park a
/// Tokio task indefinitely.
///
/// `reqwest::Client::new()` has NO request timeout by default. That default is
/// what made the Marzban token fetch dangerous: `AuthClient::fetch_token` runs
/// while holding `refresh_lock`, so a single stuck POST to `/api/admin/token`
/// blocks *every* task waiting for a token — buy, my-subscriptions — with no
/// error and no log line until the service is restarted.
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Escape special characters for MarkdownV2
pub fn escape_markdown_v2(text: &str) -> String {
    let special_chars = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut escaped = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if special_chars.contains(&c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Escape a value for interpolation **inside** a MarkdownV2 code span.
///
/// Not the same job as `escape_markdown_v2`. Inside `` `…` `` Telegram treats only a
/// backtick and a backslash as special, so escaping the full special-character set
/// there would render literal backslashes to the user — but escaping *nothing*, which
/// is what the subscription listing did, lets a single backtick in a panel-issued URL
/// close the span early and reject the whole message with a 400 that every send site
/// discards.
pub fn escape_markdown_v2_code(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c == '`' || c == '\\' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}
