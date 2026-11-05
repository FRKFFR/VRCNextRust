//! Invite endpoints: invite/request-invite/respond under `/invite` and
//! `/requestInvite`.
//!
//! All calls go through the shared rate-limited [`Client::send`] path;
//! path ids are validated before use.

use reqwest::Method;

use crate::models::Notification;
use crate::users::valid_user_id;
use crate::{valid_notification_id, ApiError, Client};

fn check_user_id(user_id: &str) -> Result<(), ApiError> {
    if valid_user_id(user_id) {
        Ok(())
    } else {
        Err(ApiError::InvalidId(user_id.chars().take(64).collect()))
    }
}

/// Validates an instance id for use in a request body — same charset rules
/// as [`crate::instances::valid_instance_id`], re-checked here so callers
/// can't smuggle a malformed location into the JSON body either.
fn valid_instance_segment(instance_id: &str) -> bool {
    !instance_id.is_empty()
        && instance_id.len() <= 256
        && !instance_id.contains("..")
        && instance_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '(' | ')' | '-' | '+'))
}

impl Client {
    /// `POST /invite/{userId}` — invite a user to an instance.
    ///
    /// `instance_id` is the target instance (without the `world:` prefix —
    /// the API pairs it with the world's id server-side, so callers pass
    /// the bare instance id, e.g. `12345~hidden(usr_…)~region(eu)`).
    /// `message_slot` selects a custom invite message (0–11).
    ///
    /// Returns the created invite notification.
    pub async fn invite_user(
        &self,
        user_id: &str,
        instance_id: &str,
        message_slot: Option<u8>,
    ) -> Result<Notification, ApiError> {
        check_user_id(user_id)?;
        if !valid_instance_segment(instance_id) {
            return Err(ApiError::InvalidId(instance_id.chars().take(64).collect()));
        }
        if let Some(slot) = message_slot {
            if slot > 11 {
                return Err(ApiError::InvalidId(format!("message slot {slot}")));
            }
        }
        let mut body = serde_json::json!({ "instanceId": instance_id });
        if let Some(slot) = message_slot {
            body["messageSlot"] = serde_json::json!(slot);
        }
        self.send_json(Method::POST, &format!("/invite/{user_id}"), Some(&body))
            .await
    }

    /// `POST /requestInvite/{userId}` — ask a user to invite you.
    /// `request_slot` selects a custom request message (0–11).
    pub async fn request_invite(
        &self,
        user_id: &str,
        request_slot: Option<u8>,
    ) -> Result<Notification, ApiError> {
        check_user_id(user_id)?;
        if let Some(slot) = request_slot {
            if slot > 11 {
                return Err(ApiError::InvalidId(format!("request slot {slot}")));
            }
        }
        let body = serde_json::json!({
            "requestSlot": request_slot.unwrap_or(0),
        });
        self.send_json(
            Method::POST,
            &format!("/requestInvite/{user_id}"),
            Some(&body),
        )
        .await
    }

    /// `POST /invite/{notificationId}/response` — reply to an invite
    /// notification with a response slot (0–11).
    pub async fn respond_invite(
        &self,
        notification_id: &str,
        response_slot: u8,
    ) -> Result<Notification, ApiError> {
        if !valid_notification_id(notification_id) {
            return Err(ApiError::InvalidId(
                notification_id.chars().take(64).collect(),
            ));
        }
        if response_slot > 11 {
            return Err(ApiError::InvalidId(format!(
                "response slot {response_slot}"
            )));
        }
        let body = serde_json::json!({ "responseSlot": response_slot });
        self.send_json(
            Method::POST,
            &format!("/invite/{notification_id}/response"),
            Some(&body),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_instance_segment;

    #[test]
    fn instance_segment_validation() {
        assert!(valid_instance_segment(
            "12345~hidden(usr_x)~region(eu)~nonce(abc)"
        ));
        assert!(valid_instance_segment("offline"));
        assert!(!valid_instance_segment(""));
        assert!(!valid_instance_segment("a/b"));
        assert!(!valid_instance_segment("a?b=c"));
        assert!(!valid_instance_segment("a..b"));
    }
}
