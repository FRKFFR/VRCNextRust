//! Application core: state, commands, events, tasks.
//!
//! [`App::spawn`] starts the core; front ends drive it only through
//! [`Command`]s, [`AppEvent`]s and [`StateHandle`] snapshots (RULES.md §7).
//! After a successful login/resume the friends list and favorites are
//! fetched once via the rate-limited `vrc-api` client, then kept current by
//! `vrc-pipeline` events — never re-fetched per event (RULES.md §3).

mod app;
pub mod browse;
mod dashboard;
mod discord;
pub mod format;
mod local;
mod media;
mod sections;
mod social;
mod state;
mod thumbs;
mod tools;

pub use app::{
    App, AppConfig, AppEvent, AppHandle, Command, CommandSender, StateHandle, LOCAL_PAGE_SIZE,
};
pub use dashboard::{dashboard, world_title, DashboardEntry};
pub use discord::{WebhookSlot, MAX_WEBHOOKS};
pub use sections::{search, sections, worlds_with_friends, FriendSections, WorldGroup};
pub use social::{
    friend_status_label, friends_in_instance, moderation_label, notification_kind_label,
    validate_multi_invite_targets, MULTI_INVITE_GAP, MULTI_INVITE_MAX_TARGETS,
};
pub use state::{
    AppState, AuthState, BrowsePage, CalendarDiscoveryUi, CalendarUi, DetailState, Friend,
    HistoryPage, MediaFolderInfo, MediaScanInfo, MultiInviteProgress, PipelineStatus, ProfileView,
    TimelinePage, WorldName,
};
pub use thumbs::{ThumbSource, ThumbnailHandle, ThumbnailImage};
pub use tools::{
    parse_param_value, valid_param_addr, ChatboxRelay, Companion, LaunchPrefs, OscUi, ToolsUi,
    CHATBOX_INTERVAL,
};

// Front ends (cli/ui-egui) only depend on vrc-core; re-export the pieces of
// the leaf crates they legitimately need.
pub use vrc_api::{
    ApiError, Avatar, CalendarEvent, CalendarEventPage, Client, ClientBuilder, CookieVault,
    CreateInstanceRequest, DiscoverScope, Favorite, FavoriteGroup, FavoriteKind, FriendStatus,
    Group, GroupAccessType, GroupGalleryImage, GroupInstance, GroupMember, GroupMemberUser,
    GroupPost, Instance, InstanceKind, InstanceRegion, KeyringVault, LoginOutcome, MemoryVault,
    Notification, NotificationV2, PlayerModeration, PlayerModerationKind, SuccessResponse,
    TrustRank, TwoFactorMethod, User, UserGroup, UserNote, World, USER_AGENT,
};
pub use vrc_logs::{default_log_dir, LogEvent, LogTailer};
pub use vrc_osc::params::ParamValue;
pub use vrc_pipeline::PipelineConfig;
pub use vrc_pipeline::PipelineEvent;
pub use vrc_store::{
    FriendEvent, FriendHistoryEntry, FriendHistoryFilter, FriendTime, MediaFilter, MediaItem,
    NewSession, Stats, StatsRange, Store, TimelineEntry, TimelineFilter, TimelineKind, WorldTime,
};
