//! Response models for the VRChat REST API.
//!
//! Field names follow the community-documented schema (vrchat.community
//! OpenAPI spec). Every field defaults so partially populated responses still
//! decode; unknown JSON keys are ignored.

use serde::{Deserialize, Serialize};

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
    #[serde(rename = "displayName")]
    pub display_name: String,
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
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub username: String,
    #[serde(rename = "userIcon")]
    pub user_icon: String,
    pub bio: String,
    #[serde(rename = "bioLinks")]
    pub bio_links: Vec<String>,
    #[serde(rename = "pastDisplayNames")]
    pub past_display_names: Vec<PastDisplayName>,
    #[serde(rename = "hasEmail")]
    pub has_email: bool,
    #[serde(rename = "hasPendingEmail")]
    pub has_pending_email: bool,
    #[serde(rename = "obfuscatedEmail")]
    pub obfuscated_email: String,
    #[serde(rename = "obfuscatedPendingEmail")]
    pub obfuscated_pending_email: String,
    #[serde(rename = "emailVerified")]
    pub email_verified: bool,
    #[serde(rename = "hasBirthday")]
    pub has_birthday: bool,
    pub unsubscribe: bool,
    pub friends: Vec<String>,
    #[serde(rename = "friendGroupNames")]
    pub friend_group_names: Vec<String>,
    #[serde(rename = "currentAvatarImageUrl")]
    pub current_avatar_image_url: String,
    #[serde(rename = "currentAvatarThumbnailImageUrl")]
    pub current_avatar_thumbnail_image_url: String,
    #[serde(rename = "currentAvatarTags")]
    pub current_avatar_tags: Vec<String>,
    pub status: String,
    #[serde(rename = "statusDescription")]
    pub status_description: String,
    pub state: String,
    pub tags: Vec<String>,
    #[serde(rename = "developerType")]
    pub developer_type: String,
    pub last_login: String,
    pub last_platform: String,
    #[serde(rename = "isFriend")]
    pub is_friend: bool,
    #[serde(rename = "friendKey")]
    pub friend_key: String,
    #[serde(rename = "onlineFriends")]
    pub online_friends: Vec<String>,
    #[serde(rename = "activeFriends")]
    pub active_friends: Vec<String>,
    #[serde(rename = "offlineFriends")]
    pub offline_friends: Vec<String>,
    #[serde(rename = "acceptedTOSVersion")]
    pub accepted_tos_version: i32,
    #[serde(rename = "acceptedPrivacyVersion")]
    pub accepted_privacy_version: i32,
    pub date_of_birth: String,
    #[serde(rename = "profilePicOverride")]
    pub profile_pic_override: String,
    #[serde(rename = "profilePicOverrideThumbnail")]
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
}
