use reqwest::Client;
use serde_json::{json, Value};
use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::utils::build_http_client;

pub struct NowPaymentsClient {
    api_key: String,
    base_url: String,
    client: Client,
}

impl NowPaymentsClient {
    /// Create a new client with the given API key.
    pub fn new(api_key: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            base_url: "https://api.nowpayments.io/v1".to_string(),
            client: build_http_client(),
        }
    }

    /// Create from project config (reads NOWPAYMENTS_API_KEY)
    pub fn new_from_config(cfg: &Config) -> Self {
        Self::new(&cfg.nowpayments_api_key)
    }

    /// Create a payment. Returns the response JSON on success.
    pub async fn create_payment(
        &self,
        price_amount: f64,
        price_currency: &str,
        pay_currency: &str,
        telegram_user_id: i64,
        description: &str,
    ) -> Result<Value, Box<dyn Error + Send + Sync>> {
        let url = format!("{}/payment", self.base_url.trim_end_matches('/'));

        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let order_id = format!("tg_{}_{}", telegram_user_id, now);

        let payload = json!({
            "price_amount": price_amount,
            "price_currency": price_currency,
            "pay_currency": pay_currency,
            "order_id": order_id,
            "order_description": description,
        });

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await?;

        // Read body first, then inspect status and parse JSON
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            eprintln!("Failed to create payment: {} - {}", status, body);
            return Err(format!("Failed to create payment: {}", status).into());
        }

        let json: Value = serde_json::from_str(&body)?;

        // Basic validation similar to Python: require payment_id and pay_address
        if json.get("payment_id").is_none() || json.get("pay_address").is_none() {
            return Err("Invalid response from NowPayments".into());
        }

        println!(
            "Payment created: ID={:?}, Address={:?}",
            json.get("payment_id"),
            json.get("pay_address")
        );
        Ok(json)
    }

    /// Get payment status by id.
    ///
    /// A 429 gets its own variant because the poller must NOT treat it as a lookup
    /// failure: doing so would let a rate-limit storm trip the force-expire fallback
    /// and kill invoices that were merely un-checkable for a minute.
    pub async fn get_payment_status(&self, payment_id: &str) -> Result<Value, StatusError> {
        let url = format!(
            "{}/payment/{}",
            self.base_url.trim_end_matches('/'),
            payment_id
        );

        let resp = self
            .client
            .get(&url)
            .header("x-api-key", &self.api_key)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    StatusError::Timeout
                } else {
                    StatusError::Network(e.to_string())
                }
            })?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(StatusError::RateLimited {
                retry_after: parse_retry_after(resp.headers()),
            });
        }

        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            eprintln!("Failed to get payment status: {} - {}", status, body);
            return Err(StatusError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let json: Value = serde_json::from_str(&body).map_err(|e| StatusError::Api {
            status: status.as_u16(),
            body: e.to_string(),
        })?;
        println!(
            "Payment {} status: {:?}",
            payment_id,
            json.get("payment_status")
        );
        Ok(json)
    }
}

/// Why a payment-status lookup didn't produce a status.
#[derive(Debug)]
pub enum StatusError {
    /// 429. Carries the provider's own `Retry-After` when it sent one.
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    /// Any other non-2xx, or an unparseable body.
    Api { status: u16, body: String },
    /// Transport-level failure — DNS, connection refused, TLS, body read.
    Network(String),
    /// Exceeded the client timeout.
    Timeout,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StatusError::RateLimited {
                retry_after: Some(d),
            } => {
                write!(f, "rate limited (retry after {:?})", d)
            }
            StatusError::RateLimited { retry_after: None } => write!(f, "rate limited"),
            StatusError::Api { status, body } => write!(f, "api error {}: {}", status, body),
            StatusError::Network(m) => write!(f, "network error: {}", m),
            StatusError::Timeout => write!(f, "request timed out"),
        }
    }
}

impl Error for StatusError {}

/// Read `Retry-After` as delta-seconds. The HTTP-date form is not handled — NOWPayments
/// sends seconds when it sends the header at all, and a missing value simply falls back
/// to exponential backoff.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn smoke_nowpayments_build() {
        // Use cached config which expects NOWPAYMENTS_API_KEY set
        let cfg = crate::config::get_config();
        let client = NowPaymentsClient::new_from_config(cfg);
        assert!(!client.api_key.is_empty());
    }
}
