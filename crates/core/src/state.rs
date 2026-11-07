//! Application state merged from REST fetches and pipeline events.
//!
//! [`AppState`] is shared with front ends through `Arc<RwLock<…>>` snapshot
//! reads. Pipeline events mutate it incrementally — no REST re-fetch happens
//! per event (RULES.md §3).

use std::collections::HashMap;
use std::time::Instant;

use vrc_api::{
    Avatar, CurrentUser, Favorite, FavoriteGroup, FriendStatus, Group, GroupGalleryImage,
    GroupInstance, GroupMember, GroupPost, Instance, LimitedUser, Notification, NotificationV2,
    PlayerModeration, TrustRank, TwoFactorMethod, User, UserGroup, World,
};
use vrc_store::{FriendHistoryEntry, Stats, TimelineEntry};

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
pub(crate) use vrc_api::valid_world_id;

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

/// The user profile currently shown in the profile view, plus the social
/// state fetched alongside it (`GET /users/{id}`, `/friendStatus`,
/// `/playermoderations?targetUserId=`).
#[derive(Debug, Clone, Default)]
pub struct ProfileView {
    /// The user's public profile.
    pub user: User,
    /// Friend/request relationship vs. the current user.
    pub friend_status: FriendStatus,
    /// Moderations the current user has applied to this user.
    pub moderations: Vec<PlayerModeration>,
}

impl ProfileView {
    /// `true` when a moderation of `kind` targets this profile's user.
    pub fn has_moderation(&self, kind: vrc_api::PlayerModerationKind) -> bool {
        self.moderations
            .iter()
            .any(|m| m.target_user_id == self.user.id && m.kind() == Some(kind))
    }
}

/// Progress of a running (or just-finished) multi-invite batch.
#[derive(Debug, Clone, Default)]
pub struct MultiInviteProgress {
    /// Number of targets in the batch.
    pub total: usize,
    /// Targets attempted so far.
    pub done: usize,
    /// Invites that succeeded.
    pub succeeded: usize,
    /// Invites that failed.
    pub failed: usize,
    /// `true` while the batch is still running.
    pub running: bool,
    /// User id currently being invited, when one is in flight.
    pub current: Option<String>,
}

/// One page of personal-timeline rows plus the total match count.
#[derive(Debug, Clone, Default)]
pub struct TimelinePage {
    /// Rows on this page (newest first).
    pub entries: Vec<TimelineEntry>,
    /// Total rows matching the query's filters.
    pub total: u64,
}

/// One page of friend-history rows plus the total match count.
#[derive(Debug, Clone, Default)]
pub struct HistoryPage {
    /// Rows on this page (newest first).
    pub entries: Vec<FriendHistoryEntry>,
    /// Total rows matching the query's filters.
    pub total: u64,
}

/// One page of browse/search results (worlds, groups, avatars, favorites,
/// group members/posts/galleries/instances). `total` is `Some` only for
/// endpoints that return a `{…, total}` envelope.
#[derive(Debug, Clone)]
pub struct BrowsePage<T> {
    /// Search query or resource key this page belongs to.
    pub key: String,
    /// Zero-based offset of this page.
    pub offset: usize,
    /// Rows on this page.
    pub items: Vec<T>,
    /// Total matching rows when the endpoint reports one.
    pub total: Option<u64>,
    /// A request for this page is in flight (or debouncing).
    pub loading: bool,
}

impl<T> BrowsePage<T> {
    /// An empty loading page for `key`/`offset`.
    pub fn loading(key: impl Into<String>, offset: usize) -> Self {
        Self {
            key: key.into(),
            offset,
            items: Vec::new(),
            total: None,
            loading: true,
        }
    }

    /// Whether another page likely exists (`n`-sized full page or a known
    /// total beyond this page).
    pub fn has_more(&self, page_size: usize) -> bool {
        if let Some(total) = self.total {
            return (self.offset + self.items.len()) < total as usize;
        }
        self.items.len() == page_size
    }
}

/// Resolution state of a lazily-fetched detail object (world/group/avatar),
/// cached in-session with in-flight dedup like [`WorldName`].
#[derive(Debug, Clone, PartialEq)]
pub enum DetailState<T> {
    /// A detail request is in flight.
    Pending,
    /// The fetched detail object.
    Known(T),
    /// The fetch failed; don't retry before `retry_at`.
    Unknown {
        /// Earliest instant a retry is allowed.
        retry_at: Instant,
    },
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
    /// Latest timeline page loaded via `LoadTimeline` (`None` until the
    /// first query completes).
    pub timeline: Option<TimelinePage>,
    /// Latest friend-history page loaded via `LoadFriendHistory`.
    pub friend_history: Option<HistoryPage>,
    /// Latest statistics loaded via `LoadStats`.
    pub stats: Option<Stats>,
    /// Whether the VRChat output-log tailer is running.
    pub log_tail_active: bool,
    /// Notifications (`GET /auth/user/notifications` + pipeline pushes);
    /// `None` until the first `RefreshNotifications` completes. Pipeline
    /// events upsert into it incrementally.
    pub notifications: Option<Vec<Notification>>,
    /// NotificationV2 objects kept live from pipeline events only (there is
    /// no REST list endpoint for them in the documented API).
    pub notifications_v2: Vec<NotificationV2>,
    /// The profile shown in the profile view (`None` until `LoadProfile`).
    pub profile: Option<ProfileView>,
    /// The instance loaded via `LoadInstance` (`None` until then).
    pub instance: Option<Instance>,
    /// Multi-invite batch progress (`None` until the first batch).
    pub multi_invite: Option<MultiInviteProgress>,
    /// `GET /worlds` search/browse page.
    pub world_search: Option<BrowsePage<World>>,
    /// `GET /worlds/active` page (currently active worlds).
    pub active_worlds: Option<BrowsePage<World>>,
    /// `GET /worlds/recent` page (recently visited worlds).
    pub recent_worlds: Option<BrowsePage<World>>,
    /// `GET /groups` search page.
    pub group_search: Option<BrowsePage<Group>>,
    /// `GET /users/{me}/groups` — the user's public groups (unpaged).
    pub own_groups: Option<Vec<UserGroup>>,
    /// `GET /avatars` page (featured or own scope; no text search — the
    /// documented endpoint has no `search` parameter).
    pub avatar_search: Option<BrowsePage<Avatar>>,
    /// `GET /worlds/favorites` page.
    pub favorite_worlds: Option<BrowsePage<World>>,
    /// `GET /avatars/favorites` page.
    pub favorite_avatars: Option<BrowsePage<Avatar>>,
    /// `GET /favorite/groups` page (favorite-group sets).
    pub favorite_groups: Option<BrowsePage<FavoriteGroup>>,
    /// `/favorites?type=world|avatar|friend` records used to add/remove
    /// favorites (all pages, capped).
    pub favorite_records: Vec<Favorite>,
    /// `GET /worlds/{id}` detail cache keyed by world id.
    pub world_details: HashMap<String, DetailState<World>>,
    /// `GET /groups/{id}` detail cache keyed by group id.
    pub group_details: HashMap<String, DetailState<Group>>,
    /// `GET /avatars/{id}` detail cache keyed by avatar id.
    pub avatar_details: HashMap<String, DetailState<Avatar>>,
    /// `GET /users/{me}/avatar` — the current user's active avatar.
    pub own_avatar: Option<Avatar>,
    /// `GET /groups/{id}/members` pages keyed by group id.
    pub group_members: HashMap<String, BrowsePage<GroupMember>>,
    /// `GET /groups/{id}/posts` pages keyed by group id.
    pub group_posts: HashMap<String, BrowsePage<GroupPost>>,
    /// `GET /groups/{id}/galleries/{galleryId}` pages keyed `group:gallery`.
    pub group_gallery_images: HashMap<String, BrowsePage<GroupGalleryImage>>,
    /// `GET /groups/{id}/instances` pages keyed by group id.
    pub group_instances: HashMap<String, BrowsePage<GroupInstance>>,
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
