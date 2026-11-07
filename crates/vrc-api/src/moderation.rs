//! Player-moderation endpoints: `GET`/`POST /auth/user/playermoderations`,
//! `PUT /auth/user/unplayermoderate`.
//!
//! These cover block/unblock, mute/unmute and the avatar/interact toggles.

use reqwest::Method;

use crate::models::{PlayerModeration, PlayerModerationKind, SuccessResponse};
use crate::users::valid_user_id;
use crate::{ApiError, Client};

fn check_user_id(user_id: &str) -> Result<(), ApiError> {
    if valid_user_id(user_id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(user_id.chars().take(64).collect()))
    }
}

impl Client {
    /// `GET /auth/user/playermoderations` — the current user's moderations.
    /// `target_user_id` narrows the result to one user; `kind` filters by
    /// moderation kind.
    pub async fn player_moderations(
        &self,
        target_user_id: Option<&str>,
        kind: Option<PlayerModerationKind>,
    ) -> Result<Vec<PlayerModeration>, ApiError> {
        if let Some(id) = target_user_id {
            check_user_id(id)?;
        }
        let mut query = String::from("/auth/user/playermoderations?");
        if let Some(id) = target_user_id {
            query.push_str(&format!("targetUserId={id}&"));
        }
        if let Some(kind) = kind {
            query.push_str(&format!("type={}&", kind.as_str()));
        }
        self.get_json(query.trim_end_matches('&').trim_end_matches('?'))
            .await
    }

    /// `POST /auth/user/playermoderations` — apply a moderation
    /// (`{"moderated": userId, "type": kind}`).
    pub async fn moderate_user(
        &self,
        user_id: &str,
        kind: PlayerModerationKind,
    ) -> Result<PlayerModeration, ApiError> {
        check_user_id(user_id)?;
        let body = serde_json::json!({
            "moderated": user_id,
            "type": kind.as_str(),
        });
        self.send_json(Method::POST, "/auth/user/playermoderations", Some(&body))
            .await
    }

    /// `PUT /auth/user/unplayermoderate` — remove a moderation.
    pub async fn unmoderate_user(
        &self,
        user_id: &str,
        kind: PlayerModerationKind,
    ) -> Result<SuccessResponse, ApiError> {
        check_user_id(user_id)?;
        let body = serde_json::json!({
            "moderated": user_id,
            "type": kind.as_str(),
        });
        self.send_json(Method::PUT, "/auth/user/unplayermoderate", Some(&body))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moderation_kind_roundtrip() {
        for kind in [
            PlayerModerationKind::Block,
            PlayerModerationKind::Mute,
            PlayerModerationKind::MuteChat,
            PlayerModerationKind::Unmute,
            PlayerModerationKind::HideAvatar,
            PlayerModerationKind::ShowAvatar,
            PlayerModerationKind::InteractOn,
            PlayerModerationKind::InteractOff,
        ] {
            assert_eq!(PlayerModerationKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(PlayerModerationKind::parse("bogus"), None);
    }
}
