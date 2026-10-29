use crate::config::get_config;
use crate::utils::build_http_client;
use base64::{engine::general_purpose, Engine as _};
use reqwest::Client;
use serde_json::Value;
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Async Auth client for Marzban-like token endpoint.
///
/// This mirrors the behavior of your Python `AuthClient`: it requests a token
/// from `{host}/api/admin/token` using the password grant, decodes the JWT
/// to extract the `exp` claim, and keeps the token in an internal state
/// guarded by a mutex. A separate mutex prevents concurrent refreshes.
pub struct AuthClient {
    token_url: String,
    client: Client,
    state: Mutex<AuthState>,
    refresh_lock: Mutex<()>,
}

struct AuthState {
    access_token: Option<String>,
    expire_time: u64,
}

impl AuthClient {
    /// Create a new client with explicit host (e.g. "https://marzban.example.com").
    pub fn new(host: &str) -> Self {
        let token_url = format!("{}/api/admin/token", host.trim_end_matches('/'));
        Self {
            token_url,
            // Timed out on purpose: fetch_token() runs while holding refresh_lock,
            // so an untimed hang here stalls every Marzban-backed handler.
            client: build_http_client(),
            state: Mutex::new(AuthState {
                access_token: None,
                expire_time: 0,
            }),
            refresh_lock: Mutex::new(()),
        }
    }

    /// Convenience constructor using the project's Config struct.
    pub fn new_from_config(cfg: &crate::config::Config) -> Self {
        Self::new(&cfg.marzban_host)
    }

    // Note: username/password are read from cached config when fetching a token.

    /// Internal HTTP call to fetch a fresh token and update internal state.
    pub async fn fetch_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        let cfg = get_config();
        let params = [
            ("grant_type", "password"),
            ("username", &*cfg.marzban_username),
            ("password", &*cfg.marzban_password),
            ("scope", ""),
            ("client_id", "string"),
            ("client_secret", "string"),
        ];

        let resp = self
            .client
            .post(&self.token_url)
            .form(&params)
            .send()
            .await?;

        // Inspect status first, then read body text and parse JSON from it.
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            eprintln!("Failed to fetch token: {} - {}", status, body);
            return Err(format!("Token fetch failed with status {}", status).into());
        }

        let json: Value = serde_json::from_str(&body)?;
        let access_token = json
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or("access_token missing in response")?
            .to_string();

        let exp = Self::decode_exp(&access_token)?;

        // store state
        {
            let mut st = self.state.lock().await;
            st.access_token = Some(access_token.clone());
            st.expire_time = exp;
        }

        println!("Token refreshed successfully. Expires at: {}", exp);

        Ok(access_token)
    }

    /// Decode expiration (exp) from JWT token payload.
    fn decode_exp(jwt_token: &str) -> Result<u64, Box<dyn Error + Send + Sync>> {
        let parts: Vec<&str> = jwt_token.split('.').collect();
        if parts.len() < 2 {
            return Err("invalid jwt token format".into());
        }
        let mut payload = parts[1].to_string();

        // Add padding if necessary
        let rem = payload.len() % 4;
        if rem != 0 {
            payload.push_str(&"=".repeat(4 - rem));
        }

        let decoded = general_purpose::URL_SAFE.decode(&payload)?;
        let v: Value = serde_json::from_slice(&decoded)?;
        let exp = v.get("exp").and_then(|e| e.as_u64()).unwrap_or(0);
        Ok(exp)
    }

    /// Get a valid token. Refreshes it if missing or expired. Concurrent-safe.
    pub async fn get_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();

        // Fast path: try to read state and return if valid
        {
            let st = self.state.lock().await;
            if let Some(tok) = &st.access_token {
                if now < st.expire_time {
                    return Ok(tok.clone());
                }
            }
        }

        // Acquire refresh lock so only one task refreshes the token
        let _refresh_guard = self.refresh_lock.lock().await;

        // Double-check after acquiring refresh lock
        {
            let st = self.state.lock().await;
            if let Some(tok) = &st.access_token {
                if now < st.expire_time {
                    return Ok(tok.clone());
                }
            }
        }

        // Perform fetch and return the newly acquired token
        self.fetch_token().await
    }

    /// Force refresh of token (useful when receiving 401 from API).
    pub async fn refresh_token(&self) -> Result<String, Box<dyn Error + Send + Sync>> {
        let _refresh_guard = self.refresh_lock.lock().await;
        // clear state
        {
            let mut st = self.state.lock().await;
            st.access_token = None;
            st.expire_time = 0;
        }

        self.fetch_token().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn smoke_create() {
        // We only test construction here. Real network tests should be mocked.
        let c = AuthClient::new("https://example.com");
        assert!(c.token_url.contains("example.com"));
    }
}
