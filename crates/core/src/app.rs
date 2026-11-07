//! The application core: command loop, pipeline forwarding, state updates.
//!
//! [`App::spawn`] starts a dedicated Tokio runtime on its own thread; front
//! ends send [`Command`]s over an `mpsc` channel, subscribe to [`AppEvent`]s
//! via `broadcast`, and read [`AppState`] snapshots from [`StateHandle`].
//! After authentication the core fetches friends + favorites *once* and then
//! maintains `AppState.friends` incrementally from pipeline events
//! (RULES.md §3). Secrets live only as `SecretString` inside commands and
//! are never logged (RULES.md §4).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot};
use vrc_api::{
    ApiError, AvatarSearchQuery, Client, CreateInstanceRequest, CurrentUser, DiscoverParams,
    DiscoverScope, FavoriteKind, GroupCalendarParams, LoginOutcome, PlayerModerationKind,
    TwoFactorMethod, WorldSearchQuery,
};
use vrc_logs::{LogEvent, LogTailer};
use vrc_osc::params::ParamValue;
use vrc_osc::{chatbox as osc_chatbox, OscClient, OscListener};
use vrc_pipeline::{Pipeline, PipelineConfig, PipelineEvent, PipelineHandle};
use vrc_store::{FriendHistoryFilter, MediaFilter, StatsRange, Store, TimelineFilter};

use crate::browse::{self, SearchKind, BROWSE_PAGE_SIZE, DETAIL_RETRY, SEARCH_DEBOUNCE};
use crate::discord::{self, WebhookEntry, WebhookVault};
use crate::local::{self, StoreOp};
use crate::media;
use crate::sections::worlds_with_friends;
use crate::social::{self, MULTI_INVITE_GAP};
use crate::state::{
    valid_world_id, AppState, AuthState, BrowsePage, CalendarDiscoveryUi, DetailState, Friend,
    MultiInviteProgress, PipelineStatus, ProfileView, WorldName,
};
use crate::thumbs::{self, ThumbSource, ThumbnailHandle, ThumbnailImage};
use crate::tools::{self, ChatboxRelay, Companion, LaunchPrefs};

/// Static configuration for [`App::spawn`].
#[derive(Debug)]
pub struct AppConfig {
    /// The shared rate-limited REST client (with vault, base url).
    pub client: Client,
    /// Pipeline connection tuning; override `url` in tests.
    pub pipeline: PipelineConfig,
    /// Directory that holds `vrc.db`; `None` → the platform default data
    /// directory (`directories`, application name `VRCNextRust`).
    pub data_dir: Option<PathBuf>,
    /// VRChat `output_log_*.txt` directory to tail; `None` →
    /// [`vrc_logs::default_log_dir`]. A path that does not exist disables
    /// tailing (used by headless tests).
    pub log_dir: Option<PathBuf>,
    /// A ready-made store — tests inject `Store::open_in_memory()`; `None`
    /// → open `<data_dir>/vrc.db`.
    pub store: Option<Store>,
    /// Whether this instance owns local-data writes: it tails the VRChat
    /// log, persists timeline/session/friend-history rows, and retires
    /// orphaned sessions at startup.
    ///
    /// `false` for short-lived CLI instances sharing `vrc.db` with the
    /// desktop app — a second writer would close sessions owned by the
    /// primary instance and duplicate every row it parses.
    pub local_writes: bool,
}

impl AppConfig {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            pipeline: PipelineConfig::default(),
            data_dir: None,
            log_dir: None,
            store: None,
            local_writes: true,
        }
    }
}

/// Channel type for sending [`Command`]s into the core.
pub type CommandSender = mpsc::Sender<Command>;

/// Cheaply cloneable read access to [`AppState`].
#[derive(Clone)]
pub struct StateHandle {
    inner: Arc<RwLock<AppState>>,
}

impl StateHandle {
    /// A consistent clone of the current state for rendering.
    pub fn snapshot(&self) -> AppState {
        self.read().clone()
    }

    pub(crate) fn read(&self) -> std::sync::RwLockReadGuard<'_, AppState> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, AppState> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Test-only empty state handle for unit tests in sibling modules.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        Self {
            inner: Arc::new(RwLock::new(AppState::default())),
        }
    }
}

/// Handle to the running application core.
///
/// Dropping the last `commands` sender ends the core thread.
pub struct AppHandle {
    /// Send user actions into the core.
    pub commands: CommandSender,
    /// Event stream for subscribers (already subscribed at spawn time).
    pub events: broadcast::Receiver<AppEvent>,
    /// Snapshot read access to [`AppState`].
    pub state: StateHandle,
    /// Shared decoded-thumbnail cache — front ends upload the RGBA
    /// payloads to their own texture store (M6).
    pub thumbnails: ThumbnailHandle,
}

/// Every user action is a `Command` (RULES.md §7).
pub enum Command {
    /// `GET /auth/user` with Basic credentials; on `Needs2fa` the state
    /// changes to [`AuthState::Needs2fa`].
    Login {
        /// Account e-mail/username.
        username: String,
        /// Password, kept secret and dropped right after the call.
        password: SecretString,
    },
    /// `POST /auth/twofactorauth/<method>/verify` after `Needs2fa`.
    Submit2fa {
        /// One of the methods reported by `Login`.
        method: TwoFactorMethod,
        /// One-time code, kept secret.
        code: SecretString,
    },
    /// Try to resume a session from the vault's persisted cookies.
    ResumeSession,
    /// `PUT /logout`, then clear every trace of the session.
    Logout,
    /// Re-fetch `GET /auth/user`.
    RefreshCurrentUser,
    /// Re-fetch the friends list and favorites (the only re-fetch; pipeline
    /// events otherwise keep friends current).
    RefreshFriends,
    /// Load a page of the personal timeline (parsed log events) into
    /// `AppState.timeline`.
    LoadTimeline {
        /// Zero-based page index (multiplied by the page size).
        page: u32,
        /// Kind/text filters; the paging fields are derived from `page`.
        filter: TimelineFilter,
    },
    /// Load a page of friend history (pipeline events) into
    /// `AppState.friend_history`.
    LoadFriendHistory {
        /// Zero-based page index (multiplied by the page size).
        page: u32,
        /// Friend/text filters (`user_id` selects one friend).
        filter: FriendHistoryFilter,
    },
    /// Compute time-spent statistics into `AppState.stats`.
    LoadStats {
        /// Time bounds; `StatsRange::default()` covers all time.
        range: StatsRange,
    },
    /// `GET /auth/user/notifications` into `AppState.notifications`.
    RefreshNotifications,
    /// `PUT …/notifications/{id}/see` — mark one notification seen.
    SeeNotification {
        /// `not_…`/`frq_…` notification id.
        id: String,
    },
    /// `PUT …/notifications/{id}/hide` — hide/delete one notification.
    HideNotification {
        /// `not_…`/`frq_…` notification id.
        id: String,
    },
    /// `PUT …/notifications/{id}/accept` — accept a friend request.
    AcceptNotification {
        /// `frq_…` friend-request notification id.
        id: String,
    },
    /// `PUT …/notifications/clear` — clear **all** notifications.
    ClearNotifications,
    /// Load a user profile (`GET /users/{id}` + `friendStatus` +
    /// `playermoderations`) into `AppState.profile`.
    LoadProfile {
        /// `usr_…` id of the user to inspect.
        user_id: String,
    },
    /// `POST /userNotes` — set the note on `AppState.profile`'s user.
    SetUserNote {
        /// `usr_…` id of the profile's user.
        user_id: String,
        /// Note text.
        note: String,
    },
    /// `POST /user/{id}/friendRequest`.
    SendFriendRequest {
        /// `usr_…` target id.
        user_id: String,
    },
    /// `DELETE /user/{id}/friendRequest` — cancel an outgoing request.
    CancelFriendRequest {
        /// `usr_…` target id.
        user_id: String,
    },
    /// `DELETE /auth/user/friends/{id}` — remove a friend. Destructive:
    /// requires `confirm: true`.
    Unfriend {
        /// `usr_…` target id.
        user_id: String,
        /// Must be `true`; anything else is rejected without an API call.
        confirm: bool,
    },
    /// `POST /auth/user/playermoderations` — block/mute/etc. `Block` is
    /// destructive and requires `confirm: true`; the other kinds do not.
    ModerateUser {
        /// `usr_…` target id.
        user_id: String,
        /// Moderation kind (`block`, `mute`, `hideAvatar`, …).
        kind: PlayerModerationKind,
        /// Required `true` for `Block`; ignored otherwise.
        confirm: bool,
    },
    /// `PUT /auth/user/unplayermoderate` — undo a moderation.
    UnmoderateUser {
        /// `usr_…` target id.
        user_id: String,
        /// Moderation kind to remove.
        kind: PlayerModerationKind,
    },
    /// `POST /invite/{userId}` — invite a user to an instance.
    InviteUser {
        /// `usr_…` target id.
        user_id: String,
        /// Instance id override; `None` → the session's own instance.
        instance_id: Option<String>,
        /// Custom invite message slot (0–11).
        message_slot: Option<u8>,
    },
    /// `POST /requestInvite/{userId}` — ask a user to invite you.
    RequestInvite {
        /// `usr_…` target id.
        user_id: String,
        /// Custom request message slot (0–11).
        request_slot: Option<u8>,
    },
    /// `POST /invite/{notificationId}/response` — reply to an invite.
    RespondToInvite {
        /// `not_…` invite notification id.
        notification_id: String,
        /// Response slot (0–11).
        response_slot: u8,
    },
    /// Throttled, cancellable batch of [`Command::InviteUser`] calls —
    /// sequential, ≥1.5 s apart, ≤50 targets.
    MultiInvite {
        /// `usr_…` target ids.
        user_ids: Vec<String>,
        /// Instance id override; `None` → the session's own instance.
        instance_id: Option<String>,
    },
    /// Stop the running multi-invite batch (finishes the in-flight invite).
    CancelMultiInvite,
    /// `GET /instances/{worldId}:{instanceId}` into `AppState.instance`.
    LoadInstance {
        /// `wrld_…` world id.
        world_id: String,
        /// Instance id.
        instance_id: String,
    },
    /// `GET /worlds` search/browse into `AppState.world_search` (debounced).
    SearchWorlds {
        /// Free-text world-name filter (empty = browse defaults).
        query: String,
        /// Zero-based offset (multiplied by the page size is *not* applied —
        /// pass `page * BROWSE_PAGE_SIZE`).
        offset: usize,
    },
    /// `GET /worlds/active` into `AppState.active_worlds` (debounced).
    SearchActiveWorlds {
        /// Free-text world-name filter (empty = browse defaults).
        query: String,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /worlds/recent` into `AppState.recent_worlds` (debounced).
    SearchRecentWorlds {
        /// Free-text world-name filter (empty = browse defaults).
        query: String,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /worlds/{id}` into `AppState.world_details` (cached).
    LoadWorld {
        /// `wrld_…` world id.
        world_id: String,
    },
    /// `GET /groups` search into `AppState.group_search` (debounced).
    SearchGroups {
        /// Free-text group-name filter.
        query: String,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /users/{me}/groups` into `AppState.own_groups`.
    LoadOwnGroups,
    /// `GET /groups/{id}` into `AppState.group_details` (cached).
    LoadGroup {
        /// `grp_…` group id.
        group_id: String,
        /// Include roles in the group object.
        include_roles: bool,
    },
    /// `POST /groups/{id}/join` then reload the group detail.
    JoinGroup {
        /// `grp_…` group id.
        group_id: String,
    },
    /// `POST /groups/{id}/leave` then reload the group detail.
    LeaveGroup {
        /// `grp_…` group id.
        group_id: String,
    },
    /// `GET /groups/{id}/members` into `AppState.group_members`.
    LoadGroupMembers {
        /// `grp_…` group id.
        group_id: String,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /groups/{id}/posts` into `AppState.group_posts`.
    LoadGroupPosts {
        /// `grp_…` group id.
        group_id: String,
        /// Zero-based offset.
        offset: usize,
        /// Public posts only.
        public_only: bool,
    },
    /// `GET /groups/{id}/galleries/{galleryId}` into
    /// `AppState.group_gallery_images`.
    LoadGroupGalleryImages {
        /// `grp_…` group id.
        group_id: String,
        /// `ggal_…` gallery id.
        gallery_id: String,
        /// Zero-based offset.
        offset: usize,
        /// Approved filter (`None` = all).
        approved: Option<bool>,
    },
    /// `GET /groups/{id}/instances` into `AppState.group_instances`.
    LoadGroupInstances {
        /// `grp_…` group id.
        group_id: String,
    },
    /// `GET /avatars` page into `AppState.avatar_search` (debounced).
    /// `own` selects `user=me`; otherwise `featured=true`. The documented
    /// endpoint has no text search, so there is no query field.
    LoadAvatars {
        /// Zero-based offset.
        offset: usize,
        /// List own avatars instead of featured ones.
        own: bool,
    },
    /// `GET /avatars/{id}` into `AppState.avatar_details` (cached).
    LoadAvatar {
        /// `avtr_…` avatar id.
        avatar_id: String,
    },
    /// `GET /users/{me}/avatar` into `AppState.own_avatar`.
    LoadOwnAvatar,
    /// `PUT /avatars/{id}/select` — switch avatar. Requires `confirm: true`
    /// (UI shows a confirmation dialog first).
    SelectAvatar {
        /// `avtr_…` avatar id.
        avatar_id: String,
        /// Must be `true`; anything else is rejected without an API call.
        confirm: bool,
    },
    /// Load world/avatar favorites + favorite records into state.
    RefreshFavorites,
    /// `GET /worlds/favorites` page into `AppState.favorite_worlds`.
    LoadFavoriteWorlds {
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /avatars/favorites` page into `AppState.favorite_avatars`.
    LoadFavoriteAvatars {
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /favorite/groups` page into `AppState.favorite_groups`.
    LoadFavoriteGroups {
        /// Zero-based offset.
        offset: usize,
    },
    /// `POST /favorites` — favorite a world/avatar (`friend` too, though the
    /// UI only exposes world/avatar).
    AddFavorite {
        /// Favorite kind.
        kind: FavoriteKind,
        /// `wrld_…`/`avtr_…`/`usr_…` object id.
        favorite_id: String,
        /// Favorite tags.
        tags: Vec<String>,
    },
    /// `DELETE /favorites/{fvrt_…}` resolved from `kind`+`object_id`.
    RemoveFavorite {
        /// Favorite kind.
        kind: FavoriteKind,
        /// `wrld_…`/`avtr_…`/`usr_…` object id.
        object_id: String,
    },
    /// `POST /instances` — create a world/group instance into
    /// `AppState.instance`.
    CreateInstance {
        /// Validated create request.
        request: CreateInstanceRequest,
    },
    /// `POST /invite/myself/to/{world}:{instance}` — self-invite.
    InviteMyselfToInstance {
        /// `wrld_…` world id.
        world_id: String,
        /// Instance id.
        instance_id: String,
    },
    /// Kick off a background rescan of every configured media folder
    /// (local data — works while logged out).
    ScanMedia,
    /// Load a page of indexed photos into `AppState.media`.
    LoadMedia {
        /// Folder/favorite/world/date filters + paging.
        filter: MediaFilter,
    },
    /// Toggle the local favorite flag on one media row.
    SetMediaFavorite {
        /// Media row id.
        id: i64,
        /// New flag value.
        favorite: bool,
    },
    /// Register a custom media folder (persisted in settings) and
    /// rescan.
    AddMediaFolder {
        /// Absolute folder path.
        path: String,
    },
    /// Unregister a custom media folder and drop its index rows.
    RemoveMediaFolder {
        /// Folder path previously added via `AddMediaFolder`.
        path: String,
    },
    /// Persist the blur-until-hover preference (default on).
    SetMediaBlurUntilHover {
        /// `true` blurs thumbnails until hovered.
        enabled: bool,
    },
    /// Decode a thumbnail (local file or allowlisted remote URL) into
    /// the shared cache; completion emits `AppEvent::ThumbnailReady`.
    RequestThumbnail {
        /// Cache key the front end will look up.
        key: String,
        /// Where the pixels come from.
        source: ThumbSource,
    },
    /// Send one photo file to every configured Discord webhook.
    SendMediaToDiscord {
        /// Absolute photo path.
        path: String,
    },
    /// Add a Discord webhook URL (validated; stored in the OS keyring).
    AddWebhook {
        /// `https://discord.com/api/webhooks/{id}/{token}` — secret.
        url: SecretString,
        /// Auto-post new photos to this webhook.
        autopost: bool,
    },
    /// Remove the webhook at `index` (keyring order).
    RemoveWebhook {
        /// Zero-based slot index.
        index: usize,
    },
    /// Toggle auto-post for the webhook at `index`.
    SetWebhookAutopost {
        /// Zero-based slot index.
        index: usize,
        /// New flag value.
        enabled: bool,
    },
    /// Spawn VRChat with the persisted launch prefs; companions follow
    /// when auto-start is enabled. Local-only — no login required.
    LaunchVrchat,
    /// Spawn one configured companion by index.
    LaunchCompanion {
        /// Zero-based index into `AppState.tools.companions`.
        index: usize,
    },
    /// Spawn an ad-hoc executable with arguments (CLI `launch -- <path>`).
    LaunchPath {
        /// Executable path.
        path: String,
        /// Arguments passed verbatim (no shell).
        args: Vec<String>,
    },
    /// Add a companion definition (validated, persisted).
    AddCompanion {
        /// Companion to append.
        companion: Companion,
    },
    /// Remove the companion at `index` (persisted).
    RemoveCompanion {
        /// Zero-based index into `AppState.tools.companions`.
        index: usize,
    },
    /// Replace the companion at `index` (persisted).
    UpdateCompanion {
        /// Zero-based index.
        index: usize,
        /// Replacement definition.
        companion: Companion,
    },
    /// Whether companions launch together with VRChat (persisted).
    SetAutoStartCompanion {
        /// New flag value.
        enabled: bool,
    },
    /// Override the VRChat install dir (`None`/empty = Steam auto-detect,
    /// persisted).
    SetVrchatPath {
        /// Install dir containing `VRChat.exe`.
        path: Option<String>,
    },
    /// Replace the persisted launch preferences.
    SetLaunchPrefs {
        /// New preferences.
        prefs: LaunchPrefs,
    },
    /// Queue a chatbox message — throttled to one per 1.5 s, latest wins.
    SendChatbox {
        /// Message text (sanitized + clamped by `vrc-osc`).
        text: String,
        /// `true` sends at once; `false` types it out.
        immediate: bool,
    },
    /// Toggle the chatbox typing indicator (sent immediately, unthrottled).
    SendChatboxTyping {
        /// `true` = typing, `false` = finished.
        typing: bool,
    },
    /// Send one avatar parameter (`/avatar/parameters/<name>`).
    SendOscParam {
        /// Full OSC address (validated).
        address: String,
        /// Float/int/bool value.
        value: ParamValue,
    },
    /// Start the OSC listener on `127.0.0.1:port` (0 = ephemeral; the
    /// bound port lands in `AppState.tools.osc.listen_port`).
    StartOscListen {
        /// UDP port to bind.
        port: u16,
    },
    /// Stop the OSC listener.
    StopOscListen,
    /// `GET /calendar` for `date`'s month into `AppState.calendar.month`.
    LoadCalendarMonth {
        /// Documented month key; `None` = the API default (current month).
        date: Option<String>,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /calendar/discover` — first page when `cursor` is `None`,
    /// appended next page when `Some`.
    LoadCalendarDiscover {
        /// Discovery scope.
        scope: DiscoverScope,
        /// Cursor from a previous discovery response.
        cursor: Option<String>,
    },
    /// `GET /calendar/search` into `AppState.calendar.search`.
    LoadCalendarSearch {
        /// Free-text search term.
        query: String,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /calendar/{groupId}` into `AppState.calendar.group`.
    LoadGroupCalendar {
        /// `grp_…` group id.
        group_id: String,
        /// Month `date` key; `None` = API default.
        date: Option<String>,
        /// Zero-based offset.
        offset: usize,
    },
    /// `GET /calendar/{groupId}/next` into `AppState.calendar.next_events`.
    LoadGroupNextEvent {
        /// `grp_…` group id.
        group_id: String,
    },
    /// `POST /calendar/{groupId}/{calendarId}/follow`.
    FollowCalendarEvent {
        /// `grp_…` group id.
        group_id: String,
        /// `cal_…` event id.
        calendar_id: String,
        /// New follow state.
        is_following: bool,
    },
}

impl std::fmt::Debug for Command {
    /// Account identifiers and secrets are never printed (RULES.md §4):
    /// `username` is typically an e-mail; `password`/`code` are
    /// `SecretString`s.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Command::Login { .. } => "Login",
            Command::Submit2fa { .. } => "Submit2fa",
            Command::ResumeSession => "ResumeSession",
            Command::Logout => "Logout",
            Command::RefreshCurrentUser => "RefreshCurrentUser",
            Command::RefreshFriends => "RefreshFriends",
            Command::LoadTimeline { .. } => "LoadTimeline",
            Command::LoadFriendHistory { .. } => "LoadFriendHistory",
            Command::LoadStats { .. } => "LoadStats",
            Command::RefreshNotifications => "RefreshNotifications",
            Command::SeeNotification { .. } => "SeeNotification",
            Command::HideNotification { .. } => "HideNotification",
            Command::AcceptNotification { .. } => "AcceptNotification",
            Command::ClearNotifications => "ClearNotifications",
            Command::LoadProfile { .. } => "LoadProfile",
            Command::SetUserNote { .. } => "SetUserNote",
            Command::SendFriendRequest { .. } => "SendFriendRequest",
            Command::CancelFriendRequest { .. } => "CancelFriendRequest",
            Command::Unfriend { .. } => "Unfriend",
            Command::ModerateUser { .. } => "ModerateUser",
            Command::UnmoderateUser { .. } => "UnmoderateUser",
            Command::InviteUser { .. } => "InviteUser",
            Command::RequestInvite { .. } => "RequestInvite",
            Command::RespondToInvite { .. } => "RespondToInvite",
            Command::MultiInvite { .. } => "MultiInvite",
            Command::CancelMultiInvite => "CancelMultiInvite",
            Command::LoadInstance { .. } => "LoadInstance",
            Command::SearchWorlds { .. } => "SearchWorlds",
            Command::SearchActiveWorlds { .. } => "SearchActiveWorlds",
            Command::SearchRecentWorlds { .. } => "SearchRecentWorlds",
            Command::LoadWorld { .. } => "LoadWorld",
            Command::SearchGroups { .. } => "SearchGroups",
            Command::LoadOwnGroups => "LoadOwnGroups",
            Command::LoadGroup { .. } => "LoadGroup",
            Command::JoinGroup { .. } => "JoinGroup",
            Command::LeaveGroup { .. } => "LeaveGroup",
            Command::LoadGroupMembers { .. } => "LoadGroupMembers",
            Command::LoadGroupPosts { .. } => "LoadGroupPosts",
            Command::LoadGroupGalleryImages { .. } => "LoadGroupGalleryImages",
            Command::LoadGroupInstances { .. } => "LoadGroupInstances",
            Command::LoadAvatars { .. } => "LoadAvatars",
            Command::LoadAvatar { .. } => "LoadAvatar",
            Command::LoadOwnAvatar => "LoadOwnAvatar",
            Command::SelectAvatar { .. } => "SelectAvatar",
            Command::RefreshFavorites => "RefreshFavorites",
            Command::LoadFavoriteWorlds { .. } => "LoadFavoriteWorlds",
            Command::LoadFavoriteAvatars { .. } => "LoadFavoriteAvatars",
            Command::LoadFavoriteGroups { .. } => "LoadFavoriteGroups",
            Command::AddFavorite { .. } => "AddFavorite",
            Command::RemoveFavorite { .. } => "RemoveFavorite",
            Command::CreateInstance { .. } => "CreateInstance",
            Command::InviteMyselfToInstance { .. } => "InviteMyselfToInstance",
            Command::ScanMedia => "ScanMedia",
            Command::LoadMedia { .. } => "LoadMedia",
            Command::SetMediaFavorite { .. } => "SetMediaFavorite",
            Command::AddMediaFolder { .. } => "AddMediaFolder",
            Command::RemoveMediaFolder { .. } => "RemoveMediaFolder",
            Command::SetMediaBlurUntilHover { .. } => "SetMediaBlurUntilHover",
            Command::RequestThumbnail { .. } => "RequestThumbnail",
            Command::SendMediaToDiscord { .. } => "SendMediaToDiscord",
            Command::AddWebhook { .. } => "AddWebhook",
            Command::RemoveWebhook { .. } => "RemoveWebhook",
            Command::SetWebhookAutopost { .. } => "SetWebhookAutopost",
            Command::LaunchVrchat => "LaunchVrchat",
            Command::LaunchCompanion { .. } => "LaunchCompanion",
            Command::LaunchPath { .. } => "LaunchPath",
            Command::AddCompanion { .. } => "AddCompanion",
            Command::RemoveCompanion { .. } => "RemoveCompanion",
            Command::UpdateCompanion { .. } => "UpdateCompanion",
            Command::SetAutoStartCompanion { .. } => "SetAutoStartCompanion",
            Command::SetVrchatPath { .. } => "SetVrchatPath",
            Command::SetLaunchPrefs { .. } => "SetLaunchPrefs",
            Command::SendChatbox { .. } => "SendChatbox",
            Command::SendChatboxTyping { .. } => "SendChatboxTyping",
            Command::SendOscParam { .. } => "SendOscParam",
            Command::StartOscListen { .. } => "StartOscListen",
            Command::StopOscListen => "StopOscListen",
            Command::LoadCalendarMonth { .. } => "LoadCalendarMonth",
            Command::LoadCalendarDiscover { .. } => "LoadCalendarDiscover",
            Command::LoadCalendarSearch { .. } => "LoadCalendarSearch",
            Command::LoadGroupCalendar { .. } => "LoadGroupCalendar",
            Command::LoadGroupNextEvent { .. } => "LoadGroupNextEvent",
            Command::FollowCalendarEvent { .. } => "FollowCalendarEvent",
        };
        f.debug_struct(name).finish_non_exhaustive()
    }
}

/// Events broadcast to subscribers (UI, CLI).
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// [`AppState::auth`] changed.
    AuthChanged(AuthState),
    /// The friends list was (re)loaded from the REST API.
    FriendsRefreshed,
    /// A raw pipeline event; `AppState` was already updated.
    /// Boxed — `PipelineEvent` is large compared to the other variants.
    Pipeline(Box<PipelineEvent>),
    /// A world's display name finished resolving (success or failure);
    /// `AppState.world_names` was updated.
    WorldNameResolved { world_id: String },
    /// `AppState.timeline` was updated by `LoadTimeline`.
    TimelineLoaded,
    /// `AppState.friend_history` was updated by `LoadFriendHistory`.
    FriendHistoryLoaded,
    /// `AppState.stats` was updated by `LoadStats`.
    StatsLoaded,
    /// `AppState.notifications` changed (fetch or mutation).
    NotificationsChanged,
    /// `AppState.profile` changed (load or mutation).
    ProfileChanged,
    /// `AppState.instance` was updated by `LoadInstance`.
    InstanceLoaded,
    /// An invite/request-invite/respond completed successfully.
    InviteSent,
    /// `AppState.multi_invite` progress changed.
    MultiInviteProgress,
    /// Any M5 browse/detail/favorite/instance state changed.
    BrowseChanged,
    /// `AppState.media` was updated by `LoadMedia`.
    MediaLoaded,
    /// A background media scan finished (`AppState.media_last_scan`).
    MediaScanFinished,
    /// Media config changed (folders, favorites, blur, webhooks).
    MediaChanged,
    /// A thumbnail finished decoding into the shared cache.
    ThumbnailReady,
    /// Launcher/tools configuration or OSC listener state changed.
    ToolsChanged,
    /// A process was spawned (VRChat or a companion).
    ProcessLaunched,
    /// `AppState.calendar` changed.
    CalendarChanged,
    /// Startup hydration finished (settings applied, store opened) —
    /// emitted once before the command loop; lets CLI callers snapshot
    /// persisted state without racing the store thread.
    Ready,
    /// A command failed (message carries no secrets).
    Error(String),
}

/// How long a failed world-name lookup is cached before retrying.
const WORLD_NAME_RETRY: Duration = Duration::from_secs(10 * 60);

/// Rows per page for the timeline / friend-history queries.
pub const LOCAL_PAGE_SIZE: u32 = 50;

/// The application core; drives the leaf crates and serves front ends.
pub struct App;

impl App {
    /// Spawn the core on its own thread + Tokio runtime.
    pub fn spawn(config: AppConfig) -> AppHandle {
        let (commands, command_rx) = mpsc::channel(64);
        let (event_tx, events) = broadcast::channel(256);
        let state = StateHandle {
            inner: Arc::new(RwLock::new(AppState::default())),
        };
        let inner_state = state.inner.clone();
        let thumbnails = ThumbnailHandle::new();
        let runtime_thumbnails = thumbnails.clone();
        std::thread::Builder::new()
            .name("vrc-core".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("vrc-core tokio runtime");
                runtime.block_on(run(
                    config,
                    command_rx,
                    event_tx,
                    inner_state,
                    runtime_thumbnails,
                ));
            })
            .expect("spawn vrc-core thread");
        AppHandle {
            commands,
            events,
            state,
            thumbnails,
        }
    }
}

struct Runtime {
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<AppEvent>,
    state: StateHandle,
    client: Client,
    pipeline_config: PipelineConfig,
    pipeline: Option<PipelineHandle>,
    pipeline_events: (
        mpsc::UnboundedSender<PipelineEvent>,
        mpsc::UnboundedReceiver<PipelineEvent>,
    ),
    /// Sender into the dedicated store thread; `None` when no store could
    /// be opened (local data features degrade to no-ops).
    store_tx: Option<std::sync::mpsc::Sender<StoreOp>>,
    /// Parsed VRChat log events; `None` once the tailer has ended.
    log_events: Option<mpsc::UnboundedReceiver<LogEvent>>,
    /// `AppConfig::local_writes` — gates store writes (queries still work).
    local_writes: bool,
    /// Cancellation flag for the running multi-invite batch; `None` when
    /// no batch is active.
    multi_invite_cancel: Option<Arc<AtomicBool>>,
    /// Per-kind generation counters for debounced searches; a newer command
    /// bumps the counter so stale tasks drop their results.
    search_generations: std::collections::HashMap<SearchKind, Arc<AtomicU64>>,
    /// Generation counter for media page loads — same stale-drop
    /// semantics as `search_generations` (media isn't debounced, but
    /// filter changes and paging can still race on the store thread).
    media_generation: Arc<AtomicU64>,
    /// Detached tasks (multi-invite) report a dead session here; the
    /// command loop performs the actual teardown.
    session_expired_tx: mpsc::UnboundedSender<()>,
    session_expired_rx: mpsc::UnboundedReceiver<()>,
    /// Shared decoded-thumbnail cache (M6).
    thumbnails: ThumbnailHandle,
    /// Discord webhook secret store (OS keyring, memory fallback).
    webhook_vault: Arc<dyn WebhookVault>,
    /// Sequential webhook sender; `None`-safe (jobs no-op when empty).
    webhook_relay: discord::WebhookRelay,
    /// Throttled chatbox sender (M7; latest message wins, ≤1 per 1.5 s).
    chatbox_relay: ChatboxRelay,
    /// OSC listener task; `Some` while `AppState.tools.osc.listening`.
    osc_listener: Option<tokio::task::JoinHandle<()>>,
}

async fn run(
    config: AppConfig,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<AppEvent>,
    state: Arc<RwLock<AppState>>,
    thumbnails: ThumbnailHandle,
) {
    // The log directory feeds both orphan-session cleanup (its newest
    // file's mtime bounds when VRChat was last alive) and the tailer.
    let log_dir = resolve_log_dir(&config).filter(|dir| dir.is_dir());

    // Resolve + open the local store before building the runtime so a
    // broken database degrades to "no local data" instead of killing the
    // app (headless tests inject `config.store` directly).
    let store_tx = match open_store(&config) {
        Ok(Some(store)) => {
            let tx = local::spawn_store_thread(store);
            if config.local_writes {
                // Retire sessions orphaned by a previous run (tailer
                // restart / app restart) before any new session ops
                // arrive. They ended when VRChat last wrote its log —
                // closing them at app-start `now` would credit the whole
                // downtime gap (e.g. after a crash) to the session.
                let orphan_end = log_dir
                    .as_deref()
                    .and_then(vrc_logs::newest_log_mtime)
                    .map(OffsetDateTime::from)
                    .map(|mtime| mtime.min(OffsetDateTime::now_utc()))
                    .unwrap_or_else(OffsetDateTime::now_utc);
                let _ = tx.send(StoreOp::CloseAllSessions { ts: orphan_end });
            }
            Some(tx)
        }
        Ok(None) => None,
        Err(message) => {
            tracing::warn!("vrc-core: local store unavailable: {message}");
            state.write().unwrap_or_else(|e| e.into_inner()).last_error = Some(message.clone());
            let _ = events.send(AppEvent::Error(message));
            None
        }
    };

    // Start the VRChat log tailer only when this instance writes local
    // data and the directory exists.
    let log_events = if config.local_writes {
        log_dir.map(|dir| {
            let (tx, rx) = mpsc::unbounded_channel::<LogEvent>();
            tokio::spawn(async move {
                let mut stream = LogTailer::watch(dir);
                while let Some(event) = stream.recv().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });
            rx
        })
    } else {
        None
    };

    // Hydrate local media settings (folders + blur toggle). Works
    // without a store too: the platform default folder always shows.
    let settings = match &store_tx {
        Some(tx) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if tx.send(StoreOp::QuerySettings { reply: reply_tx }).is_ok() {
                reply_rx.await.ok().and_then(Result::ok)
            } else {
                None
            }
        }
        None => None,
    };
    apply_media_settings(&state, settings.as_ref());
    apply_tools_settings(&state, settings.as_ref());
    // One Steam scan at startup populates the Launcher tab's install-dir
    // status (filesystem reads only — spawn-only rules unaffected).
    let vrchat_found = tokio::task::spawn_blocking(vrc_launcher::find_vrchat)
        .await
        .ok()
        .flatten()
        .map(|dir| dir.to_string_lossy().into_owned());
    state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .tools
        .vrchat_found = vrchat_found;

    // Webhook secrets live only in the OS keyring; a missing credential
    // store degrades to a session-only memory vault.
    let webhook_vault: Arc<dyn WebhookVault> = discord::KeyringWebhookVault::new()
        .map(|vault| Arc::new(vault) as Arc<dyn WebhookVault>)
        .unwrap_or_else(|e| {
            tracing::debug!("vrc-core: OS keyring unavailable ({e}); webhooks are session-only");
            Arc::new(discord::MemoryWebhookVault::new())
        });
    let vault_for_load = webhook_vault.clone();
    let webhook_entries = tokio::task::spawn_blocking(move || vault_for_load.load())
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    state.write().unwrap_or_else(|e| e.into_inner()).webhooks =
        discord::slots_for(&webhook_entries);
    let webhook_relay = discord::WebhookRelay::spawn(
        config.client.clone(),
        webhook_vault.clone(),
        StateHandle {
            inner: state.clone(),
        },
        events.clone(),
    );

    let (session_expired_tx, session_expired_rx) = mpsc::unbounded_channel();
    // Chatbox/OSC sends target the persisted OSC input port (default 9000).
    let chatbox_target = {
        let guard = state.read().unwrap_or_else(|e| e.into_inner());
        tools::osc_send_target(&guard.tools.launch)
    };
    let mut rt = Runtime {
        commands,
        events,
        state: StateHandle {
            inner: state.clone(),
        },
        client: config.client,
        pipeline_config: config.pipeline,
        pipeline: None,
        pipeline_events: mpsc::unbounded_channel(),
        store_tx,
        log_events,
        local_writes: config.local_writes,
        multi_invite_cancel: None,
        search_generations: Default::default(),
        media_generation: Arc::new(AtomicU64::new(0)),
        session_expired_tx,
        session_expired_rx,
        thumbnails,
        webhook_vault,
        webhook_relay,
        chatbox_relay: ChatboxRelay::new(chatbox_target),
        osc_listener: None,
    };
    rt.state.write().log_tail_active = rt.log_events.is_some();
    let _ = rt.events.send(AppEvent::Ready);
    // Index the configured folders once at startup (desktop instance
    // only — CLI runs stay fast and read-only).
    if config.local_writes {
        rt.scan_media();
    }
    loop {
        tokio::select! {
            cmd = rt.commands.recv() => match cmd {
                Some(command) => rt.handle(command).await,
                None => break,
            },
            ev = rt.pipeline_events.1.recv() => {
                if let Some(event) = ev {
                    rt.persist_pipeline_event(&event);
                    rt.apply_pipeline_event(&event);
                    rt.ensure_world_names();
                    let _ = rt.events.send(AppEvent::Pipeline(Box::new(event)));
                }
            }
            expired = rt.session_expired_rx.recv() => {
                if expired.is_some() {
                    rt.session_expired();
                }
            }
            log = async {
                match rt.log_events.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match log {
                    Some(event) => rt.apply_log_event(&event),
                    // The tailer stopped for good; disable this branch.
                    None => rt.log_events = None,
                }
            }
        }
    }
    if let Some(handle) = rt.pipeline.take() {
        handle.shutdown();
    }
}

/// The configured store, or `None` when `AppConfig.store` is unset and no
/// data directory can be resolved.
fn open_store(config: &AppConfig) -> Result<Option<Store>, String> {
    if let Some(store) = &config.store {
        return Ok(Some(store.clone()));
    }
    let Some(dir) = config.data_dir.clone().or_else(default_data_dir) else {
        return Ok(None);
    };
    Store::open(&dir.join("vrc.db"))
        .map(Some)
        .map_err(|e| e.to_string())
}

/// Platform default data directory (`%APPDATA%\VRCNextRust\data` on
/// Windows, XDG data dir on Linux, …).
fn default_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "VRCNextRust").map(|dirs| dirs.data_dir().to_path_buf())
}

/// The log directory to tail: explicit config wins, otherwise the
/// platform default (`vrc_logs::default_log_dir`).
fn resolve_log_dir(config: &AppConfig) -> Option<PathBuf> {
    config.log_dir.clone().or_else(vrc_logs::default_log_dir)
}

/// Apply persisted media settings to `AppState`: custom folders merge
/// with the platform default; the blur toggle defaults to **on** when
/// the key was never written.
fn apply_media_settings(
    state: &Arc<RwLock<AppState>>,
    settings: Option<&std::collections::HashMap<String, String>>,
) {
    let custom = media::parse_custom_folders(
        settings
            .and_then(|s| s.get(media::SETTING_MEDIA_FOLDERS))
            .map(String::as_str),
    );
    let blur = settings
        .and_then(|s| s.get(media::SETTING_MEDIA_BLUR))
        .map(|v| v == "1")
        .unwrap_or(true);
    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
    guard.media_folders = media::effective_folders(&custom);
    guard.media_blur = blur;
}

/// Apply persisted tools settings to `AppState`: companion list, VRChat
/// path override, launch prefs and the auto-start flag. All local config —
/// works while logged out (M7).
fn apply_tools_settings(
    state: &Arc<RwLock<AppState>>,
    settings: Option<&std::collections::HashMap<String, String>>,
) {
    let get = |key: &str| settings.and_then(|s| s.get(key)).map(String::as_str);
    let companions = tools::parse_companions(get(tools::SETTING_COMPANIONS));
    let vrchat_path = get(tools::SETTING_VRCHAT_PATH)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string);
    let auto_start = get(tools::SETTING_AUTO_START)
        .map(|v| v == "1")
        .unwrap_or(false);
    let launch = tools::parse_launch_prefs(get(tools::SETTING_LAUNCH));
    let mut guard = state.write().unwrap_or_else(|e| e.into_inner());
    guard.tools.companions = companions;
    guard.tools.vrchat_path = vrchat_path;
    guard.tools.auto_start_companion = auto_start;
    guard.tools.launch = launch;
}

/// Mirror a follow/unfollow response into every loaded calendar list that
/// contains the event (month, search, group, discovery).
fn apply_followed_event(state: &mut AppState, event: &vrc_api::CalendarEvent) {
    fn apply(events: &mut [vrc_api::CalendarEvent], event: &vrc_api::CalendarEvent) {
        if let Some(slot) = events.iter_mut().find(|e| e.id == event.id) {
            slot.user_interest = event.user_interest.clone();
        }
    }
    if let Some((_, page)) = state.calendar.month.as_mut() {
        apply(&mut page.results, event);
    }
    if let Some((_, page)) = state.calendar.search.as_mut() {
        apply(&mut page.results, event);
    }
    if let Some((_, page)) = state.calendar.group.as_mut() {
        apply(&mut page.results, event);
    }
    if let Some(discovery) = state.calendar.discover.as_mut() {
        apply(&mut discovery.events, event);
    }
}

/// Surface a local-store failure like a command error: `last_error` for
/// the UI's error line and `AppEvent::Error` so CLI `wait_for` callers
/// see the real SQLite error instead of a bare timeout.
fn report_store_error(
    state: &StateHandle,
    events: &broadcast::Sender<AppEvent>,
    message: impl Into<String>,
) {
    let message = message.into();
    tracing::debug!("vrc-core: local store query failed: {message}");
    state.write().last_error = Some(message.clone());
    let _ = events.send(AppEvent::Error(message));
}

/// Whether a detail id still needs a fetch: unknown ids and `Unknown`
/// entries past their retry window are fetchable; `Pending`/`Known` and
/// recently-failed entries are not (in-flight dedup, RULES.md §3).
fn detail_needed<T>(map: &std::collections::HashMap<String, DetailState<T>>, id: &str) -> bool {
    match map.get(id) {
        Some(DetailState::Known(_)) | Some(DetailState::Pending) => false,
        Some(DetailState::Unknown { retry_at }) => *retry_at <= Instant::now(),
        None => true,
    }
}

/// Mark a [`BrowsePage`] slot not-loading (used on debounced-search errors).
fn clear_page_loading<T>(slot: &mut Option<BrowsePage<T>>) {
    if let Some(page) = slot.as_mut() {
        page.loading = false;
    }
}

/// Whether `page` is a loading page whose in-flight chunk is exactly
/// `offset` — the in-flight dedup check for debounced browse fetches.
/// `offset + items.len()` is the next chunk offset for both loaded and
/// loading pages (loading continuations keep the loaded items).
fn page_loading<T>(page: Option<&BrowsePage<T>>, key: &str, offset: usize) -> bool {
    page.is_some_and(|p| p.loading && p.key == key && p.offset + p.items.len() == offset)
}

/// Cache a finished thumbnail decode (or negatively cache the failure)
/// and wake subscribers so the UI can pick up `ThumbnailReady`.
fn finish_thumbnail(
    thumbs: &ThumbnailHandle,
    events: &broadcast::Sender<AppEvent>,
    key: &str,
    result: Option<ThumbnailImage>,
) {
    match result {
        Some(image) => thumbs.insert(key.to_string(), image),
        None => thumbs.fail(key),
    }
    let _ = events.send(AppEvent::ThumbnailReady);
}

/// Store the outcome of a debounced search into its `AppState` page slot.
/// Stale generations (a newer command was queued) and post-logout writes
/// are dropped; `Unauthorized` reports a dead session through
/// `session_expired_tx` like every other detached task.
#[allow(clippy::too_many_arguments)]
fn finish_search<T>(
    state: &StateHandle,
    events: &broadcast::Sender<AppEvent>,
    session_expired_tx: &mpsc::UnboundedSender<()>,
    generation: &Arc<AtomicU64>,
    expected: u64,
    key: String,
    offset: usize,
    total: Option<u64>,
    result: Result<Vec<T>, ApiError>,
    ok: impl FnOnce(&mut AppState, BrowsePage<T>),
    err: impl FnOnce(&mut AppState),
) {
    match result {
        Ok(items) => {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let page = BrowsePage {
                key,
                offset,
                items,
                total,
                loading: false,
                exhausted,
            };
            let mut state = state.write();
            // Re-check the generation *inside* the write lock: a newer
            // command may have bumped it while this task was waiting for
            // the lock, and its results must win.
            if generation.load(Ordering::Relaxed) != expected
                || state.auth != AuthState::Authenticated
            {
                return;
            }
            ok(&mut state, page);
            drop(state);
            let _ = events.send(AppEvent::BrowseChanged);
        }
        Err(ApiError::Unauthorized) => {
            let _ = session_expired_tx.send(());
        }
        Err(e) => {
            let message = e.to_string();
            let mut state = state.write();
            if generation.load(Ordering::Relaxed) != expected
                || state.auth != AuthState::Authenticated
            {
                return;
            }
            err(&mut state);
            state.last_error = Some(message.clone());
            drop(state);
            let _ = events.send(AppEvent::Error(message));
        }
    }
}

impl Runtime {
    async fn handle(&mut self, command: Command) {
        // A new attempt supersedes the previous error: clear it up front so
        // `report` below is the only writer within this command.
        self.state.write().last_error = None;
        match command {
            Command::Login { username, password } => {
                match self.client.login(&username, &password).await {
                    Ok(LoginOutcome::LoggedIn(user)) => {
                        self.after_login(*user).await;
                    }
                    Ok(LoginOutcome::Needs2fa(methods)) => {
                        self.set_auth(AuthState::Needs2fa(methods));
                    }
                    Err(e) => self.report(e),
                }
            }
            Command::Submit2fa { method, code } => {
                match self.client.verify_2fa(method, &code).await {
                    Ok(user) => self.after_login(user).await,
                    Err(e) => self.report(e),
                }
            }
            Command::ResumeSession => match self.client.resume_session().await {
                Ok(Some(user)) => self.after_login(user).await,
                Ok(None) => self.set_auth(AuthState::LoggedOut),
                Err(e) => self.report(e),
            },
            Command::Logout => {
                if let Err(e) = self.client.logout().await {
                    self.report(e);
                    return;
                }
                self.teardown_session();
                self.set_auth(AuthState::LoggedOut);
            }
            Command::RefreshCurrentUser => match self.client.current_user().await {
                Ok(user) => {
                    self.state.write().current_user = Some(user);
                }
                // A rejected session means every authenticated call fails;
                // drop it like an explicit logout.
                Err(ApiError::Unauthorized) => self.session_expired(),
                Err(e) => self.report(e),
            },
            Command::RefreshFriends => match self.refresh_friends().await {
                Ok(()) => {}
                Err(ApiError::Unauthorized) => self.session_expired(),
                Err(e) => self.report(e),
            },
            Command::LoadTimeline { page, filter } => self.load_timeline(page, filter),
            Command::LoadFriendHistory { page, filter } => self.load_friend_history(page, filter),
            Command::LoadStats { range } => self.load_stats(range),
            Command::RefreshNotifications => self.refresh_notifications().await,
            Command::SeeNotification { id } => self.see_notification(&id).await,
            Command::HideNotification { id } => self.hide_notification(&id).await,
            Command::AcceptNotification { id } => self.accept_notification(&id).await,
            Command::ClearNotifications => self.clear_notifications().await,
            Command::LoadProfile { user_id } => self.load_profile(&user_id).await,
            Command::SetUserNote { user_id, note } => self.set_user_note(&user_id, &note).await,
            Command::SendFriendRequest { user_id } => self.send_friend_request(&user_id).await,
            Command::CancelFriendRequest { user_id } => self.cancel_friend_request(&user_id).await,
            Command::Unfriend { user_id, confirm } => self.unfriend(&user_id, confirm).await,
            Command::ModerateUser {
                user_id,
                kind,
                confirm,
            } => self.moderate_user(&user_id, kind, confirm).await,
            Command::UnmoderateUser { user_id, kind } => self.unmoderate_user(&user_id, kind).await,
            Command::InviteUser {
                user_id,
                instance_id,
                message_slot,
            } => self.invite_user(&user_id, instance_id, message_slot).await,
            Command::RequestInvite {
                user_id,
                request_slot,
            } => self.request_invite(&user_id, request_slot).await,
            Command::RespondToInvite {
                notification_id,
                response_slot,
            } => {
                self.respond_to_invite(&notification_id, response_slot)
                    .await
            }
            Command::MultiInvite {
                user_ids,
                instance_id,
            } => self.start_multi_invite(user_ids, instance_id).await,
            Command::CancelMultiInvite => self.cancel_multi_invite(),
            Command::LoadInstance {
                world_id,
                instance_id,
            } => self.load_instance(&world_id, &instance_id).await,
            Command::SearchWorlds { query, offset } => {
                self.queue_search(SearchKind::Worlds, query, offset, false)
            }
            Command::SearchActiveWorlds { query, offset } => {
                self.queue_search(SearchKind::ActiveWorlds, query, offset, false)
            }
            Command::SearchRecentWorlds { query, offset } => {
                self.queue_search(SearchKind::RecentWorlds, query, offset, false)
            }
            Command::LoadWorld { world_id } => self.load_world(&world_id).await,
            Command::SearchGroups { query, offset } => {
                self.queue_search(SearchKind::Groups, query, offset, false)
            }
            Command::LoadOwnGroups => self.load_own_groups().await,
            Command::LoadGroup {
                group_id,
                include_roles,
            } => self.load_group(&group_id, include_roles).await,
            Command::JoinGroup { group_id } => self.join_group(&group_id).await,
            Command::LeaveGroup { group_id } => self.leave_group(&group_id).await,
            Command::LoadGroupMembers { group_id, offset } => {
                self.load_group_members(&group_id, offset).await
            }
            Command::LoadGroupPosts {
                group_id,
                offset,
                public_only,
            } => self.load_group_posts(&group_id, offset, public_only).await,
            Command::LoadGroupGalleryImages {
                group_id,
                gallery_id,
                offset,
                approved,
            } => {
                self.load_group_gallery_images(&group_id, &gallery_id, offset, approved)
                    .await
            }
            Command::LoadGroupInstances { group_id } => self.load_group_instances(&group_id).await,
            Command::LoadAvatars { offset, own } => {
                self.queue_search(SearchKind::Avatars, String::new(), offset, own)
            }
            Command::LoadAvatar { avatar_id } => self.load_avatar(&avatar_id).await,
            Command::LoadOwnAvatar => self.load_own_avatar().await,
            Command::SelectAvatar { avatar_id, confirm } => {
                self.select_avatar(&avatar_id, confirm).await
            }
            Command::RefreshFavorites => self.refresh_favorites().await,
            Command::LoadFavoriteWorlds { offset } => self.load_favorite_worlds(offset).await,
            Command::LoadFavoriteAvatars { offset } => self.load_favorite_avatars(offset).await,
            Command::LoadFavoriteGroups { offset } => self.load_favorite_groups(offset).await,
            Command::AddFavorite {
                kind,
                favorite_id,
                tags,
            } => self.add_favorite(kind, &favorite_id, tags).await,
            Command::RemoveFavorite { kind, object_id } => {
                self.remove_favorite(kind, &object_id).await
            }
            Command::CreateInstance { request } => self.create_instance(request).await,
            Command::InviteMyselfToInstance {
                world_id,
                instance_id,
            } => {
                self.invite_myself_to_instance(&world_id, &instance_id)
                    .await
            }
            Command::ScanMedia => self.scan_media(),
            Command::LoadMedia { filter } => self.load_media(filter),
            Command::SetMediaFavorite { id, favorite } => self.set_media_favorite(id, favorite),
            Command::AddMediaFolder { path } => self.add_media_folder(&path).await,
            Command::RemoveMediaFolder { path } => self.remove_media_folder(&path).await,
            Command::SetMediaBlurUntilHover { enabled } => self.set_media_blur(enabled).await,
            Command::RequestThumbnail { key, source } => self.request_thumbnail(key, source),
            Command::SendMediaToDiscord { path } => self.send_media_to_discord(&path),
            Command::AddWebhook { url, autopost } => self.add_webhook(url, autopost).await,
            Command::RemoveWebhook { index } => self.remove_webhook(index).await,
            Command::SetWebhookAutopost { index, enabled } => {
                self.set_webhook_autopost(index, enabled).await
            }
            Command::LaunchVrchat => self.launch_vrchat().await,
            Command::LaunchCompanion { index } => self.launch_companion(index).await,
            Command::LaunchPath { path, args } => self.launch_path(path, args).await,
            Command::AddCompanion { companion } => self.add_companion(companion).await,
            Command::RemoveCompanion { index } => self.remove_companion(index).await,
            Command::UpdateCompanion { index, companion } => {
                self.update_companion(index, companion).await
            }
            Command::SetAutoStartCompanion { enabled } => {
                self.set_auto_start_companion(enabled).await
            }
            Command::SetVrchatPath { path } => self.set_vrchat_path(path).await,
            Command::SetLaunchPrefs { prefs } => self.set_launch_prefs(prefs).await,
            Command::SendChatbox { text, immediate } => self.send_chatbox(&text, immediate),
            Command::SendChatboxTyping { typing } => self.send_chatbox_typing(typing).await,
            Command::SendOscParam { address, value } => self.send_osc_param(&address, value).await,
            Command::StartOscListen { port } => self.start_osc_listen(port).await,
            Command::StopOscListen => self.stop_osc_listen(),
            Command::LoadCalendarMonth { date, offset } => {
                self.load_calendar_month(date, offset).await
            }
            Command::LoadCalendarDiscover { scope, cursor } => {
                self.load_calendar_discover(scope, cursor).await
            }
            Command::LoadCalendarSearch { query, offset } => {
                self.load_calendar_search(&query, offset).await
            }
            Command::LoadGroupCalendar {
                group_id,
                date,
                offset,
            } => self.load_group_calendar(&group_id, date, offset).await,
            Command::LoadGroupNextEvent { group_id } => self.load_group_next_event(&group_id).await,
            Command::FollowCalendarEvent {
                group_id,
                calendar_id,
                is_following,
            } => {
                self.follow_calendar_event(&group_id, &calendar_id, is_following)
                    .await
            }
        }
    }

    /// Run a timeline query on the store thread and publish the page.
    fn load_timeline(&self, page: u32, mut filter: TimelineFilter) {
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        filter.offset = page.saturating_mul(LOCAL_PAGE_SIZE);
        filter.limit = LOCAL_PAGE_SIZE;
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(StoreOp::QueryTimeline {
                filter,
                reply: reply_tx,
            })
            .is_err()
        {
            report_store_error(&self.state, &self.events, "local store is not running");
            return;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match reply_rx.await {
                Ok(Ok(page)) => {
                    state.write().timeline = Some(page);
                    let _ = events.send(AppEvent::TimelineLoaded);
                }
                Ok(Err(message)) => report_store_error(&state, &events, message),
                Err(_) => report_store_error(&state, &events, "local store stopped responding"),
            }
        });
    }

    /// Run a friend-history query on the store thread and publish the page.
    fn load_friend_history(&self, page: u32, mut filter: FriendHistoryFilter) {
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        filter.offset = page.saturating_mul(LOCAL_PAGE_SIZE);
        filter.limit = LOCAL_PAGE_SIZE;
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(StoreOp::QueryHistory {
                filter,
                reply: reply_tx,
            })
            .is_err()
        {
            report_store_error(&self.state, &self.events, "local store is not running");
            return;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match reply_rx.await {
                Ok(Ok(page)) => {
                    state.write().friend_history = Some(page);
                    let _ = events.send(AppEvent::FriendHistoryLoaded);
                }
                Ok(Err(message)) => report_store_error(&state, &events, message),
                Err(_) => report_store_error(&state, &events, "local store stopped responding"),
            }
        });
    }

    /// Run a stats query on the store thread and publish the result.
    fn load_stats(&self, range: StatsRange) {
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(StoreOp::QueryStats {
                range,
                reply: reply_tx,
            })
            .is_err()
        {
            report_store_error(&self.state, &self.events, "local store is not running");
            return;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match reply_rx.await {
                Ok(Ok(stats)) => {
                    state.write().stats = Some(stats);
                    let _ = events.send(AppEvent::StatsLoaded);
                }
                Ok(Err(message)) => report_store_error(&state, &events, message),
                Err(_) => report_store_error(&state, &events, "local store stopped responding"),
            }
        });
    }

    /// Queue a friend-history row for a pipeline event (no-op without a
    /// store or when this instance doesn't own local-data writes). Runs
    /// *before* [`Self::apply_pipeline_event`] so the friends map still
    /// holds pre-event names for degraded payloads.
    fn persist_pipeline_event(&self, event: &PipelineEvent) {
        if !self.local_writes {
            return;
        }
        let Some(tx) = &self.store_tx else {
            return;
        };
        let state = self.state.read();
        let Some(entry) = local::history_entry_for(event, &state.friends) else {
            return;
        };
        drop(state);
        let _ = tx.send(StoreOp::History(entry));
    }

    /// Start a background rescan of every configured media folder. The
    /// `media_scanning` state flag is both the UI indicator and the
    /// in-flight guard; `run_media_scan` clears it when done.
    fn scan_media(&self) {
        if self.state.read().media_scanning {
            return;
        }
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        self.state.write().media_scanning = true;
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            media::run_media_scan(tx, state, events).await;
        });
    }

    /// Run a paged media query on the store thread and merge the page
    /// into `AppState.media` (offset accumulation like M5 browse pages).
    fn load_media(&self, mut filter: MediaFilter) {
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        if filter.limit == 0 {
            filter.limit = media::MEDIA_PAGE_SIZE;
        }
        let key = media::media_filter_key(&filter);
        let offset = filter.offset as usize;
        let page_len = filter.limit as usize;
        {
            let mut state = self.state.write();
            // In-flight dedup: an identical query already loading is
            // dropped — the sentinel will re-fire once it lands.
            if state
                .media
                .as_ref()
                .is_some_and(|p| p.loading && p.key == key)
            {
                return;
            }
            state.media = Some(browse::continuation_loading(
                state.media.as_ref(),
                &key,
                offset,
            ));
        }
        // Bump *after* the in-flight dedup: a dropped duplicate must not
        // invalidate the query it piggybacks on.
        let generation = self.media_generation.clone();
        let expected = generation.fetch_add(1, Ordering::Relaxed) + 1;
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(StoreOp::QueryMedia {
                filter,
                reply: reply_tx,
            })
            .is_err()
        {
            report_store_error(&self.state, &self.events, "local store is not running");
            return;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match reply_rx.await {
                Ok(Ok((items, total))) => {
                    let exhausted = items.len() < page_len;
                    let page = BrowsePage {
                        key,
                        offset,
                        items,
                        total: Some(total),
                        loading: false,
                        exhausted,
                    };
                    // Re-check the generation inside the write lock: a
                    // newer LoadMedia may have landed while this reply
                    // was in flight — its results must win.
                    let mut guard = state.write();
                    if generation.load(Ordering::Relaxed) != expected {
                        return;
                    }
                    browse::merge_page(&mut guard.media, page);
                    drop(guard);
                    let _ = events.send(AppEvent::MediaLoaded);
                }
                Ok(Err(message)) => report_store_error(&state, &events, message),
                Err(_) => report_store_error(&state, &events, "local store stopped responding"),
            }
        });
    }

    /// Persist a favorite toggle and reflect it into the loaded page.
    fn set_media_favorite(&self, id: i64, favorite: bool) {
        let Some(tx) = self.store_tx.clone() else {
            report_store_error(&self.state, &self.events, "local store is unavailable");
            return;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx
            .send(StoreOp::SetMediaFavorite {
                id,
                favorite,
                reply: reply_tx,
            })
            .is_err()
        {
            report_store_error(&self.state, &self.events, "local store is not running");
            return;
        }
        let state = self.state.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match reply_rx.await {
                Ok(Ok(())) => {
                    let mut guard = state.write();
                    if let Some(page) = guard.media.as_mut() {
                        if !favorite && media::media_key_favorites_only(&page.key) {
                            // Un-favoriting inside the favorites view
                            // removes the row and shrinks the total.
                            page.items.retain(|i| i.id != id);
                            page.total = page.total.map(|t| t.saturating_sub(1));
                        } else if let Some(item) = page.items.iter_mut().find(|i| i.id == id) {
                            item.is_favorite = favorite;
                        }
                    }
                    drop(guard);
                    let _ = events.send(AppEvent::MediaChanged);
                }
                Ok(Err(message)) => report_store_error(&state, &events, message),
                Err(_) => report_store_error(&state, &events, "local store stopped responding"),
            }
        });
    }

    /// The custom (user-added) folder paths currently configured.
    fn custom_folders(&self) -> Vec<String> {
        self.state
            .read()
            .media_folders
            .iter()
            .filter(|f| f.custom)
            .map(|f| f.path.clone())
            .collect()
    }

    /// FIFO barrier on the store thread: resolves once every op queued
    /// before it has been applied. Fire-and-forget writes pair with this
    /// so "persisted" events only fire after the write is durable —
    /// matters for CLI invocations that exit right after the event.
    async fn flush_store(&self) {
        let Some(tx) = self.store_tx.clone() else {
            return;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if tx.send(StoreOp::QuerySettings { reply: reply_tx }).is_ok() {
            let _ = reply_rx.await;
        }
    }

    /// Persist the custom folder list and refresh `media_folders`. The
    /// [`AppEvent::MediaChanged`] fires only after the store thread has
    /// acknowledged the write (see [`Self::flush_store`]).
    async fn persist_folders(&self, custom: &[String]) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreOp::SetSetting {
                key: media::SETTING_MEDIA_FOLDERS.to_string(),
                value: media::folders_json(custom),
            });
        }
        self.flush_store().await;
        self.state.write().media_folders = media::effective_folders(custom);
        let _ = self.events.send(AppEvent::MediaChanged);
    }

    /// Register a custom folder: validate it exists, dedup against the
    /// effective list, persist, then rescan so it gets indexed.
    async fn add_media_folder(&mut self, path: &str) {
        let path = path.trim();
        if path.is_empty() {
            self.report_message("folder path is empty");
            return;
        }
        if !Path::new(path).is_dir() {
            self.report_message("folder does not exist or is not a directory");
            return;
        }
        let already_scanned = {
            let state = self.state.read();
            state
                .media_folders
                .iter()
                .any(|f| Path::new(&f.path) == Path::new(path))
        };
        if already_scanned {
            self.report_message("folder is already in the scan list");
            return;
        }
        let mut custom = self.custom_folders();
        custom.push(path.to_string());
        self.persist_folders(&custom).await;
        self.scan_media();
    }

    /// Unregister a custom folder and drop its index rows. The platform
    /// default folder cannot be removed.
    async fn remove_media_folder(&mut self, path: &str) {
        let path = path.trim();
        // Rows store the folder string exactly as configured — capture
        // it before the folder leaves `media_folders`.
        let stored = self
            .state
            .read()
            .media_folders
            .iter()
            .find(|f| f.custom && Path::new(&f.path) == Path::new(path))
            .map(|f| f.path.clone());
        let Some(stored) = stored else {
            self.report_message("only custom folders can be removed");
            return;
        };
        let custom: Vec<String> = self
            .custom_folders()
            .into_iter()
            .filter(|f| Path::new(f) != Path::new(path))
            .collect();
        // Queue the row delete *before* `persist_folders`' barrier so
        // `MediaChanged` covers both writes.
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreOp::DeleteMediaFolder { folder: stored });
        }
        self.persist_folders(&custom).await;
    }

    /// Persist the blur-until-hover toggle (default on).
    async fn set_media_blur(&self, enabled: bool) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreOp::SetSetting {
                key: media::SETTING_MEDIA_BLUR.to_string(),
                value: if enabled { "1" } else { "0" }.to_string(),
            });
        }
        self.flush_store().await;
        self.state.write().media_blur = enabled;
        let _ = self.events.send(AppEvent::MediaChanged);
    }

    /// Claim a thumbnail decode and run it off the UI thread: local files
    /// on `spawn_blocking`, remote bytes through the shared client
    /// (allowlist + rate limiter) then `spawn_blocking` for the decode.
    fn request_thumbnail(&self, key: String, source: ThumbSource) {
        if !self.thumbnails.begin(&key) {
            return;
        }
        match source {
            ThumbSource::File(path) => {
                let thumbs = self.thumbnails.clone();
                let events = self.events.clone();
                tokio::task::spawn_blocking(move || {
                    finish_thumbnail(
                        &thumbs,
                        &events,
                        &key,
                        thumbs::decode_file(Path::new(&path)),
                    );
                });
            }
            ThumbSource::Url(url) => {
                if !media::is_allowed_image_url(&url) {
                    self.thumbnails.fail(&key);
                    return;
                }
                let client = self.client.clone();
                let thumbs = self.thumbnails.clone();
                let events = self.events.clone();
                tokio::spawn(async move {
                    let result = match client.get_bytes(&url, thumbs::MAX_REMOTE_IMAGE_BYTES).await
                    {
                        Ok(bytes) => {
                            tokio::task::spawn_blocking(move || thumbs::decode_bytes(&bytes))
                                .await
                                .ok()
                                .flatten()
                        }
                        Err(e) => {
                            tracing::debug!("remote thumbnail fetch failed: {e}");
                            None
                        }
                    };
                    finish_thumbnail(&thumbs, &events, &key, result);
                });
            }
        }
    }

    /// Queue a manual Discord send for one photo file.
    fn send_media_to_discord(&mut self, path: &str) {
        if path.trim().is_empty() {
            self.report_message("empty photo path");
            return;
        }
        self.webhook_relay.send(PathBuf::from(path), false);
    }

    /// Add a validated webhook URL to the keyring vault (cap
    /// [`discord::MAX_WEBHOOKS`]).
    async fn add_webhook(&mut self, url: SecretString, autopost: bool) {
        if let Err(message) = discord::validate_webhook_url(url.expose_secret()) {
            self.report_message(&message);
            return;
        }
        let Some(entries) = self.vault_entries().await else {
            return;
        };
        if entries.len() >= discord::MAX_WEBHOOKS {
            self.report_message(&format!(
                "webhook limit reached ({})",
                discord::MAX_WEBHOOKS
            ));
            return;
        }
        if entries
            .iter()
            .any(|e| e.url.expose_secret() == url.expose_secret())
        {
            self.report_message("that webhook is already configured");
            return;
        }
        let mut entries = entries;
        entries.push(WebhookEntry { url, autopost });
        if !self.vault_save(&entries).await {
            return;
        }
        self.state.write().webhooks = discord::slots_for(&entries);
        let _ = self.events.send(AppEvent::MediaChanged);
    }

    /// Remove the webhook at `index`.
    async fn remove_webhook(&mut self, index: usize) {
        let Some(mut entries) = self.vault_entries().await else {
            return;
        };
        if index >= entries.len() {
            self.report_message("no webhook at that index");
            return;
        }
        entries.remove(index);
        if !self.vault_save(&entries).await {
            return;
        }
        self.state.write().webhooks = discord::slots_for(&entries);
        let _ = self.events.send(AppEvent::MediaChanged);
    }

    /// Toggle auto-post on the webhook at `index`.
    async fn set_webhook_autopost(&mut self, index: usize, enabled: bool) {
        let Some(mut entries) = self.vault_entries().await else {
            return;
        };
        let Some(entry) = entries.get_mut(index) else {
            self.report_message("no webhook at that index");
            return;
        };
        entry.autopost = enabled;
        if !self.vault_save(&entries).await {
            return;
        }
        self.state.write().webhooks = discord::slots_for(&entries);
        let _ = self.events.send(AppEvent::MediaChanged);
    }

    /// Load webhook entries off the async executor (keyring may block).
    async fn vault_entries(&mut self) -> Option<Vec<WebhookEntry>> {
        let vault = self.webhook_vault.clone();
        match tokio::task::spawn_blocking(move || vault.load()).await {
            Ok(Ok(entries)) => Some(entries),
            Ok(Err(message)) => {
                self.report_message(&message);
                None
            }
            Err(_) => {
                self.report_message("keyring read failed");
                None
            }
        }
    }

    /// Persist webhook entries off the async executor.
    async fn vault_save(&mut self, entries: &[WebhookEntry]) -> bool {
        let vault = self.webhook_vault.clone();
        let entries = entries.to_vec();
        match tokio::task::spawn_blocking(move || vault.save(&entries)).await {
            Ok(Ok(())) => true,
            Ok(Err(message)) => {
                self.report_message(&message);
                false
            }
            Err(_) => {
                self.report_message("keyring write failed");
                false
            }
        }
    }

    /// Queue the store ops a parsed log event implies (timeline row +
    /// session bookkeeping).
    fn apply_log_event(&self, event: &LogEvent) {
        if !self.local_writes {
            return;
        }
        let Some(tx) = &self.store_tx else {
            return;
        };
        for op in local::ops_for_log_event(event) {
            let _ = tx.send(op);
        }
        if let LogEvent::PhotoTaken { path, .. } = event {
            // Link the photo to its media row + the world session
            // covering the timestamp, and auto-post to flagged webhooks.
            // Photos outside every scan folder get no row — the library
            // only indexes configured folders.
            let folder = self.media_folder_for(path);
            if !folder.is_empty() {
                let _ = tx.send(StoreOp::LinkPhoto {
                    path: path.clone(),
                    ts: event.at(),
                    folder,
                });
            }
            if self.state.read().webhooks.iter().any(|w| w.autopost) {
                self.webhook_relay.send(PathBuf::from(path), true);
            }
        }
    }

    /// Longest matching configured folder for a photo path.
    fn media_folder_for(&self, path: &str) -> String {
        let photo = Path::new(path);
        self.state
            .read()
            .media_folders
            .iter()
            .filter(|f| photo.starts_with(Path::new(&f.path)))
            .max_by_key(|f| f.path.len())
            .map(|f| f.path.clone())
            .unwrap_or_default()
    }

    /// Common post-authentication path: update auth state, start the
    /// pipeline with the session's `auth` cookie, fetch friends + favorites
    /// once.
    async fn after_login(&mut self, user: CurrentUser) {
        {
            let mut state = self.state.write();
            state.current_user = Some(user);
            state.auth = AuthState::Authenticated;
        }
        let _ = self
            .events
            .send(AppEvent::AuthChanged(AuthState::Authenticated));

        if let Some(handle) = self.pipeline.take() {
            handle.shutdown();
        }
        if let Some(token) = self.client.auth_token() {
            // Fresh channel per session: queued events from a previous
            // pipeline generation cannot clobber the new connection's
            // `Connecting` status.
            self.pipeline_events = mpsc::unbounded_channel();
            let (handle, stream) = Pipeline::connect(self.pipeline_config.clone(), token);
            self.pipeline = Some(handle);
            self.state.write().pipeline = PipelineStatus::Connecting;
            let tx = self.pipeline_events.0.clone();
            tokio::spawn(async move {
                let mut stream = stream;
                while let Some(event) = stream.next().await {
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });
        }

        if let Err(e) = self.refresh_friends().await {
            if matches!(e, ApiError::Unauthorized) {
                self.session_expired();
            } else {
                self.report(e);
            }
        }
    }

    /// `GET /friends` (both variants) + `GET /favorites?type=friend`, merged
    /// into `AppState.friends` — the one fetch path; pipeline events mutate
    /// the map afterwards.
    async fn refresh_friends(&mut self) -> Result<(), ApiError> {
        let online = self.client.friends(false).await?;
        let offline = self.client.friends(true).await?;
        let favorites: std::collections::HashSet<String> = self
            .client
            .favorite_friend_ids()
            .await?
            .into_iter()
            .collect();

        let mut state = self.state.write();
        state.friends.clear();
        for user in &offline {
            if user.id.is_empty() {
                continue;
            }
            let mut friend = Friend::from_limited(user, false);
            friend.is_favorite = favorites.contains(&friend.id);
            state.friends.insert(friend.id.clone(), friend);
        }
        for user in &online {
            if user.id.is_empty() {
                continue;
            }
            let mut friend = Friend::from_limited(user, true);
            friend.is_favorite = favorites.contains(&friend.id);
            state.friends.insert(friend.id.clone(), friend);
        }
        drop(state);
        let _ = self.events.send(AppEvent::FriendsRefreshed);
        self.ensure_world_names();
        Ok(())
    }

    /// Kick off `GET /worlds/{id}` for every world on the friends' worlds
    /// dashboard whose name isn't cached yet. Dedup happens through
    /// [`AppState::world_names`]: `Pending`/`Known`/recently-`Unknown`
    /// entries are skipped, so pipeline events and refreshes never
    /// double-fetch (RULES.md §3). Failures are retried after
    /// [`WORLD_NAME_RETRY`].
    fn ensure_world_names(&mut self) {
        let now = Instant::now();
        let mut wanted: Vec<String> = Vec::new();
        {
            let state = self.state.read();
            for group in worlds_with_friends(&state) {
                // Remote payloads can carry arbitrary world_id strings;
                // only plausible `wrld_…` ids become path segments.
                if !valid_world_id(&group.world_id) {
                    continue;
                }
                let needs_fetch = match state.world_names.get(&group.world_id) {
                    Some(WorldName::Known(_)) | Some(WorldName::Pending) => false,
                    Some(WorldName::Unknown { retry_at }) => *retry_at <= now,
                    None => true,
                };
                if needs_fetch && !wanted.contains(&group.world_id) {
                    wanted.push(group.world_id);
                }
            }
        }
        for id in wanted {
            self.state
                .write()
                .world_names
                .insert(id.clone(), WorldName::Pending);
            let client = self.client.clone();
            let events = self.events.clone();
            let state = self.state.clone();
            tokio::spawn(async move {
                let name = match client.world(&id).await {
                    Ok(world) if !world.name.is_empty() => WorldName::Known(world.name),
                    _ => WorldName::Unknown {
                        retry_at: Instant::now() + WORLD_NAME_RETRY,
                    },
                };
                // A task finishing after logout must not resurrect session
                // data; teardown_session already cleared the cache.
                let mut state = state.write();
                if state.auth != AuthState::Authenticated {
                    return;
                }
                state.world_names.insert(id.clone(), name);
                drop(state);
                let _ = events.send(AppEvent::WorldNameResolved { world_id: id });
            });
        }
    }

    /// Await a social REST call; `Unauthorized` tears the session down and
    /// other errors are reported — the handler just gets `None` back.
    async fn attempt<T>(
        &mut self,
        fut: impl std::future::Future<Output = Result<T, ApiError>>,
    ) -> Option<T> {
        match fut.await {
            Ok(value) => Some(value),
            Err(ApiError::Unauthorized) => {
                self.session_expired();
                None
            }
            Err(e) => {
                self.report(e);
                None
            }
        }
    }

    /// Report a non-API failure (validation, missing state) like a command
    /// error without needing an [`ApiError`].
    fn report_message(&mut self, message: &str) {
        let message = message.to_string();
        self.state.write().last_error = Some(message.clone());
        let _ = self.events.send(AppEvent::Error(message));
    }

    /// `GET /auth/user/notifications` → `AppState.notifications`.
    async fn refresh_notifications(&mut self) {
        if let Some(list) = self.attempt(self.client.clone().notifications()).await {
            self.state.write().notifications = Some(list);
            let _ = self.events.send(AppEvent::NotificationsChanged);
        }
    }

    /// `PUT …/see` then mark the cached entry seen.
    async fn see_notification(&mut self, id: &str) {
        if self
            .attempt(self.client.clone().see_notification(id))
            .await
            .is_some()
        {
            if let Some(list) = self.state.write().notifications.as_mut() {
                if let Some(n) = list.iter_mut().find(|n| n.id == id) {
                    n.seen = true;
                }
            }
            let _ = self.events.send(AppEvent::NotificationsChanged);
        }
    }

    /// `PUT …/hide` then drop the cached entry.
    async fn hide_notification(&mut self, id: &str) {
        if self
            .attempt(self.client.clone().hide_notification(id))
            .await
            .is_some()
        {
            if let Some(list) = self.state.write().notifications.as_mut() {
                list.retain(|n| n.id != id);
            }
            let _ = self.events.send(AppEvent::NotificationsChanged);
        }
    }

    /// `PUT …/accept` — accept a friend request, drop the notification and
    /// refresh friends so the new friend appears without waiting for the
    /// pipeline `friend-add`.
    async fn accept_notification(&mut self, id: &str) {
        if self
            .attempt(self.client.clone().accept_friend_request(id))
            .await
            .is_none()
        {
            return;
        }
        {
            let mut state = self.state.write();
            if let Some(list) = state.notifications.as_mut() {
                list.retain(|n| n.id != id);
            }
        }
        let _ = self.events.send(AppEvent::NotificationsChanged);
        match self.refresh_friends().await {
            Ok(()) => {}
            Err(ApiError::Unauthorized) => self.session_expired(),
            Err(e) => self.report(e),
        }
    }

    /// `PUT …/clear` — wipe the whole notification list.
    async fn clear_notifications(&mut self) {
        if self
            .attempt(self.client.clone().clear_notifications())
            .await
            .is_some()
        {
            self.state.write().notifications = Some(Vec::new());
            let _ = self.events.send(AppEvent::NotificationsChanged);
        }
    }

    /// `GET /users/{id}` + `friendStatus` + `playermoderations` →
    /// `AppState.profile`.
    async fn load_profile(&mut self, user_id: &str) {
        let user = match self.attempt(self.client.clone().user(user_id)).await {
            Some(user) => user,
            None => return,
        };
        // The companion calls are best-effort — a failure leaves defaults
        // rather than blanking the profile — but a rejected session still
        // tears down like every other authenticated call.
        let friend_status = match self.client.clone().friend_status(user_id).await {
            Ok(status) => status,
            Err(ApiError::Unauthorized) => {
                self.session_expired();
                return;
            }
            Err(_) => Default::default(),
        };
        let moderations = match self
            .client
            .clone()
            .player_moderations(Some(user_id), None)
            .await
        {
            Ok(list) => list,
            Err(ApiError::Unauthorized) => {
                self.session_expired();
                return;
            }
            Err(_) => Vec::new(),
        };
        self.state.write().profile = Some(ProfileView {
            user,
            friend_status,
            moderations,
        });
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `POST /userNotes` and reflect the note into the loaded profile.
    async fn set_user_note(&mut self, user_id: &str, note: &str) {
        let saved = match self
            .attempt(self.client.clone().set_user_note(user_id, note))
            .await
        {
            Some(saved) => saved,
            None => return,
        };
        let mut state = self.state.write();
        if let Some(profile) = state.profile.as_mut() {
            if profile.user.id == user_id {
                profile.user.note = saved.note.clone();
            }
        }
        drop(state);
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `POST /user/{id}/friendRequest`; optimistically flips the profile's
    /// outgoing-request flag.
    async fn send_friend_request(&mut self, user_id: &str) {
        if self
            .attempt(self.client.clone().send_friend_request(user_id))
            .await
            .is_none()
        {
            return;
        }
        if let Some(profile) = self.state.write().profile.as_mut() {
            if profile.user.id == user_id {
                profile.friend_status.outgoing_request = true;
                profile.friend_status.incoming_request = false;
            }
        }
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `DELETE /user/{id}/friendRequest`; clears the outgoing-request flag.
    async fn cancel_friend_request(&mut self, user_id: &str) {
        if self
            .attempt(self.client.clone().cancel_friend_request(user_id))
            .await
            .is_none()
        {
            return;
        }
        if let Some(profile) = self.state.write().profile.as_mut() {
            if profile.user.id == user_id {
                profile.friend_status.outgoing_request = false;
            }
        }
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `DELETE /auth/user/friends/{id}` — requires `confirm`; removes the
    /// friend locally and clears profile relationship state.
    async fn unfriend(&mut self, user_id: &str, confirm: bool) {
        if !confirm {
            self.report_message("unfriend requires confirmation");
            return;
        }
        if self
            .attempt(self.client.clone().unfriend(user_id))
            .await
            .is_none()
        {
            return;
        }
        let mut state = self.state.write();
        state.friends.remove(user_id);
        if let Some(profile) = state.profile.as_mut() {
            if profile.user.id == user_id {
                profile.friend_status = Default::default();
                profile.user.friend_request_status.clear();
            }
        }
        drop(state);
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `POST /auth/user/playermoderations` — `Block` requires `confirm`.
    async fn moderate_user(&mut self, user_id: &str, kind: PlayerModerationKind, confirm: bool) {
        if kind == PlayerModerationKind::Block && !confirm {
            self.report_message("blocking a user requires confirmation");
            return;
        }
        let created = match self
            .attempt(self.client.clone().moderate_user(user_id, kind))
            .await
        {
            Some(created) => created,
            None => return,
        };
        if let Some(profile) = self.state.write().profile.as_mut() {
            if profile.user.id == user_id {
                // Dedupe on the moderation id when present, else on
                // target+kind so an id-less response can't stack copies.
                let duplicate = profile.moderations.iter().any(|m| {
                    (!created.id.is_empty() && m.id == created.id)
                        || (m.target_user_id == user_id && m.kind() == Some(kind))
                });
                if !duplicate {
                    profile.moderations.push(created);
                }
            }
        }
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// `PUT /auth/user/unplayermoderate` — drops matching moderations from
    /// the loaded profile.
    async fn unmoderate_user(&mut self, user_id: &str, kind: PlayerModerationKind) {
        if self
            .attempt(self.client.clone().unmoderate_user(user_id, kind))
            .await
            .is_none()
        {
            return;
        }
        if let Some(profile) = self.state.write().profile.as_mut() {
            profile
                .moderations
                .retain(|m| !(m.target_user_id == user_id && m.kind() == Some(kind)));
        }
        let _ = self.events.send(AppEvent::ProfileChanged);
    }

    /// Resolve the invite target instance: explicit override wins, else the
    /// session's own `presence.instance`.
    fn resolve_invite_instance(&self, override_id: Option<String>) -> Option<String> {
        override_id
            .filter(|id| !id.is_empty())
            .or_else(|| social::own_instance_id(&self.state.read()))
    }

    /// `POST /invite/{userId}`.
    async fn invite_user(
        &mut self,
        user_id: &str,
        instance_id: Option<String>,
        message_slot: Option<u8>,
    ) {
        let Some(instance) = self.resolve_invite_instance(instance_id) else {
            self.report_message("no instance to invite to — join an instance first");
            return;
        };
        if self
            .attempt(
                self.client
                    .clone()
                    .invite_user(user_id, &instance, message_slot),
            )
            .await
            .is_some()
        {
            let _ = self.events.send(AppEvent::InviteSent);
        }
    }

    /// `POST /requestInvite/{userId}`.
    async fn request_invite(&mut self, user_id: &str, request_slot: Option<u8>) {
        if self
            .attempt(self.client.clone().request_invite(user_id, request_slot))
            .await
            .is_some()
        {
            let _ = self.events.send(AppEvent::InviteSent);
        }
    }

    /// `POST /invite/{notificationId}/response`.
    async fn respond_to_invite(&mut self, notification_id: &str, response_slot: u8) {
        if self
            .attempt(
                self.client
                    .clone()
                    .respond_invite(notification_id, response_slot),
            )
            .await
            .is_some()
        {
            let _ = self.events.send(AppEvent::InviteSent);
        }
    }

    /// Start a throttled multi-invite batch. Targets are validated and
    /// deduplicated up front; the spawned task walks them sequentially,
    /// waiting [`MULTI_INVITE_GAP`] between invites and honoring the
    /// cancellation flag (RULES.md §3).
    async fn start_multi_invite(&mut self, user_ids: Vec<String>, instance_id: Option<String>) {
        let targets = match social::validate_multi_invite_targets(&user_ids) {
            Ok(targets) => targets,
            Err(message) => {
                self.report_message(&message);
                return;
            }
        };
        if self
            .state
            .read()
            .multi_invite
            .as_ref()
            .is_some_and(|p| p.running)
        {
            self.report_message("a multi-invite is already running");
            return;
        }
        let Some(instance) = self.resolve_invite_instance(instance_id) else {
            self.report_message("no instance to invite to — join an instance first");
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.multi_invite_cancel = Some(cancel.clone());
        let total = targets.len();
        {
            let mut state = self.state.write();
            state.multi_invite = Some(MultiInviteProgress {
                total,
                running: true,
                ..Default::default()
            });
        }
        let client = self.client.clone();
        let state = self.state.clone();
        let events = self.events.clone();
        let session_expired_tx = self.session_expired_tx.clone();
        tokio::spawn(async move {
            let mut progress = MultiInviteProgress {
                total,
                running: true,
                ..Default::default()
            };
            let publish = |progress: &MultiInviteProgress| {
                // Teardown (logout/401) clears the slot; keep emitting the
                // event so waiters still see the batch end, but never
                // resurrect session state afterwards.
                {
                    let mut guard = state.write();
                    if guard.multi_invite.is_some() {
                        guard.multi_invite = Some(progress.clone());
                    }
                }
                let _ = events.send(AppEvent::MultiInviteProgress);
            };
            for target in targets {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                progress.current = Some(target.clone());
                publish(&progress);
                match client.invite_user(&target, &instance, None).await {
                    Ok(_) => progress.succeeded += 1,
                    Err(ApiError::Unauthorized) => {
                        // The session is dead: count the attempt, stop the
                        // batch and let the command loop tear the session
                        // down — remaining targets are skipped.
                        progress.failed += 1;
                        progress.done += 1;
                        progress.current = None;
                        let _ = session_expired_tx.send(());
                        break;
                    }
                    Err(e) => {
                        progress.failed += 1;
                        state.write().last_error = Some(format!("invite {target}: {e}"));
                    }
                }
                progress.done += 1;
                progress.current = None;
                publish(&progress);
                // Throttle: wait out the gap in small slices so cancel
                // lands promptly instead of after a full 1.5 s.
                let deadline = Instant::now() + MULTI_INVITE_GAP;
                while Instant::now() < deadline {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            progress.running = false;
            progress.current = None;
            publish(&progress);
        });
    }

    /// Flag the running multi-invite batch to stop.
    fn cancel_multi_invite(&mut self) {
        if let Some(cancel) = &self.multi_invite_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// `GET /instances/{worldId}:{instanceId}` → `AppState.instance`.
    async fn load_instance(&mut self, world_id: &str, instance_id: &str) {
        if let Some(instance) = self
            .attempt(self.client.clone().instance(world_id, instance_id))
            .await
        {
            self.state.write().instance = Some(instance);
            let _ = self.events.send(AppEvent::InstanceLoaded);
        }
    }

    /// Generation counter for a debounced search kind (shared with the
    /// detached task so a newer command can cancel stale results).
    fn search_generation(&mut self, kind: SearchKind) -> Arc<AtomicU64> {
        self.search_generations
            .entry(kind)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone()
    }

    /// Queue a debounced browse fetch: mark the state page loading, bump
    /// the generation, and spawn a task that sleeps [`SEARCH_DEBOUNCE`] then
    /// performs the request only if it is still the newest command.
    ///
    /// `query` is the free-text filter for the worlds/groups feeds; the
    /// avatars feed ignores it (the documented endpoint has no `search`
    /// parameter) and keys its page on the `own` scope instead.
    fn queue_search(&mut self, kind: SearchKind, query: String, offset: usize, own: bool) {
        let query = browse::normalize_query(&query);
        let offset = offset.min(1_000_000);
        // The page key identifies *what* the page holds: the normalized
        // query for text feeds, the own/featured scope for avatars — so an
        // identical in-flight scope dedups while a scope switch does not.
        let key = match kind {
            SearchKind::Avatars => {
                if own {
                    "own".to_string()
                } else {
                    "featured".to_string()
                }
            }
            _ => query.clone(),
        };
        // Dedup: an identical fetch already loading is not re-queued.
        let already_loading = {
            let state = self.state.read();
            match kind {
                SearchKind::Worlds => page_loading(state.world_search.as_ref(), &key, offset),
                SearchKind::ActiveWorlds => {
                    page_loading(state.active_worlds.as_ref(), &key, offset)
                }
                SearchKind::RecentWorlds => {
                    page_loading(state.recent_worlds.as_ref(), &key, offset)
                }
                SearchKind::Groups => page_loading(state.group_search.as_ref(), &key, offset),
                SearchKind::Avatars => page_loading(state.avatar_search.as_ref(), &key, offset),
            }
        };
        if already_loading {
            return;
        }
        let generation = self.search_generation(kind);
        let expected = generation.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut state = self.state.write();
            match kind {
                SearchKind::Worlds => {
                    state.world_search = Some(browse::continuation_loading(
                        state.world_search.as_ref(),
                        &key,
                        offset,
                    ));
                }
                SearchKind::ActiveWorlds => {
                    state.active_worlds = Some(browse::continuation_loading(
                        state.active_worlds.as_ref(),
                        &key,
                        offset,
                    ));
                }
                SearchKind::RecentWorlds => {
                    state.recent_worlds = Some(browse::continuation_loading(
                        state.recent_worlds.as_ref(),
                        &key,
                        offset,
                    ));
                }
                SearchKind::Groups => {
                    state.group_search = Some(browse::continuation_loading(
                        state.group_search.as_ref(),
                        &key,
                        offset,
                    ));
                }
                SearchKind::Avatars => {
                    state.avatar_search = Some(browse::continuation_loading(
                        state.avatar_search.as_ref(),
                        &key,
                        offset,
                    ));
                }
            }
        }
        let client = self.client.clone();
        let state = self.state.clone();
        let events = self.events.clone();
        let session_expired_tx = self.session_expired_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            // A newer command superseded this one during the debounce
            // window — drop it before it ever hits the wire.
            if generation.load(Ordering::Relaxed) != expected {
                return;
            }
            if state.read().auth != AuthState::Authenticated {
                // The command was queued while logged out (or the session
                // ended without teardown clearing this slot): don't leave
                // the loading page installed forever.
                {
                    let mut s = state.write();
                    match kind {
                        SearchKind::Worlds => clear_page_loading(&mut s.world_search),
                        SearchKind::ActiveWorlds => clear_page_loading(&mut s.active_worlds),
                        SearchKind::RecentWorlds => clear_page_loading(&mut s.recent_worlds),
                        SearchKind::Groups => clear_page_loading(&mut s.group_search),
                        SearchKind::Avatars => clear_page_loading(&mut s.avatar_search),
                    }
                }
                let _ = events.send(AppEvent::BrowseChanged);
                return;
            }
            let n = BROWSE_PAGE_SIZE as u32;
            let off = offset as u64;
            match kind {
                SearchKind::Worlds | SearchKind::ActiveWorlds | SearchKind::RecentWorlds => {
                    let filters = WorldSearchQuery {
                        search: browse::non_empty_query(&query),
                        n: Some(n),
                        offset: Some(off),
                        ..Default::default()
                    };
                    let result = match kind {
                        SearchKind::Worlds => client.search_worlds(&filters).await,
                        SearchKind::ActiveWorlds => client.active_worlds(&filters).await,
                        _ => client.recent_worlds(&filters).await,
                    };
                    finish_search(
                        &state,
                        &events,
                        &session_expired_tx,
                        &generation,
                        expected,
                        key.clone(),
                        offset,
                        None,
                        result,
                        |s, page| match kind {
                            SearchKind::Worlds => browse::merge_page(&mut s.world_search, page),
                            SearchKind::ActiveWorlds => {
                                browse::merge_page(&mut s.active_worlds, page)
                            }
                            _ => browse::merge_page(&mut s.recent_worlds, page),
                        },
                        |s| match kind {
                            SearchKind::Worlds => clear_page_loading(&mut s.world_search),
                            SearchKind::ActiveWorlds => clear_page_loading(&mut s.active_worlds),
                            _ => clear_page_loading(&mut s.recent_worlds),
                        },
                    );
                }
                SearchKind::Groups => {
                    let result = client
                        .search_groups(
                            browse::non_empty_query(&query).as_deref(),
                            Some(n),
                            Some(off),
                        )
                        .await;
                    finish_search(
                        &state,
                        &events,
                        &session_expired_tx,
                        &generation,
                        expected,
                        key.clone(),
                        offset,
                        None,
                        result,
                        |s, page| browse::merge_page(&mut s.group_search, page),
                        |s| clear_page_loading(&mut s.group_search),
                    );
                }
                SearchKind::Avatars => {
                    let result = client
                        .search_avatars(&AvatarSearchQuery {
                            user: own.then(|| "me".to_string()),
                            featured: (!own).then_some(true),
                            n: Some(n),
                            offset: Some(off),
                            ..Default::default()
                        })
                        .await;
                    finish_search(
                        &state,
                        &events,
                        &session_expired_tx,
                        &generation,
                        expected,
                        key.clone(),
                        offset,
                        None,
                        result,
                        |s, page| browse::merge_page(&mut s.avatar_search, page),
                        |s| clear_page_loading(&mut s.avatar_search),
                    );
                }
            }
        });
    }

    /// `GET /worlds/{id}` → `world_details` cache (dedup + failure cache,
    /// same pattern as `world_names`).
    async fn load_world(&mut self, world_id: &str) {
        if !valid_world_id(world_id) {
            self.report_message("invalid world id");
            return;
        }
        {
            let state = self.state.read();
            if !detail_needed(&state.world_details, world_id) {
                return;
            }
        }
        self.state
            .write()
            .world_details
            .insert(world_id.to_string(), DetailState::Pending);
        match self.client.clone().world(world_id).await {
            Ok(world) => {
                let mut state = self.state.write();
                if state.auth == AuthState::Authenticated {
                    state
                        .world_details
                        .insert(world_id.to_string(), DetailState::Known(world));
                }
                drop(state);
                let _ = self.events.send(AppEvent::BrowseChanged);
            }
            Err(ApiError::Unauthorized) => self.session_expired(),
            Err(e) => {
                let message = e.to_string();
                let mut state = self.state.write();
                if state.auth == AuthState::Authenticated {
                    state.world_details.insert(
                        world_id.to_string(),
                        DetailState::Unknown {
                            retry_at: Instant::now() + DETAIL_RETRY,
                        },
                    );
                    state.last_error = Some(message.clone());
                }
                drop(state);
                let _ = self.events.send(AppEvent::Error(message));
            }
        }
    }

    /// `GET /groups/{id}` → `group_details` cache.
    async fn load_group(&mut self, group_id: &str, include_roles: bool) {
        if !vrc_api::valid_group_id(group_id) {
            self.report_message("invalid group id");
            return;
        }
        {
            let state = self.state.read();
            if !detail_needed(&state.group_details, group_id) {
                return;
            }
        }
        self.state
            .write()
            .group_details
            .insert(group_id.to_string(), DetailState::Pending);
        match self.client.clone().group(group_id, include_roles).await {
            Ok(group) => {
                {
                    let mut state = self.state.write();
                    if state.auth == AuthState::Authenticated {
                        state
                            .group_details
                            .insert(group_id.to_string(), DetailState::Known(group));
                    }
                }
                let _ = self.events.send(AppEvent::BrowseChanged);
                // Second call per the spec: a 404 simply means the group
                // has no announcement.
                match self.client.clone().group_announcement(group_id).await {
                    Ok(announcement) => {
                        self.state
                            .write()
                            .group_announcements
                            .insert(group_id.to_string(), announcement);
                        let _ = self.events.send(AppEvent::BrowseChanged);
                    }
                    Err(ApiError::Http { status: 404, .. }) => {}
                    Err(ApiError::Unauthorized) => self.session_expired(),
                    Err(_) => {}
                }
            }
            Err(ApiError::Unauthorized) => self.session_expired(),
            Err(e) => {
                let message = e.to_string();
                let mut state = self.state.write();
                if state.auth == AuthState::Authenticated {
                    state.group_details.insert(
                        group_id.to_string(),
                        DetailState::Unknown {
                            retry_at: Instant::now() + DETAIL_RETRY,
                        },
                    );
                    state.last_error = Some(message.clone());
                }
                drop(state);
                let _ = self.events.send(AppEvent::Error(message));
            }
        }
    }

    /// `POST /groups/{id}/join` then reload the group detail.
    async fn join_group(&mut self, group_id: &str) {
        if self
            .attempt(self.client.clone().join_group(group_id))
            .await
            .is_some()
        {
            // Membership changed — force a fresh detail fetch past the cache.
            self.state.write().group_details.remove(group_id);
            self.load_group(group_id, true).await;
        }
    }

    /// `POST /groups/{id}/leave` then reload the group detail.
    async fn leave_group(&mut self, group_id: &str) {
        if self
            .attempt(self.client.clone().leave_group(group_id))
            .await
            .is_some()
        {
            self.state.write().group_details.remove(group_id);
            self.load_group(group_id, true).await;
        }
    }

    /// `GET /groups/{id}/members` → `group_members` page.
    async fn load_group_members(&mut self, group_id: &str, offset: usize) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        if let Some(items) = self
            .attempt(self.client.clone().group_members(
                group_id,
                Some(n),
                Some(offset as u64),
                None,
                None,
            ))
            .await
        {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page_map(
                &mut state.group_members,
                group_id,
                BrowsePage {
                    key: group_id.to_string(),
                    offset,
                    items,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /users/{me}/groups` → `own_groups`.
    async fn load_own_groups(&mut self) {
        let user_id = self
            .state
            .read()
            .current_user
            .as_ref()
            .map(|u| u.id.clone());
        let Some(user_id) = user_id else {
            self.report_message("not logged in");
            return;
        };
        if let Some(groups) = self
            .attempt(self.client.clone().user_groups(&user_id))
            .await
        {
            self.state.write().own_groups = Some(groups);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /groups/{id}/posts` → `group_posts` page.
    async fn load_group_posts(&mut self, group_id: &str, offset: usize, public_only: bool) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        if let Some(page) = self
            .attempt(self.client.clone().group_posts(
                group_id,
                Some(n),
                Some(offset as u64),
                public_only,
            ))
            .await
        {
            let posts = page.posts;
            let exhausted = posts.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page_map(
                &mut state.group_posts,
                group_id,
                BrowsePage {
                    key: group_id.to_string(),
                    offset,
                    items: posts,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /groups/{id}/galleries/{galleryId}` → `group_gallery_images`
    /// page keyed `group:gallery`.
    async fn load_group_gallery_images(
        &mut self,
        group_id: &str,
        gallery_id: &str,
        offset: usize,
        approved: Option<bool>,
    ) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        let key = browse::gallery_key(group_id, gallery_id);
        if let Some(items) = self
            .attempt(self.client.clone().group_gallery_images(
                group_id,
                gallery_id,
                Some(n),
                Some(offset as u64),
                approved,
            ))
            .await
        {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page_map(
                &mut state.group_gallery_images,
                &key,
                BrowsePage {
                    key: key.clone(),
                    offset,
                    items,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /groups/{id}/instances` → `group_instances` page.
    async fn load_group_instances(&mut self, group_id: &str) {
        if let Some(items) = self
            .attempt(self.client.clone().group_instances(group_id))
            .await
        {
            let mut state = self.state.write();
            // Unpaged endpoint: the returned list is complete.
            browse::merge_page_map(
                &mut state.group_instances,
                group_id,
                BrowsePage {
                    key: group_id.to_string(),
                    offset: 0,
                    items,
                    total: None,
                    loading: false,
                    exhausted: true,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /avatars/{id}` → `avatar_details` cache.
    async fn load_avatar(&mut self, avatar_id: &str) {
        if !vrc_api::valid_avatar_id(avatar_id) {
            self.report_message("invalid avatar id");
            return;
        }
        {
            let state = self.state.read();
            if !detail_needed(&state.avatar_details, avatar_id) {
                return;
            }
        }
        self.state
            .write()
            .avatar_details
            .insert(avatar_id.to_string(), DetailState::Pending);
        match self.client.clone().avatar(avatar_id).await {
            Ok(avatar) => {
                let mut state = self.state.write();
                if state.auth == AuthState::Authenticated {
                    state
                        .avatar_details
                        .insert(avatar_id.to_string(), DetailState::Known(avatar));
                }
                drop(state);
                let _ = self.events.send(AppEvent::BrowseChanged);
            }
            Err(ApiError::Unauthorized) => self.session_expired(),
            Err(e) => {
                let message = e.to_string();
                let mut state = self.state.write();
                if state.auth == AuthState::Authenticated {
                    state.avatar_details.insert(
                        avatar_id.to_string(),
                        DetailState::Unknown {
                            retry_at: Instant::now() + DETAIL_RETRY,
                        },
                    );
                    state.last_error = Some(message.clone());
                }
                drop(state);
                let _ = self.events.send(AppEvent::Error(message));
            }
        }
    }

    /// `GET /users/{me}/avatar` → `own_avatar`.
    async fn load_own_avatar(&mut self) {
        let user_id = self
            .state
            .read()
            .current_user
            .as_ref()
            .map(|u| u.id.clone());
        let Some(user_id) = user_id else {
            self.report_message("not logged in");
            return;
        };
        if let Some(avatar) = self.attempt(self.client.clone().own_avatar(&user_id)).await {
            self.state.write().own_avatar = Some(avatar);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `PUT /avatars/{id}/select` — guarded by `confirm` (the UI shows a
    /// confirmation dialog first). Updates `current_user` from the
    /// response and invalidates `own_avatar` so it reloads fresh.
    async fn select_avatar(&mut self, avatar_id: &str, confirm: bool) {
        if !confirm {
            self.report_message("selecting an avatar requires confirmation");
            return;
        }
        if let Some(user) = self
            .attempt(self.client.clone().select_avatar(avatar_id))
            .await
        {
            let mut state = self.state.write();
            state.current_user = Some(user);
            state.own_avatar = None;
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// Every `/favorites?type=…` record for `kind`, all pages (same
    /// short-page/cap pattern as the friends fetch — `favorite_records`
    /// must be complete or `RemoveFavorite` cannot resolve `fvrt_…` ids).
    async fn all_favorite_records(&mut self, kind: FavoriteKind) -> Option<Vec<vrc_api::Favorite>> {
        const PAGE_SIZE: u64 = 100;
        const MAX_PAGES: u64 = 50;
        let mut records = Vec::new();
        let mut offset = 0u64;
        for _ in 0..MAX_PAGES {
            let page = self
                .attempt(
                    self.client
                        .clone()
                        .favorites(kind, Some(PAGE_SIZE as u32), Some(offset)),
                )
                .await?;
            let len = page.len() as u64;
            records.extend(page);
            if len < PAGE_SIZE {
                return Some(records);
            }
            offset += len;
        }
        tracing::warn!("favorite records pagination hit the page cap; returning partial list");
        Some(records)
    }

    /// Load world/avatar favorites plus every `/favorites` record (all
    /// kinds, all pages) used to add/remove them. All calls must succeed
    /// for state to update.
    async fn refresh_favorites(&mut self) {
        let n = BROWSE_PAGE_SIZE as u32;
        let worlds = self
            .attempt(self.client.clone().favorite_worlds(Some(n), Some(0), None))
            .await;
        let avatars = self
            .attempt(self.client.clone().favorite_avatars(Some(n), Some(0), None))
            .await;
        let world_recs = self.all_favorite_records(FavoriteKind::World).await;
        let avatar_recs = self.all_favorite_records(FavoriteKind::Avatar).await;
        let friend_recs = self.all_favorite_records(FavoriteKind::Friend).await;
        let (Some(worlds), Some(avatars), Some(world_recs), Some(avatar_recs), Some(friend_recs)) =
            (worlds, avatars, world_recs, avatar_recs, friend_recs)
        else {
            return;
        };
        let mut records = world_recs;
        records.extend(avatar_recs);
        records.extend(friend_recs);
        let worlds_exhausted = worlds.len() < BROWSE_PAGE_SIZE;
        let avatars_exhausted = avatars.len() < BROWSE_PAGE_SIZE;
        let mut state = self.state.write();
        state.favorite_worlds = Some(BrowsePage {
            key: "worlds".to_string(),
            offset: 0,
            items: worlds,
            total: None,
            loading: false,
            exhausted: worlds_exhausted,
        });
        state.favorite_avatars = Some(BrowsePage {
            key: "avatars".to_string(),
            offset: 0,
            items: avatars,
            total: None,
            loading: false,
            exhausted: avatars_exhausted,
        });
        state.favorite_records = records;
        state.favorite_records_loaded = true;
        drop(state);
        let _ = self.events.send(AppEvent::BrowseChanged);
    }

    /// `GET /worlds/favorites` page → `favorite_worlds`.
    async fn load_favorite_worlds(&mut self, offset: usize) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        if let Some(items) = self
            .attempt(
                self.client
                    .clone()
                    .favorite_worlds(Some(n), Some(offset as u64), None),
            )
            .await
        {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page(
                &mut state.favorite_worlds,
                BrowsePage {
                    key: "worlds".to_string(),
                    offset,
                    items,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /avatars/favorites` page → `favorite_avatars`.
    async fn load_favorite_avatars(&mut self, offset: usize) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        if let Some(items) = self
            .attempt(
                self.client
                    .clone()
                    .favorite_avatars(Some(n), Some(offset as u64), None),
            )
            .await
        {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page(
                &mut state.favorite_avatars,
                BrowsePage {
                    key: "avatars".to_string(),
                    offset,
                    items,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `GET /favorite/groups` page → `favorite_groups`.
    async fn load_favorite_groups(&mut self, offset: usize) {
        let offset = offset.min(1_000_000);
        let n = BROWSE_PAGE_SIZE as u32;
        if let Some(items) = self
            .attempt(
                self.client
                    .clone()
                    .favorite_groups(Some(n), Some(offset as u64), None),
            )
            .await
        {
            let exhausted = items.len() < BROWSE_PAGE_SIZE;
            let mut state = self.state.write();
            browse::merge_page(
                &mut state.favorite_groups,
                BrowsePage {
                    key: "groups".to_string(),
                    offset,
                    items,
                    total: None,
                    loading: false,
                    exhausted,
                },
            );
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `POST /favorites` — update `favorite_records` so `is_favorite`
    /// flips immediately; typed favorite lists refresh on next load.
    async fn add_favorite(&mut self, kind: FavoriteKind, favorite_id: &str, tags: Vec<String>) {
        if let Some(fav) = self
            .attempt(self.client.clone().add_favorite(kind, favorite_id, &tags))
            .await
        {
            let mut state = self.state.write();
            state
                .favorite_records
                .retain(|r| !(r.kind == kind.as_str() && r.favorite_id == favorite_id));
            state.favorite_records.push(fav);
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `DELETE /favorites/{fvrt_…}` — resolves the record id from
    /// `favorite_records` (loaded by `RefreshFavorites`).
    async fn remove_favorite(&mut self, kind: FavoriteKind, object_id: &str) {
        let record_id = {
            let state = self.state.read();
            browse::favorite_record(&state, kind, object_id).map(|f| f.id.clone())
        };
        let Some(record_id) = record_id else {
            self.report_message("favorite record not loaded — refresh favorites first");
            return;
        };
        if self
            .attempt(self.client.clone().remove_favorite(&record_id))
            .await
            .is_some()
        {
            let mut state = self.state.write();
            state.favorite_records.retain(|r| r.id != record_id);
            drop(state);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `POST /instances` — create a world/group instance into
    /// `AppState.instance`.
    async fn create_instance(&mut self, request: CreateInstanceRequest) {
        if let Some(instance) = self
            .attempt(self.client.clone().create_instance(&request))
            .await
        {
            self.state.write().instance = Some(instance);
            let _ = self.events.send(AppEvent::BrowseChanged);
        }
    }

    /// `POST /invite/myself/to/{world}:{instance}` — self-invite.
    async fn invite_myself_to_instance(&mut self, world_id: &str, instance_id: &str) {
        if self
            .attempt(
                self.client
                    .clone()
                    .invite_myself_to_instance(world_id, instance_id),
            )
            .await
            .is_some()
        {
            let _ = self.events.send(AppEvent::InviteSent);
        }
    }

    /// Merge one pipeline event into `AppState` (never re-fetches).
    fn apply_pipeline_event(&mut self, event: &PipelineEvent) {
        let mut state = self.state.write();
        match event {
            PipelineEvent::Connected => state.pipeline = PipelineStatus::Connected,
            PipelineEvent::Disconnected { .. } => {
                state.pipeline = PipelineStatus::Disconnected;
            }
            PipelineEvent::FriendOnline { user_id, user }
            | PipelineEvent::FriendActive { user_id, user } => {
                let key = if user_id.is_empty() {
                    user.id.clone()
                } else {
                    user_id.clone()
                };
                if key.is_empty() {
                    return;
                }
                let friend = state.friends.entry(key).or_default();
                // A degraded/missing user payload (`User::default`) must not
                // blank a known friend's fields.
                if !user.id.is_empty() {
                    friend.apply_user(user);
                }
                // The event kind is authoritative for presence: -online is
                // in-game-eligible, -active is on web/app. An empty payload
                // location clears the friend's stale instance.
                friend.state = if matches!(event, PipelineEvent::FriendOnline { .. }) {
                    "online".to_string()
                } else {
                    "active".to_string()
                };
                if user.location.is_empty() {
                    friend.location.clear();
                    friend.world_id.clear();
                }
            }
            PipelineEvent::FriendAdd { user_id, user } => {
                let key = if user_id.is_empty() {
                    user.id.clone()
                } else {
                    user_id.clone()
                };
                if key.is_empty() {
                    return;
                }
                let friend = state.friends.entry(key).or_default();
                // friend-add carries the friend's current `state`; an absent
                // or offline state must not fabricate presence, and a
                // degraded payload must not blank a known friend.
                if !user.id.is_empty() {
                    friend.apply_user(user);
                }
            }
            PipelineEvent::FriendOffline { user_id, .. } => {
                if let Some(friend) = state.friends.get_mut(user_id) {
                    friend.mark_offline();
                }
            }
            PipelineEvent::FriendLocation {
                user_id,
                location,
                world_id,
                user,
                ..
            } => {
                let key = if user_id.is_empty() {
                    user.as_ref().map(|u| u.id.clone()).unwrap_or_default()
                } else {
                    user_id.clone()
                };
                if key.is_empty() {
                    return;
                }
                let friend = state.friends.entry(key).or_default();
                // Apply the user payload first — it can carry a stale
                // location, so the event's own fields must overwrite it.
                if let Some(user) = user.as_ref().filter(|u| !u.id.is_empty()) {
                    friend.apply_user(user);
                }
                friend.location = location.clone();
                friend.world_id = if world_id.is_empty() {
                    crate::state::world_id_from(location)
                } else {
                    world_id.clone()
                };
                friend.state = if location.is_empty() || location == "offline" {
                    "offline".to_string()
                } else {
                    "online".to_string()
                };
            }
            PipelineEvent::FriendUpdate { user_id, user } => {
                let key = if user_id.is_empty() {
                    user.id.clone()
                } else {
                    user_id.clone()
                };
                if key.is_empty() {
                    return;
                }
                let friend = state.friends.entry(key).or_default();
                if !user.id.is_empty() {
                    friend.apply_user(user);
                }
                if friend.state == "offline" {
                    friend.mark_offline();
                }
            }
            PipelineEvent::FriendDelete { user_id } => {
                state.friends.remove(user_id);
            }
            PipelineEvent::Notification { notification } => {
                if notification.id.is_empty() {
                    return;
                }
                let list = state.notifications.get_or_insert_with(Vec::new);
                match list.iter_mut().find(|n| n.id == notification.id) {
                    Some(existing) => *existing = notification.clone(),
                    // Newest first, matching the REST list ordering.
                    None => list.insert(0, notification.clone()),
                }
            }
            PipelineEvent::SeeNotification { notification_id } => {
                if let Some(list) = state.notifications.as_mut() {
                    if let Some(n) = list.iter_mut().find(|n| &n.id == notification_id) {
                        n.seen = true;
                    }
                }
            }
            PipelineEvent::HideNotification { notification_id } => {
                if let Some(list) = state.notifications.as_mut() {
                    list.retain(|n| n.id != *notification_id);
                }
            }
            PipelineEvent::ClearNotifications => {
                state.notifications = Some(Vec::new());
            }
            PipelineEvent::NotificationV2 { notification } => {
                if notification.id.is_empty() {
                    return;
                }
                match state
                    .notifications_v2
                    .iter_mut()
                    .find(|n| n.id == notification.id)
                {
                    Some(existing) => *existing = notification.clone(),
                    None => state.notifications_v2.insert(0, notification.clone()),
                }
            }
            PipelineEvent::NotificationV2Update { id, updates } => {
                if let Some(existing) = state.notifications_v2.iter_mut().find(|n| &n.id == id) {
                    social::merge_notification_v2(existing, updates);
                }
            }
            PipelineEvent::NotificationV2Delete { ids } => {
                state.notifications_v2.retain(|n| !ids.contains(&n.id));
            }
            // user updates, responses and unknown frames don't alter state;
            // subscribers still see them via AppEvent::Pipeline.
            PipelineEvent::ResponseNotification { .. }
            | PipelineEvent::UserUpdate { .. }
            | PipelineEvent::Unknown { .. } => {}
        }
    }

    /// The API rejected the session (401): tear it down and tell
    /// subscribers we're logged out.
    fn session_expired(&mut self) {
        self.teardown_session();
        self.set_auth(AuthState::LoggedOut);
    }

    fn set_auth(&mut self, auth: AuthState) {
        self.state.write().auth = auth.clone();
        let _ = self.events.send(AppEvent::AuthChanged(auth));
    }

    /// Clear session state on logout/401.
    fn teardown_session(&mut self) {
        if let Some(handle) = self.pipeline.take() {
            handle.shutdown();
        }
        // Drop this generation's event channel so queued pipeline events —
        // e.g. a Connected already in flight — can't resurrect the session's
        // pipeline status or repopulate friends after teardown (same reason
        // `after_login` installs a fresh channel).
        self.pipeline_events = mpsc::unbounded_channel();
        // Stop any running multi-invite batch; its task observes the flag
        // and finishes without further requests.
        if let Some(cancel) = self.multi_invite_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        let mut state = self.state.write();
        state.current_user = None;
        state.friends.clear();
        state.pipeline = PipelineStatus::Disconnected;
        state.world_names.clear();
        state.notifications = None;
        state.notifications_v2.clear();
        state.profile = None;
        state.instance = None;
        state.multi_invite = None;
        state.world_search = None;
        state.active_worlds = None;
        state.recent_worlds = None;
        state.group_search = None;
        state.own_groups = None;
        state.avatar_search = None;
        state.favorite_worlds = None;
        state.favorite_avatars = None;
        state.favorite_groups = None;
        state.favorite_records.clear();
        state.favorite_records_loaded = false;
        state.world_details.clear();
        state.group_details.clear();
        state.avatar_details.clear();
        state.own_avatar = None;
        state.group_members.clear();
        state.group_posts.clear();
        state.group_gallery_images.clear();
        state.group_instances.clear();
        // Calendar data is session-scoped like the other API views;
        // `tools` (launcher/OSC config) is local and survives logout.
        state.calendar = Default::default();
    }

    // ------------------------------------------------------------------
    // M7 launcher & tools (local, spawn-only — no login required)
    // ------------------------------------------------------------------

    /// Spawn VRChat with the persisted launch prefs; companions follow
    /// when auto-start is enabled. `spawn` only — never a shell, never
    /// process inspection (RULES.md §5). The Steam scan and the spawn
    /// itself run off the async executor (`spawn_blocking`).
    async fn launch_vrchat(&mut self) {
        let (override_path, prefs, companions, auto_start) = {
            let tools = &self.state.read().tools;
            (
                tools.vrchat_path.clone(),
                tools.launch.clone(),
                tools.companions.clone(),
                tools.auto_start_companion,
            )
        };
        let result = tokio::task::spawn_blocking(move || {
            tools::resolve_install_dir(override_path.as_deref()).map(|dir| {
                tools::launch_vrchat(&dir, &prefs, &companions, auto_start)
                    .map(|summary| (summary, dir.to_string_lossy().into_owned()))
            })
        })
        .await;
        match result {
            Ok(Some(Ok((summary, found)))) => {
                {
                    let mut state = self.state.write();
                    state.tools.last_launch = Some(summary);
                    state.tools.vrchat_found = Some(found);
                }
                let _ = self.events.send(AppEvent::ProcessLaunched);
                let _ = self.events.send(AppEvent::ToolsChanged);
            }
            Ok(Some(Err(message))) => self.report_message(&message),
            Ok(None) => {
                // Refresh the cached scan so the UI shows the real status.
                let found = tokio::task::spawn_blocking(vrc_launcher::find_vrchat)
                    .await
                    .ok()
                    .flatten()
                    .map(|dir| dir.to_string_lossy().into_owned());
                self.state.write().tools.vrchat_found = found;
                self.report_message(
                    "VRChat installation not found — set a path override in the Launcher tab",
                );
            }
            Err(e) => self.report_message(&format!("launch task failed: {e}")),
        }
    }

    /// Spawn one configured companion by index (off the executor —
    /// `spawn` is a blocking syscall).
    async fn launch_companion(&mut self, index: usize) {
        let companions = self.state.read().tools.companions.clone();
        let result =
            tokio::task::spawn_blocking(move || tools::launch_companion_at(&companions, index))
                .await;
        match result {
            Ok(Ok(name)) => {
                self.state.write().tools.last_launch = Some(name);
                let _ = self.events.send(AppEvent::ProcessLaunched);
            }
            Ok(Err(message)) => self.report_message(&message),
            Err(e) => self.report_message(&format!("launch task failed: {e}")),
        }
    }

    /// Spawn an ad-hoc path + args (CLI). Off the async executor because
    /// `spawn` is a blocking syscall.
    async fn launch_path(&mut self, path: String, args: Vec<String>) {
        match tokio::task::spawn_blocking(move || tools::launch_path(&path, &args)).await {
            Ok(Ok(())) => {
                self.state.write().tools.last_launch = Some("process".to_string());
                let _ = self.events.send(AppEvent::ProcessLaunched);
            }
            Ok(Err(message)) => self.report_message(&message),
            Err(_) => self.report_message("launch task failed"),
        }
    }

    /// Companion list mutations all funnel through here: persist the JSON
    /// setting, flush the store barrier, then update state + notify.
    async fn persist_companions(&self, companions: Vec<Companion>) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreOp::SetSetting {
                key: tools::SETTING_COMPANIONS.to_string(),
                value: tools::companions_json(&companions),
            });
        }
        self.flush_store().await;
        self.state.write().tools.companions = companions;
        let _ = self.events.send(AppEvent::ToolsChanged);
    }

    /// Persist one tools setting and notify; callers update `AppState`
    /// themselves before/after as needed.
    async fn persist_tool_setting(&self, key: &str, value: String) {
        if let Some(tx) = &self.store_tx {
            let _ = tx.send(StoreOp::SetSetting {
                key: key.to_string(),
                value,
            });
        }
        self.flush_store().await;
        let _ = self.events.send(AppEvent::ToolsChanged);
    }

    fn validate_companion(companion: &Companion) -> Result<(), String> {
        if companion.name.trim().is_empty() {
            return Err("companion name is empty".to_string());
        }
        if companion.path.trim().is_empty() {
            return Err("companion path is empty".to_string());
        }
        Ok(())
    }

    async fn add_companion(&mut self, mut companion: Companion) {
        companion.name = companion.name.trim().to_string();
        companion.path = companion.path.trim().to_string();
        if let Err(message) = Self::validate_companion(&companion) {
            self.report_message(&message);
            return;
        }
        let mut companions = self.state.read().tools.companions.clone();
        if companions.iter().any(|c| c.path == companion.path) {
            self.report_message("that companion is already configured");
            return;
        }
        companions.push(companion);
        self.persist_companions(companions).await;
    }

    async fn remove_companion(&mut self, index: usize) {
        let mut companions = self.state.read().tools.companions.clone();
        if index >= companions.len() {
            self.report_message("no companion at that index");
            return;
        }
        companions.remove(index);
        self.persist_companions(companions).await;
    }

    async fn update_companion(&mut self, index: usize, mut companion: Companion) {
        companion.name = companion.name.trim().to_string();
        companion.path = companion.path.trim().to_string();
        if let Err(message) = Self::validate_companion(&companion) {
            self.report_message(&message);
            return;
        }
        let mut companions = self.state.read().tools.companions.clone();
        if index >= companions.len() {
            self.report_message("no companion at that index");
            return;
        }
        if companions
            .iter()
            .enumerate()
            .any(|(i, c)| i != index && c.path == companion.path)
        {
            self.report_message("another companion already uses that path");
            return;
        }
        companions[index] = companion;
        self.persist_companions(companions).await;
    }

    async fn set_auto_start_companion(&mut self, enabled: bool) {
        self.persist_tool_setting(
            tools::SETTING_AUTO_START,
            if enabled { "1" } else { "0" }.to_string(),
        )
        .await;
        self.state.write().tools.auto_start_companion = enabled;
    }

    async fn set_vrchat_path(&mut self, path: Option<String>) {
        let normalized = path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        if let Some(dir) = &normalized {
            // Only accept a real install dir — anything else would make
            // `launch` fail later with a confusing error.
            if !Path::new(dir).join("VRChat.exe").is_file() {
                self.report_message("path does not contain VRChat.exe");
                return;
            }
        }
        self.persist_tool_setting(
            tools::SETTING_VRCHAT_PATH,
            normalized.clone().unwrap_or_default(),
        )
        .await;
        // Clearing the override re-runs the Steam scan — filesystem I/O,
        // keep it off the async executor.
        let scanned = if normalized.is_none() {
            tokio::task::spawn_blocking(vrc_launcher::find_vrchat)
                .await
                .ok()
                .flatten()
                .map(|d| d.to_string_lossy().into_owned())
        } else {
            None
        };
        let mut state = self.state.write();
        state.tools.vrchat_path = normalized;
        // The override may point somewhere the Steam scan never saw.
        state.tools.vrchat_found = state.tools.vrchat_path.clone().or(scanned);
    }

    async fn set_launch_prefs(&mut self, prefs: LaunchPrefs) {
        // Chatbox/OSC sends follow the configured input port.
        self.chatbox_relay
            .set_target(tools::osc_send_target(&prefs));
        self.persist_tool_setting(tools::SETTING_LAUNCH, tools::launch_prefs_json(&prefs))
            .await;
        self.state.write().tools.launch = prefs;
    }

    /// Queue a chatbox message on the throttled relay (≤1 per 1.5 s,
    /// latest wins — never an unbounded queue).
    fn send_chatbox(&mut self, text: &str, immediate: bool) {
        match tools::chatbox_message(text, immediate) {
            Ok(message) => self.chatbox_relay.send(message),
            Err(message) => self.report_message(&message),
        }
    }

    /// The typing indicator bypasses the chatbox throttle (it is a
    /// different OSC address and carries no message text).
    async fn send_chatbox_typing(&mut self, typing: bool) {
        let message = osc_chatbox::chatbox_typing(typing);
        let target = tools::osc_send_target(&self.state.read().tools.launch);
        tokio::spawn(async move {
            match OscClient::bind(target).await {
                Ok(client) => {
                    if let Err(e) = client.send(&message).await {
                        tracing::debug!("chatbox typing send failed: {e}");
                    }
                }
                Err(e) => tracing::debug!("chatbox OSC client failed: {e}"),
            }
        });
    }

    /// Send one avatar parameter immediately (not chatbox-throttled).
    /// The sent value is mirrored into `tools.osc.params` right away.
    async fn send_osc_param(&mut self, address: &str, value: ParamValue) {
        let message = match tools::param_message(address, value) {
            Ok(message) => message,
            Err(message) => {
                self.report_message(&message);
                return;
            }
        };
        self.state
            .write()
            .tools
            .osc
            .params
            .insert(address.to_string(), value);
        let target = tools::osc_send_target(&self.state.read().tools.launch);
        tokio::spawn(async move {
            match OscClient::bind(target).await {
                Ok(client) => {
                    if let Err(e) = client.send(&message).await {
                        tracing::debug!("OSC param send failed: {e}");
                    }
                }
                Err(e) => tracing::debug!("OSC client failed: {e}"),
            }
        });
    }

    /// Bind the OSC listener and spawn its pump task. Parameter messages
    /// update `tools.osc.params`; `ToolsChanged` is emitted at most every
    /// 250 ms so a 60 Hz avatar stream can't flood subscribers.
    async fn start_osc_listen(&mut self, port: u16) {
        if self.osc_listener.is_some() {
            self.report_message("OSC listener is already running");
            return;
        }
        match OscListener::bind(port).await {
            Ok((listener, mut stream)) => {
                let bound = listener.local_addr().port();
                let state = self.state.clone();
                let events = self.events.clone();
                let handle = tokio::spawn(async move {
                    use futures_util::StreamExt;
                    let mut last_emit = Instant::now() - Duration::from_secs(1);
                    while let Some(message) = stream.next().await {
                        let Some((addr, value)) = tools::param_value_of(&message) else {
                            continue;
                        };
                        let is_new = {
                            let mut guard = state.write();
                            let is_new = !guard.tools.osc.params.contains_key(&addr);
                            guard.tools.osc.params.insert(addr, value);
                            is_new
                        };
                        if is_new || last_emit.elapsed() >= Duration::from_millis(250) {
                            last_emit = Instant::now();
                            let _ = events.send(AppEvent::ToolsChanged);
                        }
                    }
                });
                self.osc_listener = Some(handle);
                {
                    let mut guard = self.state.write();
                    guard.tools.osc.listening = true;
                    guard.tools.osc.listen_port = bound;
                }
                let _ = self.events.send(AppEvent::ToolsChanged);
            }
            Err(e) => self.report_message(&format!("OSC listen failed: {e}")),
        }
    }

    fn stop_osc_listen(&mut self) {
        if let Some(handle) = self.osc_listener.take() {
            handle.abort();
        }
        {
            let mut guard = self.state.write();
            guard.tools.osc.listening = false;
            guard.tools.osc.listen_port = 0;
        }
        let _ = self.events.send(AppEvent::ToolsChanged);
    }

    // ------------------------------------------------------------------
    // M7 events calendar (documented /calendar* endpoints, needs login)
    // ------------------------------------------------------------------

    /// `GET /calendar` → `AppState.calendar.month`.
    async fn load_calendar_month(&mut self, date: Option<String>, offset: usize) {
        let date = date.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
        if let Some(page) = self
            .attempt(self.client.clone().calendar_events(
                date.as_deref(),
                Some(60),
                Some(offset as u64),
            ))
            .await
        {
            self.state.write().calendar.month = Some((date.unwrap_or_default(), page));
            let _ = self.events.send(AppEvent::CalendarChanged);
        }
    }

    /// `GET /calendar/discover` → accumulated `AppState.calendar.discover`.
    async fn load_calendar_discover(&mut self, scope: DiscoverScope, cursor: Option<String>) {
        let params = DiscoverParams {
            scope: Some(scope),
            n: Some(60),
            next_cursor: cursor.as_deref(),
            ..Default::default()
        };
        if let Some(discovery) = self
            .attempt(self.client.clone().calendar_discover(&params))
            .await
        {
            let mut state = self.state.write();
            let entry = state
                .calendar
                .discover
                .get_or_insert_with(CalendarDiscoveryUi::default);
            if cursor.is_none() || entry.scope != scope.as_str() {
                entry.scope = scope.as_str().to_string();
                entry.events = discovery.results;
            } else {
                entry.events.extend(discovery.results);
            }
            entry.next_cursor = discovery.next_cursor;
            drop(state);
            let _ = self.events.send(AppEvent::CalendarChanged);
        }
    }

    /// `GET /calendar/search` → `AppState.calendar.search`.
    async fn load_calendar_search(&mut self, query: &str, offset: usize) {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.report_message("calendar search term is empty");
            return;
        }
        if let Some(page) = self
            .attempt(self.client.clone().calendar_search(
                &query,
                None,
                Some(60),
                Some(offset as u64),
            ))
            .await
        {
            self.state.write().calendar.search = Some((query, page));
            let _ = self.events.send(AppEvent::CalendarChanged);
        }
    }

    /// `GET /calendar/{groupId}` → `AppState.calendar.group`.
    async fn load_group_calendar(&mut self, group_id: &str, date: Option<String>, offset: usize) {
        if !vrc_api::valid_group_id(group_id) {
            self.report_message("invalid group id");
            return;
        }
        let params = GroupCalendarParams {
            date: date.as_deref(),
            n: Some(60),
            offset: Some(offset as u64),
            ..Default::default()
        };
        if let Some(page) = self
            .attempt(self.client.clone().group_calendar_events(group_id, &params))
            .await
        {
            self.state.write().calendar.group = Some((group_id.to_string(), page));
            let _ = self.events.send(AppEvent::CalendarChanged);
        }
    }

    /// `GET /calendar/{groupId}/next` → `AppState.calendar.next_events`;
    /// a 404 is stored as `Some(None)` ("no upcoming event").
    async fn load_group_next_event(&mut self, group_id: &str) {
        if !vrc_api::valid_group_id(group_id) {
            self.report_message("invalid group id");
            return;
        }
        match self
            .client
            .clone()
            .group_next_calendar_event(group_id)
            .await
        {
            Ok(event) => {
                self.state
                    .write()
                    .calendar
                    .next_events
                    .insert(group_id.to_string(), Some(event));
                let _ = self.events.send(AppEvent::CalendarChanged);
            }
            Err(ApiError::Http { status: 404, .. }) => {
                self.state
                    .write()
                    .calendar
                    .next_events
                    .insert(group_id.to_string(), None);
                let _ = self.events.send(AppEvent::CalendarChanged);
            }
            Err(ApiError::Unauthorized) => self.session_expired(),
            Err(e) => self.report(e),
        }
    }

    /// `POST /calendar/{groupId}/{calendarId}/follow`, then mirror the
    /// returned `userInterest` into every loaded event list.
    async fn follow_calendar_event(
        &mut self,
        group_id: &str,
        calendar_id: &str,
        is_following: bool,
    ) {
        let Some(event) = self
            .attempt(
                self.client
                    .clone()
                    .follow_calendar_event(group_id, calendar_id, is_following),
            )
            .await
        else {
            return;
        };
        apply_followed_event(&mut self.state.write(), &event);
        let _ = self.events.send(AppEvent::CalendarChanged);
    }

    fn report(&mut self, error: ApiError) {
        let message = error.to_string();
        tracing::debug!("vrc-core command failed: {message}");
        self.state.write().last_error = Some(message.clone());
        let _ = self.events.send(AppEvent::Error(message));
    }
}
