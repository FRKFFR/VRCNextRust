//! Application core: state, commands, events, tasks.
//!
//! [`App::spawn`] starts the core; front ends drive it only through
//! [`Command`]s, [`AppEvent`]s and [`StateHandle`] snapshots (RULES.md §7).
//! After a successful login/resume the friends list and favorites are
//! fetched once via the rate-limited `vrc-api` client, then kept current by
//! `vrc-pipeline` events — never re-fetched per event (RULES.md §3).

mod app;
mod dashboard;
pub mod format;
mod local;
mod sections;
mod state;

pub use app::{
    App, AppConfig, AppEvent, AppHandle, Command, CommandSender, StateHandle, LOCAL_PAGE_SIZE,
};
pub use dashboard::{dashboard, world_title, DashboardEntry};
pub use sections::{search, sections, worlds_with_friends, FriendSections, WorldGroup};
pub use state::{
    AppState, AuthState, Friend, HistoryPage, PipelineStatus, TimelinePage, WorldName,
};

// Front ends (cli/ui-egui) only depend on vrc-core; re-export the pieces of
// the leaf crates they legitimately need.
pub use vrc_api::{
    ApiError, Client, ClientBuilder, CookieVault, KeyringVault, LoginOutcome, MemoryVault,
    TrustRank, TwoFactorMethod, USER_AGENT,
};
pub use vrc_logs::{default_log_dir, LogEvent, LogTailer};
pub use vrc_pipeline::PipelineConfig;
pub use vrc_pipeline::PipelineEvent;
pub use vrc_store::{
    FriendEvent, FriendHistoryEntry, FriendHistoryFilter, FriendTime, Stats, StatsRange, Store,
    TimelineEntry, TimelineFilter, TimelineKind, WorldTime,
};
