use dotenv::dotenv;
use std::env;
use std::sync::OnceLock;

/// Global cached config instance
static CONFIG: OnceLock<Config> = OnceLock::new();

/// Get the global cached config. Initializes on first call.
pub fn get_config() -> &'static Config {
    CONFIG.get_or_init(Config::from_env)
}

pub struct Config {
    pub telegram_bot_api: String,
    pub database_url: String,
    pub marzban_host: String,
    pub marzban_username: String,
    pub marzban_password: String,
    pub nowpayments_api_key: String,
    // Payment settings
    pub payment_price_currency: String,
    pub payment_pay_currency: String,
    pub payment_description: String,
    // Admin settings
    pub admin_telegram_id: Option<i64>,
    // Support settings
    pub support_chat_id: Option<i64>,
    // Teloxide proxy (routes Telegram API traffic through a SOCKS5/HTTP proxy)
    pub teloxide_proxy: Option<String>,
}

impl Config {
    pub fn from_env() -> Self {
        dotenv().ok();

        Self {
            telegram_bot_api: env::var("TELEGRAM_BOT_API").expect("TELEGRAM_BOT_API must be set"),
            database_url: env::var("DATABASE_URL").expect("DATABASE_URL must be set"),
            marzban_host: env::var("MARZBAN_HOST").expect("MARZBAN_HOST must be set"),
            marzban_username: env::var("MARZBAN_USERNAME").expect("MARZBAN_USERNAME must be set"),
            marzban_password: env::var("MARZBAN_PASSWORD").expect("MARZBAN_PASSWORD must be set"),
            nowpayments_api_key: env::var("NOWPAYMENTS_API_KEY")
                .expect("NOWPAYMENTS_API_KEY must be set"),
            // Payment settings
            payment_price_currency: env::var("PRICE_CURRENCY")
                .unwrap_or_else(|_| "USD".to_string()),
            payment_pay_currency: env::var("PAY_CURRENCY")
                .unwrap_or_else(|_| "USDTBSC".to_string()),
            payment_description: env::var("DESCRIPTION")
                .unwrap_or_else(|_| "VPN subscription top-up".to_string()),
            // Admin settings
            admin_telegram_id: env::var("ADMIN_TELEGRAM_ID")
                .ok()
                .and_then(|s| s.parse().ok()),
            // Support settings
            support_chat_id: env::var("SUPPORT_CHAT_ID")
                .ok()
                .and_then(|s| s.parse().ok()),
            teloxide_proxy: env::var("TELOXIDE_PROXY").ok(),
        }
    }
}
