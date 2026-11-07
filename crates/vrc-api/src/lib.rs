//! VRChat REST client: auth + 2FA, rate limiter, models, USER_AGENT.
//!
//! All requests share one send path with a token-bucket rate limiter, capped
//! exponential backoff, `Retry-After` handling and automatic auth-state
//! cleanup on 401 (RULES.md §3). Only auth cookies are persisted, through a
//! pluggable [`CookieVault`] (RULES.md §4).

/// HTTP User-Agent used for every REST and WebSocket request.
///
/// Defined exactly once here (RULES.md §2). Format:
/// `VRCNextRust/<version> (https://github.com/FRKFFR/VRCNextRust)`
pub const USER_AGENT: &str = concat!(
    "VRCNextRust/",
    env!("CARGO_PKG_VERSION"),
    " (https://github.com/FRKFFR/VRCNextRust)"
);

mod auth;
mod avatars;
mod calendar;
mod client;
mod error;
mod favorites;
mod friends;
mod groups;
mod instances;
mod invites;
pub mod models;
mod moderation;
mod notifications;
mod query;
mod rate_limit;
mod users;
mod vault;
mod worlds;

pub use auth::LoginOutcome;
pub use avatars::{valid_avatar_id, AvatarSearchQuery};
pub use calendar::{
    valid_calendar_event_id, valid_file_id, CalendarEvent, CalendarEventDiscovery,
    CalendarEventPage, CalendarRecurrence, CalendarRecurrenceEnd, CalendarUserInterest,
    DiscoverParams, DiscoverScope, GroupCalendarParams, ResultFilter,
};
pub use client::{Client, ClientBuilder, ExternalResponse, API_ROOT};
pub use error::ApiError;
pub use favorites::valid_favorite_id;
pub use groups::{
    valid_group_gallery_id, valid_group_id, valid_group_member_id, valid_group_role_id,
};
pub use instances::{
    valid_instance_id, valid_world_segment, CreateInstanceRequest, GroupAccessType, InstanceKind,
    InstanceRegion,
};
pub use models::{
    Avatar, CurrentUser, CurrentUserPresence, Favorite, FavoriteGroup, FavoriteKind, FriendStatus,
    Group, GroupAnnouncement, GroupGallery, GroupGalleryImage, GroupInstance, GroupMember,
    GroupMemberUser, GroupPost, GroupPostPage, GroupRole, Instance, LimitedUser, Notification,
    NotificationV2, NotificationV2Response, PastDisplayName, PlayerModeration,
    PlayerModerationKind, SuccessResponse, TrustRank, TwoFactorMethod, User, UserGroup, UserNote,
    World,
};
pub use notifications::valid_notification_id;
pub use rate_limit::RateLimiter;
pub use users::valid_user_id;
pub use vault::{CookieVault, KeyringVault, MemoryVault};
pub use worlds::{valid_world_id, WorldSearchQuery};

#[cfg(test)]
mod tests {
    use super::USER_AGENT;

    #[test]
    fn user_agent_uses_package_version_and_ends_with_paren() {
        assert!(USER_AGENT.contains(env!("CARGO_PKG_VERSION")));
        assert!(USER_AGENT.ends_with(')'));
    }
}
