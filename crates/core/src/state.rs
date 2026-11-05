//! Application state merged from REST fetches and pipeline events.
//!
//! [`AppState`] is shared with front ends through `Arc<RwLock<…>>` snapshot
//! reads. Pipeline events mutate it incrementally — no REST re-fetch happens
//! per event (RULES.md §3).

use std::collections::HashMap;
use std::time::Instant;

use vrc_api::{CurrentUser, LimitedUser, TrustRank, TwoFactorMethod, User};

/// Whether a session exists and in what stage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AuthState {
    /// No session.
    #[default]
    LoggedOut,
    /// Credentials accepted; a second factor is pending.
    Needs2fa(Vec<TwoFactorMethod>),
    /// Fully authenticated.
    Authenticated,
}

/// Live status of the WebSocket pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PipelineStatus {
    /// No connection (initial state, after logout, between reconnects).
    #[default]
    Disconnected,
    /// A connection attempt is in flight.
    Connecting,
    /// Handshake succeeded; events are flowing.
    Connected,
}

/// A friend merged from `GET /friends` entries and pipeline `user` payloads.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Friend {
    /// `usr_…` id.
    pub id: String,
    /// Display name at last update.
    pub display_name: String,
    /// Login username (may be empty for limited entries).
    pub username: String,
    /// `userIcon` URL.
    pub user_icon: String,
    /// `profilePicOverride` or `currentAvatarThumbnailImageUrl`.
    pub avatar_image_url: String,
    /// `status` (`join me`, `active`, `ask me`, `busy`).
    pub status: String,
    /// `statusDescription`.
    pub status_description: String,
    /// Documented location tag (`wrld_…:instance`, `private`, `traveling`,
    /// `offline` or empty).
    pub location: String,
    /// World id (`wrld_…`) when the location carries one.
    pub world_id: String,
    /// `last_platform` (`standalonewindows`, `android`, `ios`, `web`, …).
    pub last_platform: String,
    /// `last_activity` timestamp string.
    pub last_activity: String,
    /// Trust rank derived from `tags`.
    pub trust_rank: TrustRank,
    /// `developerType`.
    pub developer_type: String,
    /// `bio`.
    pub bio: String,
    /// Presence state — `online` (in-game), `active` (web/app) or `offline`
    /// — set by the friends fetch and kept current by presence events (not
    /// recomputed per render).
    pub state: String,
    /// Present in the `type=friend` favorites list.
    pub is_favorite: bool,
}

impl Friend {
    /// True while the friend is online or active (`state != "offline"`).
    /// An unset/unknown state counts as offline.
    pub fn is_online(&self) -> bool {
        matches!(self.state.as_str(), "online" | "active")
    }

    /// Build a `Friend` from a `GET /auth/user/friends` entry. `online`
    /// reflects which variant of the endpoint produced the entry.
    pub fn from_limited(user: &LimitedUser, online: bool) -> Self {
        let state = if !online || user.location == "offline" {
            "offline".to_string()
        } else if !user.state.is_empty() {
            user.state.clone()
        } else if user.location.is_empty() {
            // On the friends list without an instance — on web/app.
            "active".to_string()
        } else {
            "online".to_string()
        };
        Self {
            id: user.id.clone(),
            display_name: user.display_name.clone(),
            username: user.username.clone(),
            user_icon: user.user_icon.clone(),
            avatar_image_url: if user.profile_pic_override.is_empty() {
                user.current_avatar_thumbnail_image_url.clone()
            } else {
                user.profile_pic_override.clone()
            },
            status: user.status.clone(),
            status_description: user.status_description.clone(),
            location: user.location.clone(),
            world_id: world_id_from(&user.location),
            last_platform: user.last_platform.clone(),
            last_activity: user.last_activity.clone(),
            trust_rank: user.trust_rank(),
            developer_type: user.developer_type.clone(),
            bio: user.bio.clone(),
            state,
            is_favorite: false,
        }
    }

    /// Build a `Friend` from a pipeline `user` object.
    pub fn from_user(user: &User) -> Self {
        let mut friend = Self::default();
        friend.apply_user(user);
        friend
    }

    /// Merge a pipeline `user` object into this entry (incremental update —
    /// no re-fetch per event, RULES.md §3).
    pub fn apply_user(&mut self, user: &User) {
        self.id = user.id.clone();
        self.username = user.username.clone();
        self.display_name = user.display_name.clone();
        self.user_icon = user.user_icon.clone();
        self.avatar_image_url = if user.profile_pic_override.is_empty() {
            user.current_avatar_thumbnail_image_url.clone()
        } else {
            user.profile_pic_override.clone()
        };
        self.status = user.status.clone();
        self.status_description = user.status_description.clone();
        if !user.location.is_empty() {
            self.location = user.location.clone();
            self.world_id = world_id_from(&user.location);
        }
        if !user.world_id.is_empty() {
            self.world_id = user.world_id.clone();
        }
        self.last_platform = user.last_platform.clone();
        self.last_activity = user.last_activity.clone();
        self.trust_rank = user.trust_rank();
        self.developer_type = user.developer_type.clone();
        self.bio = user.bio.clone();
        if !user.state.is_empty() {
            self.state = user.state.clone();
        }
    }

    /// Mark offline: clear presence fields.
    pub fn mark_offline(&mut self) {
        self.state = "offline".to_string();
        self.location.clear();
        self.world_id.clear();
    }
}

/// Extract `wrld_…` from a location tag (`wrld_x:instance` or bare `wrld_x`).
pub(crate) fn world_id_from(location: &str) -> String {
    location
        .split(':')
        .next()
        .filter(|head| head.starts_with("wrld_"))
        .unwrap_or_default()
        .to_string()
}

/// True when `id` is a plausible `wrld_…` world id, safe to interpolate
/// into a `/worlds/{id}` path segment. Pipeline payloads are remote data,
/// so ids that bypassed [`world_id_from`] are re-validated here.
pub(crate) fn valid_world_id(id: &str) -> bool {
    id.strip_prefix("wrld_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// Resolution state of a world's display name (`GET /worlds/{id}`),
/// fetched once per world id and cached here (RULES.md §3).
#[derive(Debug, Clone, PartialEq)]
pub enum WorldName {
    /// A `GET /worlds/{id}` request is in flight for this world.
    Pending,
    /// The world's display name.
    Known(String),
    /// Resolution failed; don't retry before `retry_at`.
    Unknown {
        /// Earliest instant a retry is allowed.
        retry_at: Instant,
    },
}

impl Default for WorldName {
    /// Treated like a fresh `Unknown`: eligible for an immediate fetch.
    fn default() -> Self {
        Self::Unknown {
            retry_at: Instant::now(),
        }
    }
}

/// Snapshot of everything the UI/CLI renders.
#[derive(Debug, Clone, Default)]
pub struct AppState {
    /// Authentication stage.
    pub auth: AuthState,
    /// The authenticated user (`None` while logged out).
    pub current_user: Option<CurrentUser>,
    /// Friends keyed by `usr_…` id.
    pub friends: HashMap<String, Friend>,
    /// Pipeline connectivity.
    pub pipeline: PipelineStatus,
    /// Most recent command error, if any (never secret material).
    pub last_error: Option<String>,
    /// World display names keyed by `wrld_…` id, resolved lazily by the
    /// dashboard's name-resolution task.
    pub world_names: HashMap<String, WorldName>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_id_from_extracts_wrld_head_only() {
        assert_eq!(world_id_from("wrld_abc:12345"), "wrld_abc");
        assert_eq!(world_id_from("wrld_abc"), "wrld_abc");
        assert_eq!(world_id_from("private"), "");
        assert_eq!(world_id_from(""), "");
    }

    #[test]
    fn valid_world_id_accepts_only_safe_wrld_ids() {
        assert!(valid_world_id("wrld_00000000-0000-0000-0000-00000000000a"));
        assert!(!valid_world_id("wrld_"));
        assert!(!valid_world_id("usr_00000000-0000-0000-0000-000000000001"));
        assert!(!valid_world_id("private"));
        assert!(!valid_world_id("wrld_x/../auth/user"));
        assert!(!valid_world_id("wrld_x?y=1"));
        assert!(!valid_world_id("wrld_x y"));
    }
}
