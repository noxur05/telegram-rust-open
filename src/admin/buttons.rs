pub const CB_BROADCAST_CONFIRM: &str = "broadcast_confirm";
pub const CB_BROADCAST_CANCEL: &str = "broadcast_cancel";

// Admin menu callback constants
pub const CB_ADMIN_STATS: &str = "admin_stats";
pub const CB_ADMIN_BROADCAST: &str = "admin_broadcast";
pub const CB_ADMIN_USER_LOOKUP: &str = "admin_user_lookup";
pub const CB_ADMIN_BALANCE_MGMT: &str = "admin_balance_mgmt";
pub const CB_ADMIN_BACK: &str = "admin_back";
pub const CB_ADMIN_BALANCE_CONFIRM: &str = "admin_balance_confirm";
pub const CB_ADMIN_BALANCE_CANCEL: &str = "admin_balance_cancel";
pub const CB_ADMIN_USERS_LIST: &str = "admin_users_list";
pub const CB_ADMIN_TXNS_LIST: &str = "admin_txns_list";
// Pagination prefixes — actual callback data will be e.g. "au_p:0", "at_p:0"
pub const CB_ADMIN_USERS_PAGE_PREFIX: &str = "au_p:";
pub const CB_ADMIN_TXNS_PAGE_PREFIX: &str = "at_p:";
// Detail prefixes — e.g. "au_d:{telegram_id}:{page}", "at_d:{txn_id}:{page}"
pub const CB_ADMIN_USER_DETAIL_PREFIX: &str = "au_d:";
pub const CB_ADMIN_TXN_DETAIL_PREFIX: &str = "at_d:";
// User transactions pagination — "aut_p:{telegram_id}:{txn_page}:{users_page}"
pub const CB_ADMIN_USER_TXN_PAGE_PREFIX: &str = "aut_p:";
// User transaction detail — "aut_d:{txn_id}:{telegram_id}:{users_page}"
pub const CB_ADMIN_USER_TXN_DETAIL_PREFIX: &str = "aut_d:";

// Subscription management callbacks
pub const CB_ADMIN_SUBSCRIPTIONS: &str = "admin_subscriptions";
pub const CB_ADMIN_SUB_TYPE_ADD: &str = "ast_add";
pub const CB_ADMIN_SUB_TYPE_PREFIX: &str = "ast:";
pub const CB_ADMIN_SUB_TYPE_DELETE_PREFIX: &str = "astd:";
pub const CB_ADMIN_SUB_ADD_PREFIX: &str = "asa:";
pub const CB_ADMIN_SUB_DETAIL_PREFIX: &str = "asd:";
pub const CB_ADMIN_SUB_EDIT_PREFIX: &str = "ase:";
pub const CB_ADMIN_SUB_DELETE_PREFIX: &str = "asdel:";
pub const CB_ADMIN_SUB_CONFIRM: &str = "as_confirm";
pub const CB_ADMIN_SUB_CANCEL: &str = "as_cancel";

// Historical payment reconciliation (dry-run, then confirm)
pub const CB_ADMIN_RECONCILE: &str = "admin_reconcile";
pub const CB_ADMIN_RECONCILE_CONFIRM: &str = "arec_confirm";
pub const CB_ADMIN_RECONCILE_CANCEL: &str = "arec_cancel";
