//! Typed pipeline events and the `{"type","content"}` envelope decoder.
//!
//! The VRChat pipeline sends JSON messages whose `type` selects the event and
//! whose `content` is *either* a JSON object or a JSON-encoded string holding
//! the same object (community-documented quirk). Malformed frames are logged
//! and skipped; documented-looking types we do not model become
//! [`PipelineEvent::Unknown`].

use serde_json::Value;
use vrc_api::models::{Notification, User};

/// Events pushed by the VRChat WebSocket pipeline.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// The WebSocket (re)connected and the handshake succeeded.
    Connected,
    /// The WebSocket dropped (server close, transport error or failed
    /// handshake). The pipeline reconnects automatically; `reason` never
    /// contains secrets (RULES.md §4).
    Disconnected {
        /// Why the previous connection attempt ended.
        reason: String,
    },
    /// `friend-online`: a friend came online.
    FriendOnline {
        /// `userId` of the friend.
        user_id: String,
        /// The friend's user object.
        user: User,
    },
    /// `friend-active`: a friend is active on the website (not in-game).
    FriendActive {
        /// `userId` of the friend.
        user_id: String,
        /// The friend's user object.
        user: User,
    },
    /// `friend-offline`: a friend went offline.
    FriendOffline {
        /// `userId` of the friend.
        user_id: String,
        /// Last platform the friend was seen on, when reported.
        platform: String,
    },
    /// `friend-location`: a friend's instance changed.
    FriendLocation {
        /// `userId` of the friend.
        user_id: String,
        /// Documented location tag (`wrld_…:instance`, `private`,
        /// `traveling`, `offline`).
        location: String,
        /// Pending destination while traveling, when reported.
        traveling_to_location: String,
        /// World id extracted from the payload (`wrld_…`), when reported.
        world_id: String,
        /// The friend's user object, when the payload carries it.
        user: Option<User>,
    },
    /// `friend-update`: a friend's profile/status changed.
    FriendUpdate {
        /// `userId` of the friend.
        user_id: String,
        /// The friend's user object.
        user: User,
    },
    /// `friend-add`: a new friend was added.
    FriendAdd {
        /// `userId` of the new friend.
        user_id: String,
        /// The friend's user object.
        user: User,
    },
    /// `friend-delete`: a friend was removed.
    FriendDelete {
        /// `userId` of the removed friend.
        user_id: String,
    },
    /// `notification`: a notification was pushed.
    Notification(Notification),
    /// `user-update`: the current user's profile changed.
    UserUpdate {
        /// `userId` (the current user).
        user_id: String,
        /// The user object.
        user: User,
    },
    /// A message of a type we do not model; `raw` is the whole frame.
    Unknown {
        /// The message's `type` value.
        kind: String,
        /// The original frame text.
        raw: String,
    },
}

/// Decode one WebSocket text frame into a [`PipelineEvent`].
///
/// Returns `None` for frames that are not JSON objects with a string `type`;
/// the caller logs and skips them.
pub(crate) fn decode(text: &str) -> Option<PipelineEvent> {
    let envelope: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            tracing::warn!("pipeline: skipping malformed frame: {e}");
            return None;
        }
    };
    let kind = envelope.get("type").and_then(Value::as_str)?;
    // `content` is either an object or a JSON-encoded string of the object.
    let content = match envelope.get("content") {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(value) => value,
            Err(e) => {
                tracing::warn!("pipeline: bad string content in {kind} frame: {e}");
                Value::Null
            }
        },
        Some(value) => value.clone(),
        None => Value::Null,
    };
    Some(decode_typed(kind, &content, text))
}

fn decode_typed(kind: &str, content: &Value, raw: &str) -> PipelineEvent {
    let unknown = || PipelineEvent::Unknown {
        kind: kind.to_string(),
        raw: raw.to_string(),
    };
    let str_field = |name: &str| {
        content
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let user_id = || str_field("userId");
    // A missing/undecodable `user` degrades to a default object; friend ids
    // still land correctly.
    let user = || {
        content
            .get("user")
            .and_then(|u| {
                serde_json::from_value::<User>(u.clone())
                    .map_err(|e| {
                        tracing::warn!("pipeline: bad {kind} user payload: {e}");
                    })
                    .ok()
            })
            .unwrap_or_default()
    };
    match kind {
        "friend-online" => PipelineEvent::FriendOnline {
            user_id: user_id(),
            user: user(),
        },
        "friend-active" => PipelineEvent::FriendActive {
            user_id: user_id(),
            user: user(),
        },
        "friend-offline" => PipelineEvent::FriendOffline {
            user_id: user_id(),
            platform: str_field("platform"),
        },
        "friend-location" => PipelineEvent::FriendLocation {
            user_id: user_id(),
            location: str_field("location"),
            traveling_to_location: str_field("travelingToLocation"),
            world_id: str_field("worldId"),
            user: content
                .get("user")
                .and_then(|u| serde_json::from_value::<User>(u.clone()).ok()),
        },
        "friend-update" => PipelineEvent::FriendUpdate {
            user_id: user_id(),
            user: user(),
        },
        "friend-add" => PipelineEvent::FriendAdd {
            user_id: user_id(),
            user: user(),
        },
        "friend-delete" => PipelineEvent::FriendDelete { user_id: user_id() },
        "notification" => match serde_json::from_value::<Notification>(content.clone()) {
            Ok(notification) => PipelineEvent::Notification(notification),
            Err(e) => {
                tracing::warn!("pipeline: bad notification payload: {e}");
                unknown()
            }
        },
        "user-update" => PipelineEvent::UserUpdate {
            user_id: user_id(),
            user: user(),
        },
        _ => unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic `User` payload (RULES.md §6: hand-written, fake ids).
    const USER_JSON: &str = r#"{
        "id": "usr_00000000-0000-0000-0000-000000000002",
        "displayName": "Test Friend",
        "userIcon": "",
        "bio": "Synthetic test fixture.",
        "bioLinks": [],
        "status": "join me",
        "statusDescription": "synthetic",
        "state": "online",
        "tags": ["system_trust_basic", "system_trust_known", "language_eng"],
        "location": "wrld_00000000-0000-0000-0000-000000000001:12345~hidden(usr_1)",
        "instanceId": "12345~hidden(usr_1)",
        "worldId": "wrld_00000000-0000-0000-0000-000000000001",
        "travelingToLocation": "",
        "developerType": "none",
        "last_platform": "standalonewindows",
        "currentAvatarImageUrl": "https://api.vrchat.cloud/api/1/file/file_00000000-0000-0000-0000-000000000001/1/file",
        "currentAvatarThumbnailImageUrl": "https://api.vrchat.cloud/api/1/image/file_00000000-0000-0000-0000-000000000001/1/256",
        "currentAvatarTags": [],
        "friendKey": "friendkey_synthetic",
        "isFriend": true,
        "profilePicOverride": "",
        "profilePicOverrideThumbnail": "",
        "last_activity": "2026-01-02T03:04:05.000Z"
    }"#;

    #[test]
    fn decodes_object_form_content() {
        let raw = format!(
            r#"{{"type":"friend-online","content":{{"userId":"usr_00000000-0000-0000-0000-000000000002","user":{USER_JSON}}}}}"#
        );
        let event = decode(&raw).unwrap();
        match event {
            PipelineEvent::FriendOnline { user_id, user } => {
                assert_eq!(user_id, "usr_00000000-0000-0000-0000-000000000002");
                assert_eq!(user.display_name, "Test Friend");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn decodes_string_form_content() {
        // The documented quirk: `content` is a JSON-encoded *string*.
        let inner = format!(
            r#"{{"userId":"usr_00000000-0000-0000-0000-000000000002","user":{USER_JSON}}}"#
        );
        let escaped = serde_json::to_string(&inner).unwrap();
        let raw = format!(r#"{{"type":"friend-active","content":{escaped}}}"#);
        let event = decode(&raw).unwrap();
        match event {
            PipelineEvent::FriendActive { user_id, user } => {
                assert_eq!(user_id, "usr_00000000-0000-0000-0000-000000000002");
                assert_eq!(user.status, "join me");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn unknown_type_becomes_unknown_event() {
        let event = decode(r#"{"type":"tacos","content":{"a":1}}"#).unwrap();
        match event {
            PipelineEvent::Unknown { kind, raw } => {
                assert_eq!(kind, "tacos");
                assert!(raw.contains("tacos"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn malformed_frames_are_skipped() {
        assert!(decode("not json at all").is_none());
        assert!(decode(r#"{"content":{}}"#).is_none());
    }

    #[test]
    fn friend_offline_and_delete_decode() {
        let event = decode(
            r#"{"type":"friend-offline","content":"{\"userId\":\"usr_00000000-0000-0000-0000-000000000002\",\"platform\":\"standalonewindows\"}"}"#,
        )
        .unwrap();
        assert!(matches!(
            event,
            PipelineEvent::FriendOffline { ref platform, .. } if platform == "standalonewindows"
        ));
        let event =
            decode(r#"{"type":"friend-delete","content":{"userId":"usr_00000000-0000-0000-0000-000000000002"}}"#)
                .unwrap();
        assert!(matches!(event, PipelineEvent::FriendDelete { .. }));
    }
}
