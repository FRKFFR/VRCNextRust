//! Application state merged from REST fetches and pipeline events.
//!
//! [`AppState`] is shared with front ends through `Arc<RwLock<…>>` snapshot
//! reads. Pipeline events mutate it incrementally — no REST re-fetch happens
//! per event (RULES.md §3).

use std::collections::HashMap;

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
    /// Currently online or active (set by the friends fetch and by
    /// presence events — not recomputed per render).
    pub is_online: bool,
    /// Present in the `type=friend` favorites list.
    pub is_favorite: bool,
}

impl Friend {
    /// Build a `Friend` from a `GET /friends` entry. `is_online` reflects
    /// which variant of the endpoint produced the entry.
    pub fn from_limited(user: &LimitedUser, online: bool) -> Self {
        let mut friend = Self {
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
            is_online: online,
            is_favorite: false,
        };
        friend.is_online = online && !matches!(friend.location.as_str(), "offline");
        friend
    }

    /// Build a `Friend` from a pipeline `user` object (seen only while the
    /// friend is online).
    pub fn from_user(user: &User) -> Self {
        let mut friend = Self::default();
        friend.apply_user(user);
        friend.is_online = true;
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
    }

    /// Mark offline: clear presence fields.
    pub fn mark_offline(&mut self) {
        self.is_online = false;
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
}
