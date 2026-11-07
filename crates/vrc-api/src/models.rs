//! Response models for the VRChat REST API.
//!
//! Field names follow the community-documented schema (vrchat.community
//! OpenAPI spec). Every field defaults so partially populated responses still
//! decode; unknown JSON keys are ignored.

use serde::{Deserialize, Deserializer, Serialize};

/// Deserialize an explicit JSON `null` as `T::default()`: `#[serde(default)]`
/// only covers a missing key, while the API returns `null` for some optional
/// fields.
fn null_to_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|v| v.unwrap_or_default())
}

/// A two-factor verification method offered by the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TwoFactorMethod {
    /// Time-based one-time code from an authenticator app (`totp`).
    Totp,
    /// One-time code sent by e-mail (`emailOtp`).
    EmailOtp,
    /// Single-use recovery code (`otp`).
    RecoveryOtp,
}

impl TwoFactorMethod {
    /// Path segment of `/auth/twofactorauth/<segment>/verify`.
    pub fn verify_path(self) -> &'static str {
        match self {
            Self::Totp => "totp",
            Self::EmailOtp => "emailotp",
            Self::RecoveryOtp => "otp",
        }
    }

    /// Map a `requiresTwoFactorAuth` entry to a method. Unknown methods are
    /// ignored so newly added API methods never break login.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "totp" => Some(Self::Totp),
            "emailOtp" | "emailotp" => Some(Self::EmailOtp),
            "otp" | "recoveryOtp" | "recoveryotp" => Some(Self::RecoveryOtp),
            _ => None,
        }
    }
}

/// One entry of `CurrentUser::past_display_names`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PastDisplayName {
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// Trust rank derived from a user's `tags` (ordered low to high).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustRank {
    Nuisance,
    #[default]
    Visitor,
    NewUser,
    User,
    KnownUser,
    TrustedUser,
    Veteran,
}

impl TrustRank {
    /// Derive the rank from the documented `system_trust_*` / `system_troll*`
    /// tags.
    pub fn from_tags(tags: &[String]) -> Self {
        let has = |needle: &str| tags.iter().any(|t| t == needle);
        if has("system_probable_troll") || has("system_troll") {
            Self::Nuisance
        } else if has("system_trust_veteran") {
            Self::Veteran
        } else if has("system_trust_trusted") {
            Self::TrustedUser
        } else if has("system_trust_known") {
            Self::KnownUser
        } else if has("system_trust_user") {
            Self::User
        } else if has("system_trust_newuser") {
            Self::NewUser
        } else {
            Self::Visitor
        }
    }
}

/// The currently authenticated user (`GET /auth/user`).
///
/// Non-secret public profile data only. `friends`/`*_friends` are user-ID
/// lists, per the documented schema.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurrentUser {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub username: String,
    #[serde(rename = "userIcon", deserialize_with = "null_to_default")]
    pub user_icon: String,
    #[serde(deserialize_with = "null_to_default")]
    pub bio: String,
    #[serde(rename = "bioLinks", deserialize_with = "null_to_default")]
    pub bio_links: Vec<String>,
    #[serde(rename = "pastDisplayNames", deserialize_with = "null_to_default")]
    pub past_display_names: Vec<PastDisplayName>,
    #[serde(rename = "hasEmail", deserialize_with = "null_to_default")]
    pub has_email: bool,
    #[serde(rename = "hasPendingEmail", deserialize_with = "null_to_default")]
    pub has_pending_email: bool,
    #[serde(rename = "emailVerified", deserialize_with = "null_to_default")]
    pub email_verified: bool,
    #[serde(rename = "hasBirthday", deserialize_with = "null_to_default")]
    pub has_birthday: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub unsubscribe: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub friends: Vec<String>,
    #[serde(rename = "friendGroupNames", deserialize_with = "null_to_default")]
    pub friend_group_names: Vec<String>,
    #[serde(rename = "currentAvatarImageUrl", deserialize_with = "null_to_default")]
    pub current_avatar_image_url: String,
    #[serde(
        rename = "currentAvatarThumbnailImageUrl",
        deserialize_with = "null_to_default"
    )]
    pub current_avatar_thumbnail_image_url: String,
    #[serde(rename = "currentAvatarTags", deserialize_with = "null_to_default")]
    pub current_avatar_tags: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(rename = "statusDescription", deserialize_with = "null_to_default")]
    pub status_description: String,
    #[serde(deserialize_with = "null_to_default")]
    pub state: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(rename = "developerType", deserialize_with = "null_to_default")]
    pub developer_type: String,
    #[serde(deserialize_with = "null_to_default")]
    pub last_login: String,
    #[serde(deserialize_with = "null_to_default")]
    pub last_platform: String,
    #[serde(rename = "isFriend", deserialize_with = "null_to_default")]
    pub is_friend: bool,
    #[serde(rename = "friendKey", deserialize_with = "null_to_default")]
    pub friend_key: String,
    #[serde(rename = "onlineFriends", deserialize_with = "null_to_default")]
    pub online_friends: Vec<String>,
    #[serde(rename = "activeFriends", deserialize_with = "null_to_default")]
    pub active_friends: Vec<String>,
    #[serde(rename = "offlineFriends", deserialize_with = "null_to_default")]
    pub offline_friends: Vec<String>,
    #[serde(rename = "acceptedTOSVersion", deserialize_with = "null_to_default")]
    pub accepted_tos_version: i32,
    #[serde(
        rename = "acceptedPrivacyVersion",
        deserialize_with = "null_to_default"
    )]
    pub accepted_privacy_version: i32,
    #[serde(deserialize_with = "null_to_default")]
    pub date_of_birth: String,
    #[serde(rename = "profilePicOverride", deserialize_with = "null_to_default")]
    pub profile_pic_override: String,
    #[serde(
        rename = "profilePicOverrideThumbnail",
        deserialize_with = "null_to_default"
    )]
    pub profile_pic_override_thumbnail: String,
    /// The current user's live location/session info (`presence` on
    /// `GET /auth/user`); `None` when absent.
    #[serde(deserialize_with = "null_to_default")]
    pub presence: Option<CurrentUserPresence>,
}

impl CurrentUser {
    /// Trust rank derived from `tags`.
    pub fn trust_rank(&self) -> TrustRank {
        TrustRank::from_tags(&self.tags)
    }
}

/// A friend-list entry (`GET /friends`) — the documented `LimitedUserFriend`
/// shape.
///
/// Public profile data only; the schema carries no e-mail fields and any
/// that appear on the wire are ignored on decode (RULES.md §4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitedUser {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub username: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(rename = "userIcon", deserialize_with = "null_to_default")]
    pub user_icon: String,
    #[serde(deserialize_with = "null_to_default")]
    pub bio: String,
    #[serde(rename = "bioLinks", deserialize_with = "null_to_default")]
    pub bio_links: Vec<String>,
    #[serde(rename = "friendKey", deserialize_with = "null_to_default")]
    pub friend_key: String,
    #[serde(rename = "friendId", deserialize_with = "null_to_default")]
    pub friend_id: String,
    #[serde(rename = "last_platform", deserialize_with = "null_to_default")]
    pub last_platform: String,
    #[serde(deserialize_with = "null_to_default")]
    pub platform: String,
    #[serde(rename = "currentAvatarImageUrl", deserialize_with = "null_to_default")]
    pub current_avatar_image_url: String,
    #[serde(
        rename = "currentAvatarThumbnailImageUrl",
        deserialize_with = "null_to_default"
    )]
    pub current_avatar_thumbnail_image_url: String,
    #[serde(rename = "currentAvatarTags", deserialize_with = "null_to_default")]
    pub current_avatar_tags: Vec<String>,
    #[serde(rename = "developerType", deserialize_with = "null_to_default")]
    pub developer_type: String,
    #[serde(deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(rename = "statusDescription", deserialize_with = "null_to_default")]
    pub status_description: String,
    #[serde(deserialize_with = "null_to_default")]
    pub state: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub location: String,
    #[serde(rename = "travelingToLocation", deserialize_with = "null_to_default")]
    pub traveling_to_location: String,
    #[serde(rename = "fallbackAvatar", deserialize_with = "null_to_default")]
    pub fallback_avatar: String,
    #[serde(rename = "profilePicOverride", deserialize_with = "null_to_default")]
    pub profile_pic_override: String,
    #[serde(
        rename = "profilePicOverrideThumbnail",
        deserialize_with = "null_to_default"
    )]
    pub profile_pic_override_thumbnail: String,
    #[serde(rename = "isFriend", deserialize_with = "null_to_default")]
    pub is_friend: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub last_login: String,
    #[serde(deserialize_with = "null_to_default")]
    pub last_activity: String,
}

impl LimitedUser {
    /// Trust rank derived from `tags`.
    pub fn trust_rank(&self) -> TrustRank {
        TrustRank::from_tags(&self.tags)
    }
}

/// A full public user object (pipeline `user` payloads, `GET /users/{id}`).
///
/// Public profile data only; e-mail fields are never modelled (RULES.md §4).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct User {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub username: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(rename = "userIcon", deserialize_with = "null_to_default")]
    pub user_icon: String,
    #[serde(deserialize_with = "null_to_default")]
    pub bio: String,
    #[serde(rename = "bioLinks", deserialize_with = "null_to_default")]
    pub bio_links: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(rename = "statusDescription", deserialize_with = "null_to_default")]
    pub status_description: String,
    #[serde(deserialize_with = "null_to_default")]
    pub state: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub location: String,
    #[serde(rename = "instanceId", deserialize_with = "null_to_default")]
    pub instance_id: String,
    #[serde(rename = "worldId", deserialize_with = "null_to_default")]
    pub world_id: String,
    #[serde(rename = "travelingToLocation", deserialize_with = "null_to_default")]
    pub traveling_to_location: String,
    #[serde(rename = "developerType", deserialize_with = "null_to_default")]
    pub developer_type: String,
    #[serde(rename = "last_platform", deserialize_with = "null_to_default")]
    pub last_platform: String,
    #[serde(deserialize_with = "null_to_default")]
    pub last_activity: String,
    #[serde(deserialize_with = "null_to_default")]
    pub last_login: String,
    #[serde(rename = "currentAvatarImageUrl", deserialize_with = "null_to_default")]
    pub current_avatar_image_url: String,
    #[serde(
        rename = "currentAvatarThumbnailImageUrl",
        deserialize_with = "null_to_default"
    )]
    pub current_avatar_thumbnail_image_url: String,
    #[serde(rename = "currentAvatarTags", deserialize_with = "null_to_default")]
    pub current_avatar_tags: Vec<String>,
    #[serde(rename = "allowAvatarCopying", deserialize_with = "null_to_default")]
    pub allow_avatar_copying: bool,
    #[serde(rename = "friendKey", deserialize_with = "null_to_default")]
    pub friend_key: String,
    #[serde(rename = "isFriend", deserialize_with = "null_to_default")]
    pub is_friend: bool,
    #[serde(rename = "profilePicOverride", deserialize_with = "null_to_default")]
    pub profile_pic_override: String,
    #[serde(
        rename = "profilePicOverrideThumbnail",
        deserialize_with = "null_to_default"
    )]
    pub profile_pic_override_thumbnail: String,
    #[serde(rename = "fallbackAvatar", deserialize_with = "null_to_default")]
    pub fallback_avatar: String,
    /// The current user's note on this user (empty when unset).
    #[serde(deserialize_with = "null_to_default")]
    pub note: String,
    /// Friend-request state reported by the API (`none`, `incoming`,
    /// `outgoing`, …).
    #[serde(rename = "friendRequestStatus", deserialize_with = "null_to_default")]
    pub friend_request_status: String,
    /// The user's pronouns, when set.
    #[serde(deserialize_with = "null_to_default")]
    pub pronouns: String,
}

impl User {
    /// Trust rank derived from `tags`.
    pub fn trust_rank(&self) -> TrustRank {
        TrustRank::from_tags(&self.tags)
    }
}

/// A world (`GET /worlds/{id}`).
///
/// Public metadata only; every field defaults so partially populated
/// responses still decode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct World {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "name", deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(rename = "authorName", deserialize_with = "null_to_default")]
    pub author_name: String,
    #[serde(rename = "authorId", deserialize_with = "null_to_default")]
    pub author_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub capacity: i32,
    #[serde(rename = "recommendedCapacity", deserialize_with = "null_to_default")]
    pub recommended_capacity: i32,
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    #[serde(rename = "thumbnailImageUrl", deserialize_with = "null_to_default")]
    pub thumbnail_image_url: String,
    #[serde(deserialize_with = "null_to_default")]
    pub url: String,
    #[serde(deserialize_with = "null_to_default")]
    pub popularity: i32,
    #[serde(deserialize_with = "null_to_default")]
    pub favorites: i32,
    #[serde(deserialize_with = "null_to_default")]
    pub occupancy: i32,
    #[serde(rename = "releaseStatus", deserialize_with = "null_to_default")]
    pub release_status: String,
    #[serde(deserialize_with = "null_to_default")]
    pub platforms: Vec<String>,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
    #[serde(rename = "publicationDate", deserialize_with = "null_to_default")]
    pub publication_date: String,
}

/// One entry of `GET /favorites` — a pointer at a favorited object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Favorite {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    /// Favorite group kind: `world`, `avatar` or `friend`.
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
    /// Id of the favorited object (user id for `friend` favorites).
    #[serde(rename = "favoriteId", deserialize_with = "null_to_default")]
    pub favorite_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub visibility: String,
}

/// A notification (`GET /auth/user/notifications`, pipeline `notification`).
///
/// `id` is `not_…` for regular notifications and `frq_…` for friend
/// requests. `details` is a JSON-encoded object on REST responses and an
/// object on WebSocket pushes; both decode into [`serde_json::Value`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notification {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    /// Notification kind: `friendRequest`, `invite`, `inviteResponse`,
    /// `message`, `requestInvite`, `requestInviteResponse`, `votetokick`.
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
    #[serde(rename = "created_at", deserialize_with = "null_to_default")]
    pub created_at: String,
    /// Type-specific payload (invite world, vote details, …); always a
    /// decoded JSON value — REST sends it string-encoded, the pipeline
    /// sends it as an object.
    #[serde(deserialize_with = "details_to_value")]
    pub details: serde_json::Value,
    #[serde(deserialize_with = "null_to_default")]
    pub message: String,
    #[serde(deserialize_with = "null_to_default")]
    pub seen: bool,
    #[serde(rename = "senderUserId", deserialize_with = "null_to_default")]
    pub sender_user_id: String,
    #[serde(rename = "senderUsername", deserialize_with = "null_to_default")]
    pub sender_username: String,
    #[serde(rename = "receiverUserId", deserialize_with = "null_to_default")]
    pub receiver_user_id: String,
}

impl Notification {
    /// `true` for incoming friend requests (accepted via
    /// `PUT …/notifications/{id}/accept`). REST reports `friendRequest`;
    /// pipeline pushes may use the kebab-case spelling.
    pub fn is_friend_request(&self) -> bool {
        matches!(self.kind.as_str(), "friendRequest" | "friend-request")
    }
}

/// `details` arrives either as a JSON string (REST) or an object
/// (WebSocket); normalise both to the decoded value.
fn details_to_value<'de, D>(deserializer: D) -> Result<serde_json::Value, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::String(s)) => {
            Ok(serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)))
        }
        Some(value) => Ok(value),
        None => Ok(serde_json::Value::Null),
    }
}

/// A response action offered on a [`NotificationV2`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationV2Response {
    /// Response kind: `delete`, `unsubscribe`, …
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
    #[serde(deserialize_with = "null_to_default")]
    pub data: String,
    #[serde(deserialize_with = "null_to_default")]
    pub icon: String,
    #[serde(deserialize_with = "null_to_default")]
    pub text: String,
}

/// A NotificationV2 object (pipeline `notification-v2`).
///
/// The newer notification system (group announcements, social pushes). All
/// fields default so partial pushes still decode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationV2 {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub version: i64,
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
    #[serde(deserialize_with = "null_to_default")]
    pub category: String,
    #[serde(rename = "isSystem", deserialize_with = "null_to_default")]
    pub is_system: bool,
    #[serde(rename = "ignoreDND", deserialize_with = "null_to_default")]
    pub ignore_dnd: bool,
    #[serde(rename = "senderUserId", deserialize_with = "null_to_default")]
    pub sender_user_id: String,
    #[serde(rename = "senderUsername", deserialize_with = "null_to_default")]
    pub sender_username: String,
    #[serde(rename = "receiverUserId", deserialize_with = "null_to_default")]
    pub receiver_user_id: String,
    #[serde(
        rename = "relatedNotificationsId",
        deserialize_with = "null_to_default"
    )]
    pub related_notifications_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub title: String,
    #[serde(deserialize_with = "null_to_default")]
    pub message: String,
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    #[serde(deserialize_with = "null_to_default")]
    pub link: String,
    #[serde(rename = "linkText", deserialize_with = "null_to_default")]
    pub link_text: String,
    #[serde(deserialize_with = "null_to_default")]
    pub responses: Vec<NotificationV2Response>,
    #[serde(rename = "expiresAt", deserialize_with = "null_to_default")]
    pub expires_at: String,
    #[serde(rename = "expiryAfterSeen", deserialize_with = "null_to_default")]
    pub expiry_after_seen: i64,
    #[serde(rename = "requireSeen", deserialize_with = "null_to_default")]
    pub require_seen: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub seen: bool,
    #[serde(rename = "canDelete", deserialize_with = "null_to_default")]
    pub can_delete: bool,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// Friend-relationship state between the current user and another user
/// (`GET /user/{userId}/friendStatus`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FriendStatus {
    #[serde(rename = "incomingRequest", deserialize_with = "null_to_default")]
    pub incoming_request: bool,
    #[serde(rename = "isFriend", deserialize_with = "null_to_default")]
    pub is_friend: bool,
    #[serde(rename = "outgoingRequest", deserialize_with = "null_to_default")]
    pub outgoing_request: bool,
}

/// The current user's note on another user (`POST /userNotes`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserNote {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(deserialize_with = "null_to_default")]
    pub note: String,
    #[serde(rename = "targetUserId", deserialize_with = "null_to_default")]
    pub target_user_id: String,
    #[serde(rename = "userId", deserialize_with = "null_to_default")]
    pub user_id: String,
}

/// Player-moderation kinds (`PlayerModerationType` in the API docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayerModerationKind {
    Block,
    HideAvatar,
    InteractOff,
    InteractOn,
    Mute,
    MuteChat,
    ShowAvatar,
    Unmute,
    UnmuteChat,
}

impl PlayerModerationKind {
    /// The documented wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::HideAvatar => "hideAvatar",
            Self::InteractOff => "interactOff",
            Self::InteractOn => "interactOn",
            Self::Mute => "mute",
            Self::MuteChat => "muteChat",
            Self::ShowAvatar => "showAvatar",
            Self::Unmute => "unmute",
            Self::UnmuteChat => "unmuteChat",
        }
    }

    /// Parse a documented kind name; unknown values return `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "block" => Self::Block,
            "hideAvatar" => Self::HideAvatar,
            "interactOff" => Self::InteractOff,
            "interactOn" => Self::InteractOn,
            "mute" => Self::Mute,
            "muteChat" => Self::MuteChat,
            "showAvatar" => Self::ShowAvatar,
            "unmute" => Self::Unmute,
            "unmuteChat" => Self::UnmuteChat,
            _ => return None,
        })
    }
}

/// One player moderation the current user has applied
/// (`GET /auth/user/playermoderations`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerModeration {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub created: String,
    #[serde(rename = "sourceDisplayName", deserialize_with = "null_to_default")]
    pub source_display_name: String,
    #[serde(rename = "sourceUserId", deserialize_with = "null_to_default")]
    pub source_user_id: String,
    #[serde(rename = "targetDisplayName", deserialize_with = "null_to_default")]
    pub target_display_name: String,
    #[serde(rename = "targetUserId", deserialize_with = "null_to_default")]
    pub target_user_id: String,
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
}

impl PlayerModeration {
    /// Parsed [`PlayerModerationKind`], when the kind is documented.
    pub fn kind(&self) -> Option<PlayerModerationKind> {
        PlayerModerationKind::parse(&self.kind)
    }
}

/// An instance (`GET /instances/{worldId}:{instanceId}`).
///
/// `users` is only populated when querying an instance owned by the
/// requesting user (documented `LimitedUserInstance` entries).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Instance {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "instanceId", deserialize_with = "null_to_default")]
    pub instance_id: String,
    #[serde(rename = "worldId", deserialize_with = "null_to_default")]
    pub world_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(rename = "secureName", deserialize_with = "null_to_default")]
    pub secure_name: String,
    #[serde(rename = "shortName", deserialize_with = "null_to_default")]
    pub short_name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub capacity: i32,
    #[serde(rename = "recommendedCapacity", deserialize_with = "null_to_default")]
    pub recommended_capacity: i32,
    #[serde(rename = "n_users", deserialize_with = "null_to_default")]
    pub n_users: i32,
    #[serde(rename = "userCount", deserialize_with = "null_to_default")]
    pub user_count: i32,
    #[serde(deserialize_with = "null_to_default")]
    pub region: String,
    #[serde(rename = "photonRegion", deserialize_with = "null_to_default")]
    pub photon_region: String,
    /// Instance kind: `public`, `friends`, `hidden`, `private`, `group`.
    #[serde(rename = "type", deserialize_with = "null_to_default")]
    pub kind: String,
    #[serde(rename = "ownerId", deserialize_with = "null_to_default")]
    pub owner_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub location: String,
    #[serde(deserialize_with = "null_to_default")]
    pub strict: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub permanent: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub active: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub full: bool,
    #[serde(rename = "queueEnabled", deserialize_with = "null_to_default")]
    pub queue_enabled: bool,
    #[serde(rename = "queueSize", deserialize_with = "null_to_default")]
    pub queue_size: i32,
    #[serde(rename = "hasCapacityForYou", deserialize_with = "null_to_default")]
    pub has_capacity_for_you: bool,
    #[serde(rename = "canRequestInvite", deserialize_with = "null_to_default")]
    pub can_request_invite: bool,
    #[serde(rename = "ageGate", deserialize_with = "null_to_default")]
    pub age_gate: bool,
    #[serde(rename = "closedAt", deserialize_with = "null_to_default")]
    pub closed_at: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    /// Occupants — populated for the requesting user's own instance only.
    #[serde(deserialize_with = "null_to_default")]
    pub users: Vec<LimitedUser>,
    /// The world this instance belongs to.
    #[serde(deserialize_with = "null_to_default")]
    pub world: World,
}

/// The `{"success": {…}}` envelope returned by action endpoints
/// (unfriend, clear notifications, unmoderate, …).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SuccessResponse {
    pub success: SuccessDetail,
}

/// Inner `success` object of [`SuccessResponse`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SuccessDetail {
    #[serde(deserialize_with = "null_to_default")]
    pub message: String,
    #[serde(rename = "status_code", deserialize_with = "null_to_default")]
    pub status_code: i32,
}

/// The `presence` object on `GET /auth/user` — the current session's live
/// location, used to resolve "my instance" for invites.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CurrentUserPresence {
    #[serde(rename = "world", deserialize_with = "null_to_default")]
    pub world: String,
    #[serde(deserialize_with = "null_to_default")]
    pub instance: String,
    #[serde(rename = "instanceType", deserialize_with = "null_to_default")]
    pub instance_type: String,
    #[serde(deserialize_with = "null_to_default")]
    pub platform: String,
    #[serde(deserialize_with = "null_to_default")]
    pub status: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(rename = "userIcon", deserialize_with = "null_to_default")]
    pub user_icon: String,
    #[serde(rename = "avatarThumbnail", deserialize_with = "null_to_default")]
    pub avatar_thumbnail: String,
    #[serde(rename = "profilePicOverride", deserialize_with = "null_to_default")]
    pub profile_pic_override: String,
    #[serde(rename = "travelingToWorld", deserialize_with = "null_to_default")]
    pub traveling_to_world: String,
    #[serde(rename = "travelingToInstance", deserialize_with = "null_to_default")]
    pub traveling_to_instance: String,
    #[serde(rename = "isRejoining", deserialize_with = "null_to_default")]
    pub is_rejoining: String,
    #[serde(deserialize_with = "null_to_default")]
    pub groups: Vec<String>,
}

/// The `GET /auth/user` envelope while 2FA is still pending. Internal to the
/// crate.
#[derive(Debug, Deserialize)]
pub(crate) struct AuthState {
    #[serde(rename = "requiresTwoFactorAuth", default)]
    pub requires_two_factor_auth: Vec<String>,
}

/// Favorite-object kinds accepted by `/favorites` and the typed favorite
/// list endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FavoriteKind {
    /// A world (`wrld_…`).
    World,
    /// An avatar (`avtr_…`).
    Avatar,
    /// A user (`usr_…`).
    Friend,
}

impl FavoriteKind {
    /// The documented `type` query/body value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Avatar => "avatar",
            Self::Friend => "friend",
        }
    }

    /// Parse a documented kind name; unknown values return `None`.
    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "world" => Self::World,
            "avatar" => Self::Avatar,
            "friend" => Self::Friend,
            _ => return None,
        })
    }
}

/// An avatar (`GET /avatars`, `/avatars/{id}`, `/users/{id}/avatar`,
/// `/avatars/favorites`).
///
/// Public metadata only; price/listing internals that are not needed for
/// browse/switch are ignored on decode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Avatar {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(rename = "authorId", deserialize_with = "null_to_default")]
    pub author_id: String,
    #[serde(rename = "authorName", deserialize_with = "null_to_default")]
    pub author_name: String,
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    #[serde(rename = "thumbnailImageUrl", deserialize_with = "null_to_default")]
    pub thumbnail_image_url: String,
    #[serde(rename = "assetUrl", deserialize_with = "null_to_default")]
    pub asset_url: String,
    #[serde(rename = "unityPackageUrl", deserialize_with = "null_to_default")]
    pub unity_package_url: String,
    #[serde(rename = "releaseStatus", deserialize_with = "null_to_default")]
    pub release_status: String,
    #[serde(deserialize_with = "null_to_default")]
    pub tags: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub featured: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub searchable: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub lock: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub version: i32,
    #[serde(rename = "created_at", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updated_at", deserialize_with = "null_to_default")]
    pub updated_at: String,
    #[serde(rename = "listingDate", deserialize_with = "null_to_default")]
    pub listing_date: String,
    #[serde(deserialize_with = "null_to_default")]
    pub performance: serde_json::Value,
    #[serde(rename = "productId", deserialize_with = "null_to_default")]
    pub product_id: String,
    #[serde(rename = "pendingUpload", deserialize_with = "null_to_default")]
    pub pending_upload: bool,
}

/// A group (`GET /groups`, `/groups/{id}`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(rename = "shortCode", deserialize_with = "null_to_default")]
    pub short_code: String,
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(rename = "iconUrl", deserialize_with = "null_to_default")]
    pub icon_url: String,
    #[serde(rename = "bannerUrl", deserialize_with = "null_to_default")]
    pub banner_url: String,
    #[serde(rename = "ownerId", deserialize_with = "null_to_default")]
    pub owner_id: String,
    #[serde(rename = "memberCount", deserialize_with = "null_to_default")]
    pub member_count: i32,
    #[serde(rename = "onlineMemberCount", deserialize_with = "null_to_default")]
    pub online_member_count: i32,
    #[serde(rename = "joinState", deserialize_with = "null_to_default")]
    pub join_state: String,
    #[serde(rename = "membershipStatus", deserialize_with = "null_to_default")]
    pub membership_status: String,
    #[serde(rename = "isSearchable", deserialize_with = "null_to_default")]
    pub is_searchable: bool,
    #[serde(rename = "isVerified", deserialize_with = "null_to_default")]
    pub is_verified: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub badges: Vec<String>,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
    #[serde(rename = "lastPostCreatedAt", deserialize_with = "null_to_default")]
    pub last_post_created_at: String,
    /// The requesting user's membership, when present (`myMember`).
    #[serde(rename = "myMember", deserialize_with = "null_to_default")]
    pub my_member: Option<GroupMember>,
    #[serde(deserialize_with = "null_to_default")]
    pub galleries: Vec<GroupGallery>,
    #[serde(deserialize_with = "null_to_default")]
    pub roles: Vec<GroupRole>,
}

/// A group gallery (`Group.galleries`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupGallery {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(rename = "membersOnly", deserialize_with = "null_to_default")]
    pub members_only: bool,
    #[serde(rename = "roleIdsToView", deserialize_with = "null_to_default")]
    pub role_ids_to_view: Vec<String>,
    #[serde(rename = "roleIdsToSubmit", deserialize_with = "null_to_default")]
    pub role_ids_to_submit: Vec<String>,
    #[serde(rename = "roleIdsToManage", deserialize_with = "null_to_default")]
    pub role_ids_to_manage: Vec<String>,
    #[serde(rename = "roleIdsToAutoApprove", deserialize_with = "null_to_default")]
    pub role_ids_to_auto_approve: Vec<String>,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// A group role (`Group.roles` when `includeRoles=true`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupRole {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub description: String,
    #[serde(deserialize_with = "null_to_default")]
    pub permissions: Vec<String>,
    #[serde(rename = "defaultRole", deserialize_with = "null_to_default")]
    pub default_role: bool,
    #[serde(rename = "isManagementRole", deserialize_with = "null_to_default")]
    pub is_management_role: bool,
    #[serde(rename = "isSelfAssignable", deserialize_with = "null_to_default")]
    pub is_self_assignable: bool,
    #[serde(rename = "requiresPurchase", deserialize_with = "null_to_default")]
    pub requires_purchase: bool,
    #[serde(rename = "requiresTwoFactor", deserialize_with = "null_to_default")]
    pub requires_two_factor: bool,
    #[serde(deserialize_with = "null_to_default")]
    pub order: i32,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// The limited user object embedded on a [`GroupMember`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupMemberUser {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "displayName", deserialize_with = "null_to_default")]
    pub display_name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub username: String,
    #[serde(rename = "iconUrl", deserialize_with = "null_to_default")]
    pub icon_url: String,
    #[serde(rename = "bannerUrl", deserialize_with = "null_to_default")]
    pub banner_url: String,
    #[serde(rename = "bannerColor", deserialize_with = "null_to_default")]
    pub banner_color: String,
    #[serde(rename = "bannerType", deserialize_with = "null_to_default")]
    pub banner_type: String,
    #[serde(rename = "iconFrame", deserialize_with = "null_to_default")]
    pub icon_frame: String,
    #[serde(rename = "nameplateEffect", deserialize_with = "null_to_default")]
    pub nameplate_effect: String,
    #[serde(rename = "profileEffect", deserialize_with = "null_to_default")]
    pub profile_effect: String,
}

/// A group membership (`GET /groups/{id}/members`, join/leave responses).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupMember {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(rename = "userId", deserialize_with = "null_to_default")]
    pub user_id: String,
    #[serde(rename = "membershipStatus", deserialize_with = "null_to_default")]
    pub membership_status: String,
    #[serde(rename = "roleIds", deserialize_with = "null_to_default")]
    pub role_ids: Vec<String>,
    #[serde(rename = "mRoleIds", deserialize_with = "null_to_default")]
    pub m_role_ids: Vec<String>,
    #[serde(rename = "joinedAt", deserialize_with = "null_to_default")]
    pub joined_at: String,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "bannedAt", deserialize_with = "null_to_default")]
    pub banned_at: String,
    #[serde(rename = "hasJoinedFromPurchase", deserialize_with = "null_to_default")]
    pub has_joined_from_purchase: bool,
    #[serde(rename = "has2FA", deserialize_with = "null_to_default")]
    pub has_2fa: bool,
    #[serde(rename = "isRepresenting", deserialize_with = "null_to_default")]
    pub is_representing: bool,
    #[serde(
        rename = "isSubscribedToAnnouncements",
        deserialize_with = "null_to_default"
    )]
    pub is_subscribed_to_announcements: bool,
    #[serde(
        rename = "isSubscribedToEventAnnouncements",
        deserialize_with = "null_to_default"
    )]
    pub is_subscribed_to_event_announcements: bool,
    #[serde(rename = "lastPostReadAt", deserialize_with = "null_to_default")]
    pub last_post_read_at: String,
    #[serde(rename = "managerNotes", deserialize_with = "null_to_default")]
    pub manager_notes: String,
    #[serde(rename = "acceptedById", deserialize_with = "null_to_default")]
    pub accepted_by_id: String,
    #[serde(rename = "acceptedByDisplayName", deserialize_with = "null_to_default")]
    pub accepted_by_display_name: String,
    #[serde(deserialize_with = "null_to_default")]
    pub visibility: String,
    #[serde(deserialize_with = "null_to_default")]
    pub permissions: Vec<String>,
    #[serde(deserialize_with = "null_to_default")]
    pub user: Option<GroupMemberUser>,
}

/// A group post (`GET /groups/{id}/posts`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupPost {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(rename = "authorId", deserialize_with = "null_to_default")]
    pub author_id: String,
    #[serde(rename = "editorId", deserialize_with = "null_to_default")]
    pub editor_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub title: String,
    #[serde(deserialize_with = "null_to_default")]
    pub text: String,
    #[serde(rename = "imageId", deserialize_with = "null_to_default")]
    pub image_id: String,
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    #[serde(deserialize_with = "null_to_default")]
    pub visibility: String,
    #[serde(rename = "roleIds", deserialize_with = "null_to_default")]
    pub role_ids: Vec<String>,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
    #[serde(rename = "updatedAt", deserialize_with = "null_to_default")]
    pub updated_at: String,
}

/// The `{posts, total}` envelope returned by `GET /groups/{id}/posts`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupPostPage {
    #[serde(deserialize_with = "null_to_default")]
    pub posts: Vec<GroupPost>,
    #[serde(deserialize_with = "null_to_default")]
    pub total: i64,
}

/// One image inside a group gallery.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupGalleryImage {
    #[serde(deserialize_with = "null_to_default")]
    pub id: String,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(rename = "galleryId", deserialize_with = "null_to_default")]
    pub gallery_id: String,
    #[serde(rename = "fileId", deserialize_with = "null_to_default")]
    pub file_id: String,
    #[serde(rename = "imageUrl", deserialize_with = "null_to_default")]
    pub image_url: String,
    #[serde(rename = "submittedByUserId", deserialize_with = "null_to_default")]
    pub submitted_by_user_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub approved: bool,
    #[serde(rename = "approvedByUserId", deserialize_with = "null_to_default")]
    pub approved_by_user_id: String,
    #[serde(rename = "approvedAt", deserialize_with = "null_to_default")]
    pub approved_at: String,
    #[serde(rename = "createdAt", deserialize_with = "null_to_default")]
    pub created_at: String,
}

/// One active instance of a group (`GET /groups/{id}/instances`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupInstance {
    #[serde(rename = "instanceId", deserialize_with = "null_to_default")]
    pub instance_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub location: String,
    #[serde(rename = "memberCount", deserialize_with = "null_to_default")]
    pub member_count: i32,
    #[serde(rename = "groupId", deserialize_with = "null_to_default")]
    pub group_id: String,
    #[serde(deserialize_with = "null_to_default")]
    pub world: World,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_factor_method_parses_documented_names() {
        assert_eq!(TwoFactorMethod::parse("totp"), Some(TwoFactorMethod::Totp));
        assert_eq!(
            TwoFactorMethod::parse("emailOtp"),
            Some(TwoFactorMethod::EmailOtp)
        );
        assert_eq!(
            TwoFactorMethod::parse("otp"),
            Some(TwoFactorMethod::RecoveryOtp)
        );
        assert_eq!(TwoFactorMethod::parse("carrierPigeon"), None);
    }

    #[test]
    fn two_factor_method_maps_to_verify_paths() {
        assert_eq!(TwoFactorMethod::Totp.verify_path(), "totp");
        assert_eq!(TwoFactorMethod::EmailOtp.verify_path(), "emailotp");
        assert_eq!(TwoFactorMethod::RecoveryOtp.verify_path(), "otp");
    }

    #[test]
    fn trust_rank_picks_highest_tag() {
        let tags = vec![
            "system_trust_basic".to_string(),
            "system_trust_known".to_string(),
            "language_eng".to_string(),
        ];
        assert_eq!(TrustRank::from_tags(&tags), TrustRank::KnownUser);
        assert_eq!(TrustRank::from_tags(&[]), TrustRank::Visitor);
        assert_eq!(
            TrustRank::from_tags(&["system_probable_troll".to_string()]),
            TrustRank::Nuisance
        );
    }

    #[test]
    fn obfuscated_email_fields_are_not_stored() {
        // RULES.md §4: e-mail addresses are never stored, even obfuscated —
        // the keys are ignored on decode and absent from any serialized form.
        let user: CurrentUser = serde_json::from_str(
            r#"{"id":"usr_00000000","obfuscatedEmail":"t***@example.invalid","obfuscatedPendingEmail":"p***@example.invalid","hasEmail":true,"emailVerified":true}"#,
        )
        .unwrap();
        assert!(user.has_email && user.email_verified);
        let serialized = serde_json::to_string(&user).unwrap();
        assert!(!serialized.contains('@'), "leaked email: {serialized}");
        assert!(
            !serialized.contains("obfuscated"),
            "leaked email field: {serialized}"
        );
    }

    #[test]
    fn explicit_nulls_decode_to_defaults() {
        // The API returns null for unset optional fields; each must decode to
        // the field default instead of failing.
        let user: CurrentUser = serde_json::from_str(
            r#"{"id":"usr_00000000","bio":null,"userIcon":null,"last_platform":null,"date_of_birth":null,"friendKey":null,"profilePicOverride":null,"bioLinks":null,"tags":null,"pastDisplayNames":[{"displayName":null,"updated_at":null}],"hasEmail":null}"#,
        )
        .unwrap();
        assert_eq!(user.id, "usr_00000000");
        assert!(user.bio.is_empty() && user.user_icon.is_empty());
        assert!(user.bio_links.is_empty() && user.tags.is_empty());
        assert!(!user.has_email);
        assert_eq!(user.past_display_names.len(), 1);
        assert!(user.past_display_names[0].display_name.is_empty());
    }
}
