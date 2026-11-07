//! Avatar endpoints: search, detail, own avatar, select.

use reqwest::Method;

use crate::models::{Avatar, CurrentUser};
use crate::query::{append_bool, append_page, append_query_opt};
use crate::users::valid_user_id;
use crate::{ApiError, Client};

/// `true` for a well-formed `avtr_…` avatar id.
pub fn valid_avatar_id(id: &str) -> bool {
    id.strip_prefix("avtr_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

fn check_avatar_id(id: &str) -> Result<(), ApiError> {
    if valid_avatar_id(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

/// Filters for `GET /avatars` (Search Avatars). The documented API only
/// searches your own (`user=me`) or featured avatars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AvatarSearchQuery {
    /// `search` — name/description filter.
    pub search: Option<String>,
    /// `user` — documented `me` scope for own avatars.
    pub user: Option<String>,
    /// `userId` — `usr_…` author filter.
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
    /// `isInternalVariant`.
    pub is_internal_variant: Option<bool>,
    /// `n` — page size (clamped to 1–100).
    pub n: Option<u32>,
    /// `offset` — zero-based offset (clamped).
    pub offset: Option<u64>,
}

impl Client {
    /// `GET /avatars` — search/list avatars (own or featured scope).
    pub async fn search_avatars(&self, query: &AvatarSearchQuery) -> Result<Vec<Avatar>, ApiError> {
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
        let mut path = String::from("/avatars");
        append_bool(&mut path, "featured", query.featured);
        append_query_opt(&mut path, "sort", query.sort.as_deref());
        append_query_opt(&mut path, "user", query.user.as_deref());
        append_query_opt(&mut path, "userId", query.user_id.as_deref());
        append_page(&mut path, query.n, query.offset);
        append_query_opt(&mut path, "order", query.order.as_deref());
        append_query_opt(&mut path, "search", query.search.as_deref());
        append_query_opt(&mut path, "tag", query.tag.as_deref());
        append_query_opt(&mut path, "notag", query.notag.as_deref());
        append_query_opt(&mut path, "releaseStatus", query.release_status.as_deref());
        append_query_opt(&mut path, "platform", query.platform.as_deref());
        append_bool(&mut path, "isInternalVariant", query.is_internal_variant);
        self.get_json(&path).await
    }

    /// `GET /avatars/{avatarId}` — public avatar metadata.
    pub async fn avatar(&self, avatar_id: &str) -> Result<Avatar, ApiError> {
        check_avatar_id(avatar_id)?;
        self.get_json(&format!("/avatars/{avatar_id}")).await
    }

    /// `GET /users/{userId}/avatar` — the current user's active avatar.
    /// The API rejects any user other than the authenticated one.
    pub async fn own_avatar(&self, user_id: &str) -> Result<Avatar, ApiError> {
        if !valid_user_id(user_id) {
            return Err(ApiError::InvalidId(user_id.chars().take(64).collect()));
        }
        self.get_json(&format!("/users/{user_id}/avatar")).await
    }

    /// `PUT /avatars/{avatarId}/select` — switch into an avatar; returns
    /// the updated user object.
    pub async fn select_avatar(&self, avatar_id: &str) -> Result<CurrentUser, ApiError> {
        check_avatar_id(avatar_id)?;
        self.send_json(Method::PUT, &format!("/avatars/{avatar_id}/select"), None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_avatar_id;

    #[test]
    fn avatar_id_validation() {
        assert!(valid_avatar_id("avtr_00000000-0000-0000-0000-000000000001"));
        assert!(!valid_avatar_id("avtr_"));
        assert!(!valid_avatar_id("usr_123"));
        assert!(!valid_avatar_id("avtr_../x"));
        assert!(!valid_avatar_id("avtr_a?b=c"));
        assert!(!valid_avatar_id("avtr_a b"));
    }
}
