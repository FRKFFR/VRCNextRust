//! Error type for all `vrc-api` operations.

use std::time::Duration;

/// Errors returned by the VRChat REST client.
///
/// Never carries secrets: messages are derived from public API response
/// bodies or transport-level error descriptions only (RULES.md §4).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The API answered with a non-retryable 4xx status, or exhausted retries
    /// on a 5xx status.
    #[error("HTTP {status}: {message}")]
    Http { status: u16, message: String },

    /// The API returned 429 and the retry budget was exhausted.
    #[error("rate limited by the VRChat API (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },

    /// The API rejected the credentials or the session is no longer valid.
    /// Any stored auth cookies have been cleared already.
    #[error("unauthorized")]
    Unauthorized,

    /// The account requires two-factor authentication before the session can
    /// be used.
    #[error("two-factor authentication required")]
    Requires2fa,

    /// A transport-level failure (DNS, connect, timeout, TLS, body stream).
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),

    /// The response body could not be decoded into the expected model.
    #[error("failed to decode API response: {0}")]
    Decode(#[source] serde_json::Error),

    /// Reading or writing the cookie vault failed.
    #[error("cookie vault error: {0}")]
    Vault(String),

    /// A caller-supplied id was malformed; no request was sent.
    #[error("invalid id: {0}")]
    InvalidId(String),

    /// A response body exceeded the caller's byte cap.
    #[error("response exceeded the {limit} byte limit")]
    PayloadTooLarge {
        /// The configured cap in bytes.
        limit: usize,
    },
}
