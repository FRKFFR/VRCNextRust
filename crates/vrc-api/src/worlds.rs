//! World endpoints: `GET /worlds/{id}`.

use crate::models::World;
use crate::{ApiError, Client};

/// `true` for a well-formed `wrld_…` id (ASCII alphanumerics, `_`, `-`),
/// which is safe to place in a URL path without encoding.
pub fn valid_world_id(id: &str) -> bool {
    id.strip_prefix("wrld_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

impl Client {
    /// `GET /worlds/{id}` — public metadata for one world.
    ///
    /// Rejects malformed ids with [`ApiError::InvalidId`] before any request,
    /// so a remote-supplied location can never alter the request path.
    pub async fn world(&self, id: &str) -> Result<World, ApiError> {
        if !valid_world_id(id) {
            return Err(ApiError::InvalidId(id.chars().take(64).collect()));
        }
        self.get_json(&format!("/worlds/{id}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_world_id;

    #[test]
    fn world_id_validation() {
        assert!(valid_world_id("wrld_00000000-0000-0000-0000-000000000001"));
        assert!(!valid_world_id("wrld_"));
        assert!(!valid_world_id("usr_123"));
        assert!(!valid_world_id("wrld_../auth/user"));
        assert!(!valid_world_id("wrld_a?b=c"));
    }
}
