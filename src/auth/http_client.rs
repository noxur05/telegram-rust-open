use reqwest::{
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
    Client, Method,
};
use serde_json::Value;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::auth::auth_client::AuthClient;

/// Response container similar to the Python dict {status, data}
#[derive(Debug)]
pub struct ResponseData {
    pub status: u16,
    pub data: Value,
}

/// Authenticated HTTP client which automatically attaches Bearer token and
/// retries once on 401 after forcing a token refresh.
pub struct AuthenticatedHttpClient {
    base_url: Option<String>,
    client: Client,
    auth: Arc<AuthClient>,
    refresh_lock: Mutex<()>,
}

impl AuthenticatedHttpClient {
    /// Create from explicit base_url, timeout (seconds) and an existing AuthClient.
    pub fn new(base_url: Option<String>, timeout_secs: u64, auth: Arc<AuthClient>) -> Self {
        let client = Client::builder()
            .connect_timeout(crate::utils::HTTP_CONNECT_TIMEOUT)
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .unwrap_or_else(|_| crate::utils::build_http_client());

        Self {
            base_url,
            client,
            auth,
            refresh_lock: Mutex::new(()),
        }
    }

    fn build_full_url(&self, url: &str) -> String {
        if url.starts_with("http") {
            url.to_string()
        } else if let Some(base) = &self.base_url {
            format!("{}{}", base.trim_end_matches('/'), url)
        } else {
            url.to_string()
        }
    }

    async fn make_request(
        &self,
        method: Method,
        url: &str,
        headers: Option<HeaderMap>,
        body_json: Option<Value>,
        retry_on_401: bool,
    ) -> Result<ResponseData, Box<dyn Error + Send + Sync>> {
        let full_url = self.build_full_url(url);

        // Obtain token
        let token = self.auth.get_token().await?;

        let mut hdrs = headers.clone().unwrap_or_else(HeaderMap::new);
        hdrs.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );

        let mut req = self
            .client
            .request(method.clone(), &full_url)
            .headers(hdrs.clone());

        if let Some(json) = &body_json {
            req = req.json(json);
        }

        let mut resp = req.send().await?;

        if resp.status().as_u16() == 401 && retry_on_401 {
            let failed_token = token.clone();
            let _g = self.refresh_lock.lock().await;

            // Check if another task already refreshed the token while we waited
            let new_token = match self.auth.get_token().await {
                Ok(t) if t != failed_token => t,
                _ => {
                    // Token is still the same (or expired), force refresh
                    eprintln!(
                        "401 received, refreshing token for: {} {}",
                        method, &full_url
                    );
                    self.auth.refresh_token().await?
                }
            };

            let mut new_hdrs = headers.clone().unwrap_or_else(HeaderMap::new);
            new_hdrs.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", new_token))?,
            );

            let mut retry_req = self.client.request(method, &full_url).headers(new_hdrs);
            if let Some(json) = &body_json {
                retry_req = retry_req.json(json);
            }
            resp = retry_req.send().await?;
        }

        // Read the response body once, then try to parse JSON from it.
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        let data = serde_json::from_str::<Value>(&body).unwrap_or(Value::String(body));

        Ok(ResponseData { status, data })
    }

    /// GET request convenience
    pub async fn get(
        &self,
        url: &str,
        headers: Option<HeaderMap>,
    ) -> Result<ResponseData, Box<dyn Error + Send + Sync>> {
        self.make_request(Method::GET, url, headers, None, true)
            .await
    }

    /// POST request convenience (JSON body)
    pub async fn post_json(
        &self,
        url: &str,
        json: Value,
        headers: Option<HeaderMap>,
    ) -> Result<ResponseData, Box<dyn Error + Send + Sync>> {
        self.make_request(Method::POST, url, headers, Some(json), true)
            .await
    }

    /// PUT request convenience (JSON body). Used by the extend flow to modify an
    /// existing Marzban account (`PUT /api/user/{username}`).
    pub async fn put_json(
        &self,
        url: &str,
        json: Value,
        headers: Option<HeaderMap>,
    ) -> Result<ResponseData, Box<dyn Error + Send + Sync>> {
        self.make_request(Method::PUT, url, headers, Some(json), true)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn smoke_client_build() {
        let auth = Arc::new(AuthClient::new("https://example.com"));
        let client = AuthenticatedHttpClient::new(Some("https://example.com".to_string()), 5, auth);
        assert!(client.base_url.is_some());
    }
}
