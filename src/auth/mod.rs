pub mod auth_client;
pub mod http_client;
pub mod nowpayments;

use crate::config::get_config;
use auth_client::AuthClient;
use http_client::AuthenticatedHttpClient;
use std::sync::{Arc, OnceLock};

/// Global shared HTTP client for Marzban API
static MARZBAN_CLIENT: OnceLock<Arc<AuthenticatedHttpClient>> = OnceLock::new();

/// Get or initialize the global Marzban HTTP client.
/// This ensures we reuse the same client and cached token across all requests.
pub fn get_marzban_client() -> Arc<AuthenticatedHttpClient> {
    MARZBAN_CLIENT
        .get_or_init(|| {
            let cfg = get_config();
            let auth = Arc::new(AuthClient::new_from_config(cfg));
            Arc::new(AuthenticatedHttpClient::new(
                Some(cfg.marzban_host.clone()),
                30,
                auth,
            ))
        })
        .clone()
}
