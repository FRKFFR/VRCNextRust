//! Instance endpoint: `GET /instances/{worldId}:{instanceId}`.
//!
//! Returns the documented `Instance` object; `users` is populated only
//! for instances owned by the requesting user.

use crate::models::Instance;
use crate::{ApiError, Client};

/// `true` for a well-formed instance id — safe charset only
/// (alphanumerics plus `_ ~ ( ) - +`, the documented `~hidden(…)~region(…)`
/// forms), no path/query separators, no `..`.
pub fn valid_instance_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '(' | ')' | '-' | '+'))
}

/// `true` for a well-formed `wrld_…` world id.
pub fn valid_world_segment(world_id: &str) -> bool {
    crate::worlds::valid_world_id(world_id)
}

fn check_path_ids(world_id: &str, instance_id: &str) -> Result<(), ApiError> {
    if !valid_world_segment(world_id) {
        return Err(ApiError::InvalidId(world_id.chars().take(64).collect()));
    }
    if !valid_instance_id(instance_id) {
        return Err(ApiError::InvalidId(instance_id.chars().take(64).collect()));
    }
    Ok(())
}

impl Client {
    /// `GET /instances/{worldId}:{instanceId}` — instance details, including
    /// `users` when the instance belongs to the requesting user.
    pub async fn instance(&self, world_id: &str, instance_id: &str) -> Result<Instance, ApiError> {
        check_path_ids(world_id, instance_id)?;
        self.get_json(&format!("/instances/{world_id}:{instance_id}"))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_validation() {
        assert!(valid_instance_id(
            "12345~hidden(usr_x)~region(eu)~nonce(abc)"
        ));
        assert!(valid_instance_id("offline"));
        assert!(valid_instance_id("private"));
        assert!(!valid_instance_id(""));
        assert!(!valid_instance_id("a/b"));
        assert!(!valid_instance_id("a?b=c"));
        assert!(!valid_instance_id("a&b=c"));
        assert!(!valid_instance_id("a..b"));
        assert!(!valid_instance_id("a:b"));
    }
}
