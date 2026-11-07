//! Favorite endpoints: `/favorites` records plus the typed world/avatar
//! favorite lists.

use reqwest::Method;

use crate::avatars::valid_avatar_id;
use crate::models::{Avatar, Favorite, FavoriteKind, SuccessResponse, World};
use crate::query::{append_page, append_query_opt};
use crate::users::valid_user_id;
use crate::worlds::valid_world_id;
use crate::{ApiError, Client};

/// `true` for a well-formed `fvrt_…` favorite-record id.
pub fn valid_favorite_id(id: &str) -> bool {
    id.strip_prefix("fvrt_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

fn check_favorite_object_id(kind: FavoriteKind, id: &str) -> Result<(), ApiError> {
    let ok = match kind {
        FavoriteKind::World => valid_world_id(id),
        FavoriteKind::Avatar => valid_avatar_id(id),
        FavoriteKind::Friend => valid_user_id(id),
    };
    if ok {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

impl Client {
    /// `GET /favorites?type=…` — one page of favorite records.
    pub async fn favorites(
        &self,
        kind: FavoriteKind,
        n: Option<u32>,
        offset: Option<u64>,
    ) -> Result<Vec<Favorite>, ApiError> {
        let mut path = format!("/favorites?type={}", kind.as_str());
        append_page(&mut path, n, offset);
        self.get_json(&path).await
    }

    /// `POST /favorites` — favorite an object; returns the record.
    pub async fn add_favorite(
        &self,
        kind: FavoriteKind,
        favorite_id: &str,
        tags: &[String],
    ) -> Result<Favorite, ApiError> {
        check_favorite_object_id(kind, favorite_id)?;
        let body = serde_json::json!({
            "type": kind.as_str(),
            "favoriteId": favorite_id,
            "tags": tags,
        });
        self.send_json(Method::POST, "/favorites", Some(&body))
            .await
    }

    /// `DELETE /favorites/{favoriteId}` — remove a favorite record.
    pub async fn remove_favorite(&self, favorite_id: &str) -> Result<SuccessResponse, ApiError> {
        if !valid_favorite_id(favorite_id) {
            return Err(ApiError::InvalidId(favorite_id.chars().take(64).collect()));
        }
        self.send_json(Method::DELETE, &format!("/favorites/{favorite_id}"), None)
            .await
    }

    /// `GET /worlds/favorites` — favorited worlds.
    pub async fn favorite_worlds(
        &self,
        n: Option<u32>,
        offset: Option<u64>,
        tag: Option<&str>,
    ) -> Result<Vec<World>, ApiError> {
        let mut path = String::from("/worlds/favorites");
        append_page(&mut path, n, offset);
        append_query_opt(&mut path, "tag", tag);
        self.get_json(&path).await
    }

    /// `GET /avatars/favorites` — favorited avatars.
    pub async fn favorite_avatars(
        &self,
        n: Option<u32>,
        offset: Option<u64>,
        tag: Option<&str>,
    ) -> Result<Vec<Avatar>, ApiError> {
        let mut path = String::from("/avatars/favorites");
        append_page(&mut path, n, offset);
        append_query_opt(&mut path, "tag", tag);
        self.get_json(&path).await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_favorite_id;

    #[test]
    fn favorite_id_validation() {
        assert!(valid_favorite_id(
            "fvrt_00000000-0000-0000-0000-000000000001"
        ));
        assert!(!valid_favorite_id("fvrt_"));
        assert!(!valid_favorite_id("fv_123"));
        assert!(!valid_favorite_id("fvrt_../x"));
        assert!(!valid_favorite_id("fvrt_a?b=c"));
    }
}
