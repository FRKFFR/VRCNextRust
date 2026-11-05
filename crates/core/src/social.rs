//! Pure helpers for the social-actions surface: validation, labels and
//! instance/friend matching. No I/O — everything here is unit-testable
//! (RULES.md §7).

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;
use vrc_api::{valid_instance_id, valid_user_id, NotificationV2, PlayerModerationKind};

use crate::state::{AppState, Friend};

/// Maximum number of targets in one multi-invite batch (anti-spam cap).
pub const MULTI_INVITE_MAX_TARGETS: usize = 50;

/// Minimum delay between consecutive invites in a multi-invite batch.
/// Combined with the shared rate limiter this keeps batches polite
/// (RULES.md §3).
pub const MULTI_INVITE_GAP: Duration = Duration::from_millis(1500);

/// `presence.instance` sentinels that are not joinable instances — invites
/// to them are meaningless, so `own_instance_id` reports `None` instead.
const NON_INSTANCE_LOCATIONS: &[&str] = &["", "offline", "private", "traveling", "hidden"];

/// Validate and deduplicate multi-invite targets, preserving order.
///
/// Returns the cleaned id list or a human-readable error (surfaced via
/// `AppEvent::Error` / `last_error`). Each id must be a well-formed
/// `usr_…` id; the batch is capped at [`MULTI_INVITE_MAX_TARGETS`].
pub fn validate_multi_invite_targets(ids: &[String]) -> Result<Vec<String>, String> {
    if ids.len() > MULTI_INVITE_MAX_TARGETS {
        return Err(format!(
            "multi-invite is limited to {MULTI_INVITE_MAX_TARGETS} targets (got {})",
            ids.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if !valid_user_id(id) {
            return Err(format!("invalid user id: {id}"));
        }
        if seen.insert(id.as_str()) {
            out.push(id.clone());
        }
    }
    if out.is_empty() {
        return Err("multi-invite needs at least one target".to_string());
    }
    Ok(out)
}

/// The instance id the current user is in, for "invite to my instance"
/// flows. `None` when the session reports no joinable instance (offline,
/// traveling, …) — callers surface that as an error.
pub fn own_instance_id(state: &AppState) -> Option<String> {
    let instance = state
        .current_user
        .as_ref()?
        .presence
        .as_ref()?
        .instance
        .trim()
        .to_string();
    if NON_INSTANCE_LOCATIONS.contains(&instance.as_str()) || !valid_instance_id(&instance) {
        return None;
    }
    Some(instance)
}

/// Friends whose `location` puts them in `world_id:instance_id`.
///
pub fn friends_in_instance<'a>(
    friends: &'a HashMap<String, Friend>,
    world_id: &str,
    instance_id: &str,
) -> Vec<&'a Friend> {
    if world_id.is_empty() || instance_id.is_empty() {
        return Vec::new();
    }
    friends
        .values()
        .filter(|f| {
            f.world_id == world_id
                && f.location
                    .split_once(':')
                    .map(|(_, inst)| inst)
                    .unwrap_or("")
                    == instance_id
        })
        .collect()
}

/// Merge a `notification-v2-update` payload into a stored NotificationV2.
///
/// `NotificationV2` serializes back to wire field names, so a shallow
/// object merge followed by a re-decode applies the documented `updates`
/// shape.
pub fn merge_notification_v2(existing: &mut NotificationV2, updates: &Value) {
    let Some(fields) = updates.as_object() else {
        return;
    };
    let Ok(mut merged) = serde_json::to_value(&*existing) else {
        return;
    };
    let Some(object) = merged.as_object_mut() else {
        return;
    };
    for (key, value) in fields {
        object.insert(key.clone(), value.clone());
    }
    if let Ok(decoded) = serde_json::from_value::<NotificationV2>(merged) {
        *existing = decoded;
    }
}

/// Short label for a notification `type` value. REST uses camelCase
/// (`friendRequest`, `inviteResponse`, …) while pipeline pushes use the
/// documented kebab-case spellings (`invite-response`, `vote-to-kick`, …);
/// both map to the same label. Unknown kinds render as-is.
pub fn notification_kind_label(kind: &str) -> &'static str {
    match kind {
        "friendRequest" | "friend-request" => "Friend request",
        "invite" => "Invite",
        "inviteResponse" | "invite-response" => "Invite response",
        "message" => "Message",
        "requestInvite" | "request-invite" => "Invite request",
        "requestInviteResponse" | "request-invite-response" => "Invite request response",
        "votetokick" | "vote-to-kick" => "Vote to kick",
        "boop" => "Boop",
        "groupAnnouncement" => "Group announcement",
        "groupJoinRequest" => "Group join request",
        "groupJoinResponse" => "Group join response",
        "groupLeave" => "Group leave",
        "groupInvite" => "Group invite",
        "friendRequestResponse" => "Friend request response",
        "worldUploaded" => "World uploaded",
        _ => "Notification",
    }
}

/// Human label for a [`PlayerModerationKind`] wire name.
pub fn moderation_label(kind: &str) -> &'static str {
    match PlayerModerationKind::parse(kind) {
        Some(PlayerModerationKind::Block) => "Blocked",
        Some(PlayerModerationKind::HideAvatar) => "Avatar hidden",
        Some(PlayerModerationKind::InteractOff) => "Interactions off",
        Some(PlayerModerationKind::InteractOn) => "Interactions on",
        Some(PlayerModerationKind::Mute) => "Muted",
        Some(PlayerModerationKind::MuteChat) => "Chat muted",
        Some(PlayerModerationKind::ShowAvatar) => "Avatar shown",
        Some(PlayerModerationKind::Unmute) => "Unmuted",
        Some(PlayerModerationKind::UnmuteChat) => "Chat unmuted",
        None => "Moderated",
    }
}

/// Human label for [`crate::state::FriendStatus`].
pub fn friend_status_label(status: &vrc_api::FriendStatus) -> &'static str {
    if status.is_friend {
        "Friends"
    } else if status.outgoing_request {
        "Request sent"
    } else if status.incoming_request {
        "Request received"
    } else {
        "Not friends"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, Friend, ProfileView};
    use vrc_api::{CurrentUser, CurrentUserPresence};

    fn uid(n: u8) -> String {
        format!("usr_00000000-0000-0000-0000-{n:012}")
    }

    #[test]
    fn multi_invite_validation_dedupes_and_caps() {
        let ids = vec![uid(1), uid(1), uid(2)];
        let out = validate_multi_invite_targets(&ids).unwrap();
        assert_eq!(out, vec![uid(1), uid(2)]);

        let too_many: Vec<String> = (0..=MULTI_INVITE_MAX_TARGETS)
            .map(|i| uid(i as u8))
            .collect();
        assert!(validate_multi_invite_targets(&too_many).is_err());

        assert!(validate_multi_invite_targets(&[]).is_err());
        assert!(validate_multi_invite_targets(&["not_1".to_string()]).is_err());
        assert!(validate_multi_invite_targets(&["usr_../x".to_string()]).is_err());
    }

    #[test]
    fn own_instance_id_reads_presence_and_filters_sentinels() {
        let mut state = AppState::default();
        assert_eq!(own_instance_id(&state), None);

        state.current_user = Some(CurrentUser {
            presence: Some(CurrentUserPresence {
                instance: "12345~hidden(usr_x)~region(eu)".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            own_instance_id(&state).as_deref(),
            Some("12345~hidden(usr_x)~region(eu)")
        );

        state
            .current_user
            .as_mut()
            .unwrap()
            .presence
            .as_mut()
            .unwrap()
            .instance = "offline".to_string();
        assert_eq!(own_instance_id(&state), None);

        state
            .current_user
            .as_mut()
            .unwrap()
            .presence
            .as_mut()
            .unwrap()
            .instance = "bad/id".to_string();
        assert_eq!(own_instance_id(&state), None);
    }

    #[test]
    fn friends_in_instance_matches_world_and_instance_parts() {
        let friends: HashMap<String, Friend> = [
            (
                uid(1),
                Friend {
                    id: uid(1),
                    world_id: "wrld_a".to_string(),
                    location: "wrld_a:inst_one".to_string(),
                    ..Default::default()
                },
            ),
            (
                uid(2),
                Friend {
                    id: uid(2),
                    world_id: "wrld_a".to_string(),
                    location: "wrld_a:inst_two".to_string(),
                    ..Default::default()
                },
            ),
            (
                uid(3),
                Friend {
                    id: uid(3),
                    world_id: "wrld_b".to_string(),
                    location: "wrld_b:inst_one".to_string(),
                    ..Default::default()
                },
            ),
        ]
        .into_iter()
        .collect();

        let found = friends_in_instance(&friends, "wrld_a", "inst_one");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, uid(1));
        assert!(friends_in_instance(&friends, "", "inst_one").is_empty());
        assert!(friends_in_instance(&friends, "wrld_a", "").is_empty());
    }

    #[test]
    fn merge_notification_v2_applies_update_fields() {
        let mut v2 = NotificationV2 {
            id: "ntf2_1".to_string(),
            title: "Hello".to_string(),
            seen: false,
            ..Default::default()
        };
        merge_notification_v2(
            &mut v2,
            &serde_json::json!({"seen": true, "title": "Updated"}),
        );
        assert!(v2.seen);
        assert_eq!(v2.title, "Updated");
        assert_eq!(v2.id, "ntf2_1");

        // Non-object updates are ignored.
        merge_notification_v2(&mut v2, &serde_json::json!("nope"));
        assert_eq!(v2.title, "Updated");
    }

    #[test]
    fn notification_kind_labels_cover_rest_and_pipeline_spellings() {
        // REST camelCase and pipeline kebab-case spellings share labels.
        assert_eq!(notification_kind_label("friendRequest"), "Friend request");
        assert_eq!(notification_kind_label("friend-request"), "Friend request");
        assert_eq!(
            notification_kind_label("invite-response"),
            "Invite response"
        );
        assert_eq!(notification_kind_label("request-invite"), "Invite request");
        assert_eq!(
            notification_kind_label("request-invite-response"),
            "Invite request response"
        );
        assert_eq!(notification_kind_label("vote-to-kick"), "Vote to kick");
        assert_eq!(
            notification_kind_label("groupAnnouncement"),
            "Group announcement"
        );
        assert_eq!(notification_kind_label("something-else"), "Notification");
    }

    #[test]
    fn profile_view_has_moderation_matches_kind_and_target() {
        let mut profile = ProfileView::default();
        profile.user.id = uid(9);
        profile.moderations.push(vrc_api::PlayerModeration {
            target_user_id: uid(9),
            kind: "mute".to_string(),
            ..Default::default()
        });
        profile.moderations.push(vrc_api::PlayerModeration {
            target_user_id: uid(8),
            kind: "block".to_string(),
            ..Default::default()
        });
        assert!(profile.has_moderation(PlayerModerationKind::Mute));
        assert!(!profile.has_moderation(PlayerModerationKind::Block));
    }
}
