//! World endpoints: `GET /worlds/{id}`.

use crate::models::World;
use crate::{ApiError, Client};

impl Client {
    /// `GET /worlds/{id}` — public metadata for one world.
    ///
    /// `id` is the `wrld_…` part of a friend location; world ids only
    /// contain path-safe characters, so no extra encoding is needed.
    pub async fn world(&self, id: &str) -> Result<World, ApiError> {
        self.get_json(&format!("/worlds/{id}")).await
    }
}
