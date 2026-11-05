//! Notification endpoints: `GET /auth/user/notifications` and the per-id
//! actions (`see`, `hide`, `accept`, `clear`).
//!
//! Every call travels through the shared rate-limited [`Client::send`]
//! path (RULES.md §3); ids are validated before they become path segments.

use reqwest::Method;

use crate::models::Notification;
use crate::{ApiError, Client};

/// Documented maximum page size for list endpoints.
const PAGE_SIZE: usize = 100;

/// Upper bound on pages fetched per call.
const MAX_PAGES: usize = 50;

/// `true` for a well-formed notification id — `not_…` for regular
/// notifications and `frq_…` for friend requests (the documented id used by
/// `…/accept`). Safe charset only, so the id can sit in a path segment.
pub fn valid_notification_id(id: &str) -> bool {
    let rest = id.strip_prefix("not_").or_else(|| id.strip_prefix("frq_"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

impl Client {
    /// `GET /auth/user/notifications` — every page merged into one list,
    /// capped at [`MAX_PAGES`] like the friends fetch.
    pub async fn notifications(&self) -> Result<Vec<Notification>, ApiError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        for _ in 0..MAX_PAGES {
            let page: Vec<Notification> = self
                .get_json(&format!(
                    "/auth/user/notifications?n={PAGE_SIZE}&offset={offset}"
                ))
                .await?;
            let len = page.len();
            out.extend(page);
            if len < PAGE_SIZE {
                return Ok(out);
            }
            offset += len;
        }
        tracing::warn!(
            "notifications pagination hit the {MAX_PAGES}-page cap; returning partial list"
        );
        Ok(out)
    }

    /// `GET /auth/user/notifications/{id}` — one notification.
    pub async fn notification(&self, id: &str) -> Result<Notification, ApiError> {
        if !valid_notification_id(id) {
            return Err(ApiError::InvalidId(id.chars().take(64).collect()));
        }
        self.get_json(&format!("/auth/user/notifications/{id}"))
            .await
    }

    /// `PUT /auth/user/notifications/{id}/see` — mark as seen.
    pub async fn see_notification(&self, id: &str) -> Result<Notification, ApiError> {
        if !valid_notification_id(id) {
            return Err(ApiError::InvalidId(id.chars().take(64).collect()));
        }
        self.send_json(
            Method::PUT,
            &format!("/auth/user/notifications/{id}/see"),
            None,
        )
        .await
    }

    /// `PUT /auth/user/notifications/{id}/hide` — delete/hide it; returns
    /// the `Success` envelope like the other mutating actions.
    pub async fn hide_notification(&self, id: &str) -> Result<crate::SuccessResponse, ApiError> {
        if !valid_notification_id(id) {
            return Err(ApiError::InvalidId(id.chars().take(64).collect()));
        }
        self.send_json(
            Method::PUT,
            &format!("/auth/user/notifications/{id}/hide"),
            None,
        )
        .await
    }

    /// `PUT /auth/user/notifications/{id}/accept` — accept a friend
    /// request (the id is the `frq_…` notification id).
    pub async fn accept_friend_request(
        &self,
        id: &str,
    ) -> Result<crate::SuccessResponse, ApiError> {
        if !valid_notification_id(id) {
            return Err(ApiError::InvalidId(id.chars().take(64).collect()));
        }
        self.send_json(
            Method::PUT,
            &format!("/auth/user/notifications/{id}/accept"),
            None,
        )
        .await
    }

    /// `PUT /auth/user/notifications/clear` — clear **all** notifications.
    pub async fn clear_notifications(&self) -> Result<crate::SuccessResponse, ApiError> {
        self.send_json(Method::PUT, "/auth/user/notifications/clear", None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::valid_notification_id;

    #[test]
    fn notification_id_validation() {
        assert!(valid_notification_id(
            "not_00000000-0000-0000-0000-000000000001"
        ));
        assert!(valid_notification_id(
            "frq_00000000-0000-0000-0000-000000000001"
        ));
        assert!(!valid_notification_id("not_"));
        assert!(!valid_notification_id("usr_123"));
        assert!(!valid_notification_id("not_../auth/user"));
        assert!(!valid_notification_id("not_a?b=c"));
    }
}
