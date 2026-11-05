//! VRChat REST client: auth + 2FA, rate limiter, models, USER_AGENT.
//!
//! All requests share one send path with a token-bucket rate limiter, capped
//! exponential backoff, `Retry-After` handling and automatic auth-state
//! cleanup on 401 (RULES.md §3). Only auth cookies are persisted, through a
//! pluggable [`CookieVault`] (RULES.md §4).

/// HTTP User-Agent used for every REST and WebSocket request.
///
/// Defined exactly once here (RULES.md §2). Format:
/// `VRCNextRust/<version> (https://github.com/FRKFFR/VRCNextRust)`
pub const USER_AGENT: &str = concat!(
    "VRCNextRust/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/FRKFFR/VRCNextRust)"
);

mod auth;
mod client;
mod error;
mod friends;
pub mod models;
mod rate_limit;
mod vault;
mod worlds;

pub use auth::LoginOutcome;
pub use client::{Client, ClientBuilder, API_ROOT};
pub use error::ApiError;
pub use models::{
    CurrentUser, Favorite, LimitedUser, PastDisplayName, TrustRank, TwoFactorMethod, User, World,
};
pub use rate_limit::RateLimiter;
pub use vault::{CookieVault, KeyringVault, MemoryVault};

#[cfg(test)]
mod tests {
    use super::USER_AGENT;

    #[test]
    fn user_agent_uses_package_version_and_ends_with_paren() {
        assert!(USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(USER_AGENT.ends_with(')'));
    }
}
