// Callback data constants (these should remain the same for functionality)
pub const CB_BALANCE: &str = "balance";
pub const CB_BUY_SUBSCRIPTION: &str = "buy_subscription";
pub const CB_MY_SUBSCRIPTIONS: &str = "my_subscriptions";
pub const CB_HELP: &str = "help";
pub const CB_MAIN_MENU: &str = "main_menu";
pub const CB_TOP_UP: &str = "top_up";

// Help section callback constants
pub const CB_HELP_PRICING: &str = "help_pricing";
pub const CB_HELP_TOPUP: &str = "help_topup";
pub const CB_HELP_HOWTOBUY: &str = "help_howtobuy";
pub const CB_HELP_COMMANDS: &str = "help_commands";
pub const CB_HELP_SUPPORT: &str = "help_support";

// Top-up amount selection callback constants
pub const CB_TOPUP_5: &str = "topup_5";
pub const CB_TOPUP_10: &str = "topup_10";
pub const CB_TOPUP_15: &str = "topup_15";
pub const CB_TOPUP_CUSTOM: &str = "topup_custom";

// Language selection callback constants
pub const CB_LANG_TK: &str = "lang_tk";
pub const CB_LANG_RU: &str = "lang_ru";
pub const CB_LANG_EN: &str = "lang_en";

// Buy subscription plan selection prefix — e.g. "buy_sub:5"
pub const CB_BUY_SUB_PREFIX: &str = "buy_sub:";

// Extend flow — stateless, the account and plan ride in the callback data.
// Telegram caps callback_data at 64 bytes; the longest realistic payload is
// "xsp:tg_1234567890_1712345678:12" = 31 bytes.
// NOTE: `xsp:`/`xsc:` MUST be matched before `xs:` in the router.
pub const CB_EXT_PICK_PREFIX: &str = "xs:"; // xs:{username}          — choose a plan
pub const CB_EXT_PLAN_PREFIX: &str = "xsp:"; // xsp:{username}:{plan} — confirm screen
pub const CB_EXT_CONFIRM_PREFIX: &str = "xsc:"; // xsc:{username}:{plan} — execute

// Choosing a username for a new subscription.
// `bna:{plan_id}`            — generate one automatically
// `bns:{plan_id}:{username}` — accept the suggested name after a collision
// Longest realistic payload: "bns:9999:" + 32-char name = 41 bytes of the 64 available.
pub const CB_BUY_NAME_AUTO_PREFIX: &str = "bna:";
pub const CB_BUY_NAME_USE_PREFIX: &str = "bns:";
