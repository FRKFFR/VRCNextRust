//! VRChat REST client: auth + 2FA, rate limiter, models, USER_AGENT.

/// HTTP User-Agent used for every REST and WebSocket request.
///
/// Defined exactly once here (RULES.md §2). Format:
/// `VRCNextRust/<version> (https://github.com/FRKFFR/VRCNextRust)`
pub const USER_AGENT: &str = concat!(
    "VRCNextRust/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/FRKFFR/VRCNextRust)"
);

#[cfg(test)]
mod tests {
    use super::USER_AGENT;

    #[test]
    fn user_agent_uses_package_version_and_ends_with_paren() {
        assert!(USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(USER_AGENT.ends_with(')'));
    }
}
