//! World endpoints: `GET /worlds`, `/worlds/active`, `/worlds/recent`,
//! `GET /worlds/{id}`.

use crate::models::World;
use crate::query::{append_bool, append_page, append_query_opt};
use crate::users::valid_user_id;
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

/// Filters for `GET /worlds` (Search All Worlds). Every optional value is
/// clamped/encoded by the shared query helpers; `user_id` is validated
/// before it can become a query value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorldSearchQuery {
    /// `search` — world-name filter.
    pub search: Option<String>,
    /// `user` — the documented `me` scope for own worlds (`/worlds` only).
    pub user: Option<String>,
    /// `userId` — `usr_…` author filter (`/worlds` only).
    pub user_id: Option<String>,
    /// `featured` — featured-only filter.
    pub featured: Option<bool>,
    /// `sort` — documented sort option.
    pub sort: Option<String>,
    /// `order` — `ascending`/`descending`.
    pub order: Option<String>,
    /// `releaseStatus` — `all`/`hidden`/`private`/`public`.
    pub release_status: Option<String>,
    /// `tag` — comma-separated include tags.
    pub tag: Option<String>,
    /// `notag` — comma-separated exclude tags.
    pub notag: Option<String>,
    /// `platform` — supported platform filter.
    pub platform: Option<String>,
    /// `n` — page size (clamped to 1–100).
    pub n: Option<u32>,
    /// `offset` — zero-based offset (clamped).
    pub offset: Option<u64>,
}

/// Append the query filters documented for the world *list* endpoints
/// (`/worlds`, `/worlds/active`, `/worlds/recent`). `scoped` additionally
/// appends `user`/`userId`, which only `/worlds` documents.
fn append_world_filters(path: &mut String, query: &WorldSearchQuery, scoped: bool) {
    append_bool(path, "featured", query.featured);
    append_query_opt(path, "sort", query.sort.as_deref());
    if scoped {
        append_query_opt(path, "user", query.user.as_deref());
        append_query_opt(path, "userId", query.user_id.as_deref());
    }
    append_page(path, query.n, query.offset);
    append_query_opt(path, "order", query.order.as_deref());
    append_query_opt(path, "search", query.search.as_deref());
    append_query_opt(path, "tag", query.tag.as_deref());
    append_query_opt(path, "notag", query.notag.as_deref());
    append_query_opt(path, "releaseStatus", query.release_status.as_deref());
    append_query_opt(path, "platform", query.platform.as_deref());
}

fn check_world_scope(query: &WorldSearchQuery) -> Result<(), ApiError> {
    if let Some(user) = &query.user {
        if user != "me" {
            return Err(ApiError::InvalidId(format!("user scope {user}")));
        }
    }
    if let Some(user_id) = &query.user_id {
        if !valid_user_id(user_id) {
            return Err(ApiError::InvalidId(user_id.chars().take(64).collect()));
        }
    }
    Ok(())
}

impl Client {
    /// `GET /worlds` — search/list worlds with the documented filters.
    pub async fn search_worlds(&self, query: &WorldSearchQuery) -> Result<Vec<World>, ApiError> {
        check_world_scope(query)?;
        let mut path = String::from("/worlds");
        append_world_filters(&mut path, query, true);
        self.get_json(&path).await
    }

    /// `GET /worlds/active` — currently active worlds (`searchWorlds`
    /// family; `user`/`userId` are not documented here).
    pub async fn active_worlds(&self, query: &WorldSearchQuery) -> Result<Vec<World>, ApiError> {
        check_world_scope(query)?;
        let mut path = String::from("/worlds/active");
        append_world_filters(&mut path, query, false);
        self.get_json(&path).await
    }

    /// `GET /worlds/recent` — recently visited worlds (`searchWorlds`
    /// family; `user`/`userId` are not documented here).
    pub async fn recent_worlds(&self, query: &WorldSearchQuery) -> Result<Vec<World>, ApiError> {
        check_world_scope(query)?;
        let mut path = String::from("/worlds/recent");
        append_world_filters(&mut path, query, false);
        self.get_json(&path).await
    }

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
