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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TrustRank {
    Nuisance,
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
}

impl CurrentUser {
    /// Trust rank derived from `tags`.
    pub fn trust_rank(&self) -> TrustRank {
        TrustRank::from_tags(&self.tags)
    }
}

/// The `GET /auth/user` envelope while 2FA is still pending. Internal to the
/// crate.
#[derive(Debug, Deserialize)]
pub(crate) struct AuthState {
    #[serde(rename = "requiresTwoFactorAuth", default)]
    pub requires_two_factor_auth: Vec<String>,
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
