//! HTTP client core: one shared send path for every REST call.
//!
//! Every request goes through [`Client::send`], which applies the shared
//! [`RateLimiter`], retries 429/5xx/network failures with capped exponential
//! backoff, honours `Retry-After`, clears auth state on 401 and never retries
//! other 4xx responses (RULES.md §3, §4).

use std::fmt;
use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use reqwest::header::{self, HeaderValue};
use reqwest::{Method, Response, StatusCode, Url};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use secrecy::SecretString;
use serde::de::DeserializeOwned;
use tokio::time::sleep;

use crate::{ApiError, CookieVault, RateLimiter, USER_AGENT};

/// Documented REST API root.
pub const API_ROOT: &str = "https://api.vrchat.cloud/api/1";

/// Cookies that carry session state and may be persisted in the vault.
const AUTH_COOKIE_NAMES: [&str; 2] = ["auth", "twoFactorAuth"];

/// Maximum number of retries after the initial attempt.
const MAX_RETRIES: u32 = 3;

/// Shared, cheaply cloneable VRChat REST client.
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    http: reqwest::Client,
    cookies: Arc<CookieStoreMutex>,
    vault: Option<Arc<dyn CookieVault>>,
    limiter: RateLimiter,
    base_url: String,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.inner.base_url)
            .field("limiter", &self.inner.limiter)
            .finish_non_exhaustive()
    }
}

/// Builder for [`Client`]. Defaults: [`API_ROOT`], no vault, 1 request/second
/// with a burst of 3 (RULES.md §3).
pub struct ClientBuilder {
    base_url: String,
    vault: Option<Arc<dyn CookieVault>>,
    requests_per_second: f64,
    burst: u32,
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("base_url", &self.base_url)
            .field("requests_per_second", &self.requests_per_second)
            .field("burst", &self.burst)
            .finish_non_exhaustive()
    }
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            base_url: API_ROOT.to_string(),
            vault: None,
            requests_per_second: 1.0,
            burst: 3,
        }
    }
}

impl ClientBuilder {
    /// Override the API root — used by tests to point at a local mock.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Install the vault where auth cookies are persisted.
    pub fn vault(mut self, vault: impl CookieVault + 'static) -> Self {
        self.vault = Some(Arc::new(vault));
        self
    }

    /// Override the token-bucket parameters.
    pub fn rate_limit(mut self, requests_per_second: f64, burst: u32) -> Self {
        self.requests_per_second = requests_per_second;
        self.burst = burst;
        self
    }

    pub fn build(self) -> Result<Client, ApiError> {
        let cookies = Arc::new(CookieStoreMutex::default());
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .cookie_provider(cookies.clone())
            .build()
            .map_err(ApiError::Network)?;
        Ok(Client {
            inner: Arc::new(Inner {
                http,
                cookies,
                vault: self.vault,
                limiter: RateLimiter::new(self.requests_per_second, self.burst),
                base_url: self.base_url.trim_end_matches('/').to_string(),
            }),
        })
    }
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// API root this client talks to.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// The `auth` cookie value, for the WebSocket pipeline (vrc-pipeline).
    /// Kept as a [`SecretString`] and never logged (RULES.md §4).
    pub fn auth_token(&self) -> Option<SecretString> {
        let url = Url::parse(&self.inner.base_url).ok()?;
        let store = self.cookie_store();
        let token = store
            .get_request_values(&url)
            .find(|(name, _)| *name == "auth")
            .map(|(_, value)| SecretString::from(value.to_string()));
        token
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{}", self.inner.base_url, path)
    }

    /// `GET path` and decode the JSON body into `T`.
    pub(crate) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let url = self.url(path);
        let resp = self.send(|http| http.request(Method::GET, &url)).await?;
        self.decode(resp).await
    }

    /// `POST path` with a JSON body, decoding the JSON response into `T`.
    pub(crate) async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, ApiError> {
        let url = self.url(path);
        let resp = self
            .send(|http| http.request(Method::POST, &url).json(body))
            .await?;
        self.decode(resp).await
    }

    /// Single send path for every request: rate limit, retry, 401 handling.
    ///
    /// `make` rebuilds the [`reqwest::RequestBuilder`] on each attempt.
    pub(crate) async fn send(
        &self,
        make: impl Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    ) -> Result<Response, ApiError> {
        let mut retries = 0;
        loop {
            self.inner.limiter.acquire().await;
            match make(&self.inner.http).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        let retry_after = retry_after(resp.headers());
                        if retries >= MAX_RETRIES {
                            return Err(ApiError::RateLimited { retry_after });
                        }
                        let wait = retry_after.unwrap_or_else(|| self.retry_delay(retries));
                        tracing::debug!("rate-limited by API, pausing requests for {wait:?}");
                        self.inner.limiter.pause(wait);
                        retries += 1;
                        continue;
                    }
                    if status == StatusCode::UNAUTHORIZED {
                        // Session or credentials rejected: drop every stored
                        // trace of them (RULES.md §4).
                        self.clear_auth_state();
                        return Err(ApiError::Unauthorized);
                    }
                    if status.is_server_error() {
                        if retries >= MAX_RETRIES {
                            let message = error_body(resp).await;
                            return Err(ApiError::Http {
                                status: status.as_u16(),
                                message,
                            });
                        }
                        let wait = self.retry_delay(retries);
                        tracing::debug!("server error {status}, retrying in {wait:?}");
                        sleep(wait).await;
                        retries += 1;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    if e.is_builder() || retries >= MAX_RETRIES {
                        return Err(ApiError::Network(e));
                    }
                    sleep(self.retry_delay(retries)).await;
                    retries += 1;
                }
            }
        }
    }

    /// Decode a response body. Any remaining 4xx maps to [`ApiError::Http`]
    /// with the API's own `error.message` when present.
    pub(crate) async fn decode<T: DeserializeOwned>(&self, resp: Response) -> Result<T, ApiError> {
        let status = resp.status();
        let body = resp.bytes().await.map_err(ApiError::Network)?;
        if status.is_client_error() || status.is_server_error() {
            return Err(ApiError::Http {
                status: status.as_u16(),
                message: extract_error_message(&body),
            });
        }
        serde_json::from_slice(&body).map_err(ApiError::Decode)
    }

    fn retry_delay(&self, attempt: u32) -> Duration {
        self.inner.limiter.retry_delay(attempt)
    }

    /// Persist the auth cookies the session currently holds into the vault.
    /// Only `auth`/`twoFactorAuth` are stored — nothing else (RULES.md §4).
    pub(crate) fn persist_auth_cookies(&self) -> Result<(), ApiError> {
        let Some(vault) = &self.inner.vault else {
            return Ok(());
        };
        let url = Url::parse(&self.inner.base_url).map_err(|e| ApiError::Vault(e.to_string()))?;
        let header = {
            let store = self.cookie_store();
            store
                .get_request_values(&url)
                .filter(|(name, _)| AUTH_COOKIE_NAMES.contains(name))
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        if !header.is_empty() {
            vault.save(&header)?;
        }
        Ok(())
    }

    /// Inject a persisted cookie header (`name=value; name=value`) into the
    /// cookie store ahead of a resumed session.
    pub(crate) fn inject_auth_cookies(&self, header_value: &str) -> Result<(), ApiError> {
        let url = Url::parse(&self.inner.base_url).map_err(|e| ApiError::Vault(e.to_string()))?;
        let mut store = self.cookie_store();
        for pair in header_value.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            // A stored "name=value" pair parses like a Set-Cookie value;
            // Path=/ keeps it valid for every endpoint under the API root.
            let _ = store.parse(&format!("{pair}; Path=/"), &url);
        }
        Ok(())
    }

    /// Drop the auth cookies from memory and from the vault.
    pub(crate) fn clear_auth_state(&self) {
        self.cookie_store().clear();
        if let Some(vault) = &self.inner.vault {
            let _ = vault.clear();
        }
    }

    pub(crate) fn vault(&self) -> Option<&Arc<dyn CookieVault>> {
        self.inner.vault.as_ref()
    }

    fn cookie_store(&self) -> MutexGuard<'_, CookieStore> {
        self.inner.cookies.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Parse a `Retry-After` header value (integer seconds).
fn retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    headers
        .get(header::RETRY_AFTER)
        .and_then(|v: &HeaderValue| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Best-effort body text for an error response, capped at 512 bytes.
async fn error_body(resp: Response) -> String {
    let body = resp.bytes().await.unwrap_or_default();
    extract_error_message(&body)
}

/// Pull `error.message` out of a VRChat error body, else truncate the body.
fn extract_error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(message) = value
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return message.to_string();
        }
    }
    text.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_to_api_root() {
        let client = Client::builder().build().unwrap();
        assert_eq!(client.base_url(), API_ROOT);
    }

    #[test]
    fn extract_error_message_prefers_api_message() {
        let body = br#"{"error":{"message":"synthetic failure","status_code":400}}"#;
        assert_eq!(extract_error_message(body), "synthetic failure");
        assert_eq!(extract_error_message(b"not json"), "not json");
    }
}
