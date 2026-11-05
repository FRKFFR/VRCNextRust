//! User endpoints: profile, friend status/requests, unfriend, notes.
//!
//! `GET /users/{userId}` is the public profile; friend actions go through
//! `/user/{userId}/friendRequest` and `/auth/user/friends/{userId}`; notes
//! through `POST /userNotes`. All ids are validated before they become
//! path segments.

use reqwest::Method;

use crate::models::{FriendStatus, Notification, SuccessResponse, User, UserNote};
use crate::{ApiError, Client};

/// `true` for a well-formed `usr_…` id (ASCII alphanumerics, `_`, `-`),
/// safe to place in a URL path without encoding.
pub fn valid_user_id(id: &str) -> bool {
    id.strip_prefix("usr_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

fn check_user_id(id: &str) -> Result<(), ApiError> {
    if valid_user_id(id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(id.chars().take(64).collect()))
    }
}

impl Client {
    /// `GET /users/{userId}` — public profile of one user.
    pub async fn user(&self, user_id: &str) -> Result<User, ApiError> {
        check_user_id(user_id)?;
        self.get_json(&format!("/users/{user_id}")).await
    }

    /// `GET /user/{userId}/friendStatus` — friend/request state vs. one user.
    pub async fn friend_status(&self, user_id: &str) -> Result<FriendStatus, ApiError> {
        check_user_id(user_id)?;
        self.get_json(&format!("/user/{user_id}/friendStatus")).await
    }

    /// `POST /user/{userId}/friendRequest` — send a friend request; returns
    /// the created `frq_…` notification.
    pub async fn send_friend_request(&self, user_id: &str) -> Result<Notification, ApiError> {
        check_user_id(user_id)?;
        self.send_json(Method::POST, &format!("/user/{user_id}/friendRequest"), None)
            .await
    }

    /// `DELETE /user/{userId}/friendRequest` — cancel an outgoing friend
    /// request.
    pub async fn cancel_friend_request(&self, user_id: &str) -> Result<SuccessResponse, ApiError> {
        check_user_id(user_id)?;
        self.send_json(Method::DELETE, &format!("/user/{user_id}/friendRequest"), None)
            .await
    }

    /// `DELETE /auth/user/friends/{userId}` — remove a friend.
    pub async fn unfriend(&self, user_id: &str) -> Result<SuccessResponse, ApiError> {
        check_user_id(user_id)?;
        self.send_json(Method::DELETE, &format!("/auth/user/friends/{user_id}"), None)
            .await
    }

    /// `POST /userNotes` — create/update the current user's note on `user_id`.
    pub async fn set_user_note(&self, user_id: &str, note: &str) -> Result<UserNote, ApiError> {
        check_user_id(user_id)?;
        let body = serde_json::json!({
            "targetUserId": user_id,
            "note": note,
        });
        self.send_json(Method::POST, "/userNotes", Some(&body)).await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_user_id;

    #[test]
    fn user_id_validation() {
        assert!(valid_user_id("usr_00000000-0000-0000-0000-000000000001"));
        assert!(!valid_user_id("usr_"));
        assert!(!valid_user_id("not_123"));
        assert!(!valid_user_id("usr_../auth/user"));
        assert!(!valid_user_id("usr_a?b=c"));
        assert!(!valid_user_id("usr_a b"));
    }
}
