//! `vrcnext-cli` binary: headless clap CLI driving `vrc-core`.
//!
//! Auth cookies persist only in the OS credential store via
//! `vrc_api::KeyringVault` (RULES.md §4). Passwords and 2FA codes are read
//! with `rpassword` and held in memory as `SecretString`s for the duration
//! of the login call only.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use secrecy::SecretString;
use time::{Duration, OffsetDateTime};
use tokio::sync::broadcast;

use vrc_core::{
    format, friend_status_label, moderation_label, notification_kind_label, parse_param_value,
    sections, App, AppConfig, AppEvent, AppHandle, AuthState, BrowsePage, Client, Command,
    CreateInstanceRequest, DetailState, Favorite, FavoriteGroup, Group, GroupAccessType,
    InstanceKind, InstanceRegion, KeyringVault, LaunchPrefs, MediaFilter, MemoryVault, ParamValue,
    StatsRange, TimelineFilter, TimelineKind, TwoFactorMethod, World, USER_AGENT,
};

#[derive(Parser)]
#[command(name = "vrcnext-cli", version, about = "VRCNextRust headless CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Print the HTTP User-Agent used for all API requests
    Version,
    /// Log in with username/e-mail + password (and 2FA when required)
    Login {
        /// Account username or e-mail; prompted interactively when omitted
        username: Option<String>,
    },
    /// Show the currently logged-in user
    Me,
    /// List friends grouped by section (favorites, in-game, active, offline)
    Friends,
    /// Stream live pipeline events until Ctrl-C
    Watch,
    /// Log out and clear stored session cookies
    Logout,
    /// Show the personal timeline parsed from VRChat output logs
    Timeline {
        /// Restrict to one event kind
        #[arg(long, value_enum)]
        kind: Option<TimelineKindArg>,
        /// Case-insensitive text filter (name, world, location, path)
        #[arg(long)]
        query: Option<String>,
        /// Zero-based page index (50 rows per page)
        #[arg(long, default_value_t = 0)]
        page: u32,
    },
    /// Show time-spent statistics (worlds, friends)
    Stats {
        /// Time range to cover
        #[arg(long, value_enum, default_value_t = StatsRangeArg::AllTime)]
        range: StatsRangeArg,
    },
    /// Stream parsed VRChat log events until Ctrl-C (no login needed)
    TailLogs {
        /// Log directory override (default: the platform VRChat log dir)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// List notifications, or act on one (see/hide/accept/clear)
    Notifications {
        /// Action to perform; omitted lists notifications
        #[command(subcommand)]
        command: Option<NotificationCommand>,
    },
    /// Invite a user to an instance
    Invite {
        /// `usr_…` id of the user to invite
        user_id: String,
        /// Instance to invite to: `wrld_…:instanceId` or a bare instance id
        /// (default: your current instance)
        location: Option<String>,
        /// Custom invite message slot (0–11)
        #[arg(long)]
        slot: Option<u8>,
    },
    /// Ask a user to invite you to their instance
    RequestInvite {
        /// `usr_…` id of the user to ask
        user_id: String,
        /// Custom request message slot (0–11)
        #[arg(long)]
        slot: Option<u8>,
    },
    /// Invite several users to your instance (throttled, cancellable)
    MultiInvite {
        /// `usr_…` ids of the users to invite (max 50)
        user_ids: Vec<String>,
        /// Instance to invite to: `wrld_…:instanceId` or a bare instance id
        /// (default: your current instance)
        #[arg(long)]
        location: Option<String>,
    },
    /// Show a user's profile, friend status, note and moderations
    Profile {
        /// `usr_…` id of the user to inspect
        user_id: String,
    },
    /// Set your note on a user
    Note {
        /// `usr_…` id of the user
        user_id: String,
        /// Note text (multiple words are joined with spaces)
        note: Vec<String>,
    },
    /// Browse worlds (search, active, recent)
    Worlds {
        /// Which worlds feed to list
        #[command(subcommand)]
        command: WorldsCommand,
    },
    /// Show one world's public details
    World {
        /// `wrld_…` world id
        world_id: String,
    },
    /// Browse groups (search or your own memberships)
    Groups {
        /// Which groups feed to list
        #[command(subcommand)]
        command: GroupsCommand,
    },
    /// Show one group's details (membership, galleries, roles)
    Group {
        /// `grp_…` group id
        group_id: String,
    },
    /// Browse avatars (featured or your own)
    Avatars {
        /// Which avatar feed to list
        #[command(subcommand)]
        command: AvatarsCommand,
    },
    /// Inspect or switch into avatars
    Avatar {
        /// Action to perform
        #[command(subcommand)]
        command: AvatarCommand,
    },
    /// List your favorites (worlds, avatars, friends, groups)
    Favorites {
        /// Restrict to one favorite list; omitted lists everything
        #[command(subcommand)]
        command: Option<FavoritesCommand>,
    },
    /// Create a world or group instance
    InstanceCreate {
        /// `wrld_…` world id to instance
        world_id: String,
        /// Instance type
        #[arg(long, value_enum, default_value_t = InstanceKindArg::Public)]
        kind: InstanceKindArg,
        /// Instance region
        #[arg(long, value_enum, default_value_t = InstanceRegionArg::Us)]
        region: InstanceRegionArg,
        /// `grp_…` group id — required for --kind group
        #[arg(long)]
        group: Option<String>,
        /// Group access type (only with --group)
        #[arg(long, value_enum)]
        access: Option<GroupAccessArg>,
        /// `grol_…` role ids allowed to join a members-only group instance
        #[arg(long)]
        roles: Vec<String>,
        /// Invite+ (only with --kind private): others may request an invite
        #[arg(long)]
        can_request_invite: bool,
    },
    /// Send yourself an invite to an instance (`wrld_…:instanceId`)
    SelfInvite {
        /// Instance location `wrld_…:instanceId`
        location: String,
    },
    /// Manage the local photo library (no login needed)
    Media {
        /// Action to perform
        #[command(subcommand)]
        command: MediaCommand,
    },
    /// Launch VRChat or a companion app (no login needed)
    Launch {
        /// VRChat install dir override (default: Steam auto-detect)
        #[arg(long)]
        path: Option<String>,
        /// `--no-vr`: start in desktop mode
        #[arg(long)]
        no_vr: bool,
        /// `--profile=N` local profile number
        #[arg(long)]
        profile: Option<u32>,
        /// `--fps=N` frame-rate cap
        #[arg(long)]
        fps: Option<u32>,
        /// Join a world: `wrld_…` (instance via --instance)
        #[arg(long)]
        world: Option<String>,
        /// Instance id for --world
        #[arg(long)]
        instance: Option<String>,
        /// Launch an arbitrary executable instead of VRChat
        #[arg(long)]
        exe: Option<String>,
        /// Arguments for --exe (place after `--`)
        #[arg(last = true)]
        exe_args: Vec<String>,
    },
    /// Manage configured companion apps (no login needed)
    Companions {
        /// Action to perform
        #[command(subcommand)]
        command: CompanionsCommand,
    },
    /// Send a chatbox message or typing indicator (no login needed)
    Chatbox {
        /// Message text; omitted shows usage for the typing toggle
        text: Vec<String>,
        /// Send immediately instead of typing it out
        #[arg(long)]
        immediate: bool,
        /// Toggle the typing indicator instead of sending text
        #[arg(
            long,
            conflicts_with = "text",
            num_args = 0..=1,
            default_missing_value = "true"
        )]
        typing: Option<bool>,
    },
    /// Send/listen for OSC avatar parameters (no login needed)
    Osc {
        /// Action to perform
        #[command(subcommand)]
        command: OscCommand,
    },
    /// Browse the events calendar (login needed)
    Calendar {
        /// Action to perform
        #[command(subcommand)]
        command: CalendarCommand,
    },
}

/// `vrcnext-cli companions …` actions.
#[derive(Subcommand)]
enum CompanionsCommand {
    /// List configured companions
    List,
    /// Add a companion (name, executable path, args after `--`)
    Add {
        /// Display name
        name: String,
        /// Executable path
        path: String,
        /// Arguments (place after `--`)
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Remove a companion by index (`companions list` shows indices)
    Remove {
        /// Zero-based index
        index: usize,
    },
    /// Launch a companion by index
    Launch {
        /// Zero-based index
        index: usize,
    },
}

/// `vrcnext-cli osc …` actions.
#[derive(Subcommand)]
enum OscCommand {
    /// Send one avatar parameter: `/avatar/parameters/<name> <value>`
    Send {
        /// Full OSC address (must start with /avatar/parameters/)
        address: String,
        /// Value: float, int or true/false
        value: String,
    },
    /// Listen for avatar parameters until Ctrl-C
    Listen {
        /// UDP port to bind (default: VRChat's 9001)
        #[arg(long, default_value_t = 9001)]
        port: u16,
    },
}

/// `vrcnext-cli calendar …` actions.
#[derive(Subcommand)]
enum CalendarCommand {
    /// Your calendar events for a month
    Month {
        /// Month key (API `date` value, e.g. 2026-03-01); default: current
        #[arg(long)]
        date: Option<String>,
        /// Zero-based offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Discover public events
    Discover {
        /// Discovery scope
        #[arg(long, value_enum, default_value_t = DiscoverScopeArg::Upcoming)]
        scope: DiscoverScopeArg,
        /// Cursor from a previous `discover` call
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Search events by title
    Search {
        /// Free-text search term
        query: String,
        /// Zero-based offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// A group's calendar events
    Group {
        /// `grp_…` group id
        group_id: String,
        /// Month key; default: API default
        #[arg(long)]
        date: Option<String>,
    },
    /// A group's next upcoming event
    Next {
        /// `grp_…` group id
        group_id: String,
    },
    /// Follow or unfollow a group calendar event
    Follow {
        /// `grp_…` group id
        group_id: String,
        /// `cal_…` event id
        calendar_id: String,
        /// Unfollow instead of follow
        #[arg(long)]
        unfollow: bool,
    },
}

/// `--scope` values for `calendar discover`.
#[derive(Clone, Copy, ValueEnum)]
enum DiscoverScopeArg {
    All,
    Live,
    Upcoming,
}

impl From<DiscoverScopeArg> for vrc_core::DiscoverScope {
    fn from(arg: DiscoverScopeArg) -> Self {
        match arg {
            DiscoverScopeArg::All => vrc_core::DiscoverScope::All,
            DiscoverScopeArg::Live => vrc_core::DiscoverScope::Live,
            DiscoverScopeArg::Upcoming => vrc_core::DiscoverScope::Upcoming,
        }
    }
}

/// `vrcnext-cli media …` actions.
#[derive(Subcommand)]
enum MediaCommand {
    /// Rescan all configured folders
    Scan,
    /// List indexed photos
    List {
        /// Only favorited photos
        #[arg(long)]
        favorites: bool,
        /// Only photos under this scan folder
        #[arg(long)]
        folder: Option<String>,
        /// Case-insensitive text filter (path, world name)
        #[arg(long)]
        query: Option<String>,
        /// Rows to return
        #[arg(long, default_value_t = 60)]
        limit: u32,
        /// Rows to skip
        #[arg(long, default_value_t = 0)]
        offset: u32,
    },
    /// Manage scan folders
    Folders {
        /// Action to perform
        #[command(subcommand)]
        command: MediaFoldersCommand,
    },
}

/// `vrcnext-cli media folders …` actions.
#[derive(Subcommand)]
enum MediaFoldersCommand {
    /// List effective scan folders (platform default + custom)
    List,
    /// Add a custom scan folder and rescan
    Add {
        /// Absolute folder path to scan recursively
        path: String,
    },
    /// Remove a custom scan folder and its index rows
    Remove {
        /// Folder path previously added with `add`
        path: String,
    },
}

/// `vrcnext-cli worlds …` actions.
#[derive(Subcommand)]
enum WorldsCommand {
    /// Search/browse worlds by name
    Search {
        /// Free-text world-name filter (empty = browse defaults)
        query: Option<String>,
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Worlds currently active near you
    Active {
        /// Free-text world-name filter
        query: Option<String>,
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Worlds you recently visited
    Recent {
        /// Free-text world-name filter
        query: Option<String>,
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
}

/// `vrcnext-cli groups …` actions.
#[derive(Subcommand)]
enum GroupsCommand {
    /// Groups you belong to (`GET /users/{me}/groups`, unpaged)
    Own,
    /// Search groups by name
    Search {
        /// Free-text group-name filter
        #[arg(long)]
        query: Option<String>,
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
}

/// `vrcnext-cli avatars …` actions.
#[derive(Subcommand)]
enum AvatarsCommand {
    /// Featured avatars
    Featured {
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Your own avatars
    Own {
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
}

/// `vrcnext-cli avatar …` actions.
#[derive(Subcommand)]
enum AvatarCommand {
    /// Show one avatar's public details
    Show {
        /// `avtr_…` avatar id
        avatar_id: String,
    },
    /// Switch into an avatar (requires --yes)
    Select {
        /// `avtr_…` avatar id to switch into
        avatar_id: String,
        /// Confirm the switch without prompting
        #[arg(long)]
        yes: bool,
    },
}

/// `vrcnext-cli favorites …` actions.
#[derive(Subcommand)]
enum FavoritesCommand {
    /// List every favorite kind (the default when no subcommand is given)
    All,
    /// List favorite groups only (`GET /favorite/groups`)
    Groups {
        /// Zero-based item offset for paging
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
}

/// Actions for `vrcnext-cli notifications`.
#[derive(Subcommand)]
enum NotificationCommand {
    /// Mark a notification as seen
    See {
        /// `not_…`/`frq_…` notification id
        id: String,
    },
    /// Hide/delete a notification
    Hide {
        /// `not_…`/`frq_…` notification id
        id: String,
    },
    /// Accept a friend-request notification
    Accept {
        /// `frq_…` friend-request notification id
        id: String,
    },
    /// Clear all notifications
    Clear,
}

/// `--kind` values for `timeline` (kebab-case on the command line).
#[derive(Clone, Copy, ValueEnum)]
enum TimelineKindArg {
    JoinedWorld,
    LeftWorld,
    PlayerJoined,
    PlayerLeft,
    PhotoTaken,
}

impl From<TimelineKindArg> for TimelineKind {
    fn from(arg: TimelineKindArg) -> Self {
        match arg {
            TimelineKindArg::JoinedWorld => TimelineKind::JoinedWorld,
            TimelineKindArg::LeftWorld => TimelineKind::LeftWorld,
            TimelineKindArg::PlayerJoined => TimelineKind::PlayerJoined,
            TimelineKindArg::PlayerLeft => TimelineKind::PlayerLeft,
            TimelineKindArg::PhotoTaken => TimelineKind::PhotoTaken,
        }
    }
}

/// `--kind` values for `instance-create`.
#[derive(Clone, Copy, ValueEnum)]
enum InstanceKindArg {
    Public,
    Hidden,
    Friends,
    Private,
    Group,
}

impl From<InstanceKindArg> for InstanceKind {
    fn from(arg: InstanceKindArg) -> Self {
        match arg {
            InstanceKindArg::Public => InstanceKind::Public,
            InstanceKindArg::Hidden => InstanceKind::Hidden,
            InstanceKindArg::Friends => InstanceKind::Friends,
            InstanceKindArg::Private => InstanceKind::Private,
            InstanceKindArg::Group => InstanceKind::Group,
        }
    }
}

/// `--region` values for `instance-create`.
#[derive(Clone, Copy, ValueEnum)]
enum InstanceRegionArg {
    Us,
    Use,
    Eu,
    Jp,
    Unknown,
}

impl From<InstanceRegionArg> for InstanceRegion {
    fn from(arg: InstanceRegionArg) -> Self {
        match arg {
            InstanceRegionArg::Us => InstanceRegion::Us,
            InstanceRegionArg::Use => InstanceRegion::Use,
            InstanceRegionArg::Eu => InstanceRegion::Eu,
            InstanceRegionArg::Jp => InstanceRegion::Jp,
            InstanceRegionArg::Unknown => InstanceRegion::Unknown,
        }
    }
}

/// `--access` values for `instance-create --group`.
#[derive(Clone, Copy, ValueEnum)]
enum GroupAccessArg {
    Public,
    Plus,
    Members,
}

impl From<GroupAccessArg> for GroupAccessType {
    fn from(arg: GroupAccessArg) -> Self {
        match arg {
            GroupAccessArg::Public => GroupAccessType::Public,
            GroupAccessArg::Plus => GroupAccessType::Plus,
            GroupAccessArg::Members => GroupAccessType::Members,
        }
    }
}

/// `--range` values for `stats` (mirrors the UI presets).
#[derive(Clone, Copy, ValueEnum)]
enum StatsRangeArg {
    AllTime,
    Last24h,
    Last7d,
    Last30d,
}

impl StatsRangeArg {
    fn range(self) -> StatsRange {
        let now = OffsetDateTime::now_utc();
        let since = match self {
            Self::AllTime => None,
            Self::Last24h => Some(now - Duration::hours(24)),
            Self::Last7d => Some(now - Duration::days(7)),
            Self::Last30d => Some(now - Duration::days(30)),
        };
        StatsRange { since, until: None }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Version => {
            println!("{USER_AGENT}");
            Ok(())
        }
        Commands::Login { username } => login(username).await,
        Commands::Me => me().await,
        Commands::Friends => friends().await,
        Commands::Watch => watch().await,
        Commands::Logout => logout().await,
        Commands::Timeline { kind, query, page } => timeline(kind, query, page).await,
        Commands::Stats { range } => stats(range).await,
        Commands::TailLogs { dir } => tail_logs(dir).await,
        Commands::Notifications { command } => notifications(command).await,
        Commands::Invite {
            user_id,
            location,
            slot,
        } => invite(&user_id, location.as_deref(), slot).await,
        Commands::RequestInvite { user_id, slot } => request_invite(&user_id, slot).await,
        Commands::MultiInvite { user_ids, location } => {
            multi_invite(user_ids, location.as_deref()).await
        }
        Commands::Profile { user_id } => profile(&user_id).await,
        Commands::Note { user_id, note } => set_note(&user_id, note).await,
        Commands::Worlds { command } => match command {
            WorldsCommand::Search { query, offset } => worlds_search(query, offset).await,
            WorldsCommand::Active { query, offset } => {
                worlds_feed(WorldsFeedKind::Active, query, offset).await
            }
            WorldsCommand::Recent { query, offset } => {
                worlds_feed(WorldsFeedKind::Recent, query, offset).await
            }
        },
        Commands::World { world_id } => world(&world_id).await,
        Commands::Groups { command } => match command {
            GroupsCommand::Own => own_groups().await,
            GroupsCommand::Search { query, offset } => groups_search(query, offset).await,
        },
        Commands::Group { group_id } => group(&group_id).await,
        Commands::Avatars { command } => match command {
            AvatarsCommand::Featured { offset } => avatars(false, offset).await,
            AvatarsCommand::Own { offset } => avatars(true, offset).await,
        },
        Commands::Avatar { command } => match command {
            AvatarCommand::Show { avatar_id } => avatar(&avatar_id).await,
            AvatarCommand::Select { avatar_id, yes } => avatar_select(&avatar_id, yes).await,
        },
        Commands::Favorites { command } => match command {
            Some(FavoritesCommand::Groups { offset }) => favorite_groups(offset).await,
            _ => favorites().await,
        },
        Commands::InstanceCreate {
            world_id,
            kind,
            region,
            group,
            access,
            roles,
            can_request_invite,
        } => {
            instance_create(
                world_id,
                kind,
                region,
                group,
                access,
                roles,
                can_request_invite,
            )
            .await
        }
        Commands::SelfInvite { location } => self_invite(&location).await,
        Commands::Media { command } => match command {
            MediaCommand::Scan => media_scan().await,
            MediaCommand::List {
                favorites,
                folder,
                query,
                limit,
                offset,
            } => media_list(favorites, folder, query, limit, offset).await,
            MediaCommand::Folders { command } => match command {
                MediaFoldersCommand::List => media_folders_list().await,
                MediaFoldersCommand::Add { path } => media_folders_add(&path).await,
                MediaFoldersCommand::Remove { path } => media_folders_remove(&path).await,
            },
        },
        Commands::Launch {
            path,
            no_vr,
            profile,
            fps,
            world,
            instance,
            exe,
            exe_args,
        } => {
            launch(LaunchCmd {
                path,
                no_vr,
                profile,
                fps,
                world,
                instance,
                exe,
                exe_args,
            })
            .await
        }
        Commands::Companions { command } => match command {
            CompanionsCommand::List => companions_list().await,
            CompanionsCommand::Add { name, path, args } => companions_add(&name, &path, args).await,
            CompanionsCommand::Remove { index } => companions_remove(index).await,
            CompanionsCommand::Launch { index } => companions_launch(index).await,
        },
        Commands::Chatbox {
            text,
            immediate,
            typing,
        } => chatbox(text, immediate, typing).await,
        Commands::Osc { command } => match command {
            OscCommand::Send { address, value } => osc_send(&address, &value).await,
            OscCommand::Listen { port } => osc_listen(port).await,
        },
        Commands::Calendar { command } => calendar(command).await,
    }
}

/// REST client backed by the OS credential store.
fn build_client() -> Result<Client> {
    let vault = KeyringVault::new().context("opening the OS credential store")?;
    Client::builder()
        .vault(vault)
        .build()
        .context("building the REST client")
}

/// Client for commands that never authenticate: local data comes from the
/// SQLite store, so an in-memory vault keeps these working on machines
/// without an OS secret service.
fn build_local_client() -> Result<Client> {
    Client::builder()
        .vault(MemoryVault::new())
        .build()
        .context("building the REST client")
}

/// True when the vault holds a session to resume.
fn vault_has_session(vault: &impl vrc_core::CookieVault) -> Result<bool> {
    Ok(vault.load()?.is_some())
}

/// Client for commands that need authentication: refuses to run without
/// stored credentials so no HTTP request goes out while logged out.
fn logged_in_client() -> Result<Client> {
    let vault = KeyringVault::new().context("opening the OS credential store")?;
    if !vault_has_session(&vault).context("reading stored credentials")? {
        eprintln!("not logged in — run `vrcnext-cli login`");
        std::process::exit(1);
    }
    Client::builder()
        .vault(vault)
        .build()
        .context("building the REST client")
}

/// App config for every CLI command. The CLI shares `vrc.db` and the log
/// directory with the desktop app, so it must not tail logs, persist
/// events, or run session cleanup — a second writer would close live
/// sessions and duplicate rows.
fn cli_config(client: Client) -> AppConfig {
    AppConfig {
        local_writes: false,
        ..AppConfig::new(client)
    }
}

/// Wait for the next event matching `wanted`; errors abort the command.
/// Times out rather than hanging forever.
async fn wait_for(handle: &mut AppHandle, wanted: impl Fn(&AppEvent) -> bool) -> Result<AppEvent> {
    const WAIT: std::time::Duration = std::time::Duration::from_secs(60);
    loop {
        let next = tokio::time::timeout(WAIT, handle.events.recv())
            .await
            .context("timed out waiting for app event")?;
        match next {
            Ok(event) if wanted(&event) => return Ok(event),
            Ok(AppEvent::Error(message)) => bail!(message),
            Ok(_) => {}
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(e) => bail!("event stream closed: {e}"),
        }
    }
}

/// Send `Command::ResumeSession` and wait for the auth outcome.
async fn ensure_session(handle: &mut AppHandle) -> Result<()> {
    handle
        .commands
        .send(Command::ResumeSession)
        .await
        .context("sending ResumeSession")?;
    match wait_for(handle, |e| matches!(e, AppEvent::AuthChanged(_))).await? {
        AppEvent::AuthChanged(AuthState::Authenticated) => Ok(()),
        _ => bail!("not logged in — run `vrcnext-cli login` first"),
    }
}

async fn login(username: Option<String>) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_client()?));

    let username = match username {
        Some(username) => username,
        None => {
            print!("Username or e-mail: ");
            std::io::stdout().flush()?;
            let mut input = String::new();
            std::io::stdin()
                .read_line(&mut input)
                .context("reading username")?;
            input.trim().to_string()
        }
    };
    let password =
        SecretString::from(rpassword::prompt_password("Password: ").context("reading password")?);

    handle
        .commands
        .send(Command::Login { username, password })
        .await?;

    match wait_for(&mut handle, |e| matches!(e, AppEvent::AuthChanged(_))).await? {
        AppEvent::AuthChanged(AuthState::Authenticated) => {
            println!("Logged in.");
            Ok(())
        }
        AppEvent::AuthChanged(AuthState::Needs2fa(methods)) => {
            let method = *methods.first().context("empty 2FA method list")?;
            let label = match method {
                TwoFactorMethod::Totp => "authenticator app (totp)",
                TwoFactorMethod::EmailOtp => "e-mail (emailOtp)",
                TwoFactorMethod::RecoveryOtp => "recovery code (otp)",
            };
            eprintln!("Two-factor required; using {label} of {methods:?}");
            let code = SecretString::from(
                rpassword::prompt_password("2FA code: ").context("reading 2FA code")?,
            );
            handle
                .commands
                .send(Command::Submit2fa { method, code })
                .await?;
            match wait_for(&mut handle, |e| {
                matches!(e, AppEvent::AuthChanged(AuthState::Authenticated))
            })
            .await?
            {
                AppEvent::AuthChanged(AuthState::Authenticated) => {
                    println!("Logged in.");
                    Ok(())
                }
                other => bail!("unexpected event: {other:?}"),
            }
        }
        other => bail!("unexpected event: {other:?}"),
    }
}

async fn me() -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    let state = handle.state.snapshot();
    let user = state.current_user.context("no user despite session")?;
    println!("{}  <{}>", user.display_name, user.id);
    println!("  status:  {} — {}", user.status, user.status_description);
    println!("  trust:   {:?}", user.trust_rank());
    println!("  friends: {}", user.friends.len());
    Ok(())
}

async fn friends() -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::FriendsRefreshed)).await?;
    let state = handle.state.snapshot();
    let s = sections(&state);
    for (title, list) in [
        ("Favorites", &s.favorites),
        ("In-Game", &s.in_game),
        ("Web/Active", &s.web_active),
        ("Offline", &s.offline),
    ] {
        println!("== {title} ({}) ==", list.len());
        for f in list {
            let where_ = if f.location.is_empty() {
                f.status.clone()
            } else {
                f.location.clone()
            };
            println!("  {:<32} {:?}  {}", f.display_name, f.trust_rank, where_);
        }
    }
    Ok(())
}

async fn watch() -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    println!("Watching pipeline events (Ctrl-C to stop)…");
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Ok(AppEvent::Pipeline(e)) => println!("{e:?}"),
                Ok(AppEvent::AuthChanged(a)) => println!("auth: {a:?}"),
                Ok(AppEvent::Error(m)) => println!("error: {m}"),
                Ok(AppEvent::FriendsRefreshed) => println!("friends refreshed"),
                Ok(AppEvent::WorldNameResolved { world_id }) => {
                    println!("world name resolved: {world_id}")
                }
                Ok(AppEvent::TimelineLoaded) => println!("timeline loaded"),
                Ok(AppEvent::FriendHistoryLoaded) => println!("friend history loaded"),
                Ok(AppEvent::StatsLoaded) => println!("stats loaded"),
                Ok(AppEvent::NotificationsChanged) => println!("notifications changed"),
                Ok(AppEvent::ProfileChanged) => println!("profile changed"),
                Ok(AppEvent::InstanceLoaded) => println!("instance loaded"),
                Ok(AppEvent::InviteSent) => println!("invite sent"),
                Ok(AppEvent::MultiInviteProgress) => println!("multi-invite progress"),
                Ok(AppEvent::BrowseChanged) => println!("browse data changed"),
                Ok(AppEvent::MediaLoaded) => println!("media loaded"),
                Ok(AppEvent::MediaScanFinished) => println!("media scan finished"),
                Ok(AppEvent::MediaChanged) => println!("media settings changed"),
                Ok(AppEvent::ThumbnailReady) => println!("thumbnail ready"),
                Ok(AppEvent::Ready) => println!("ready"),
                Ok(AppEvent::ToolsChanged) => println!("tools changed"),
                Ok(AppEvent::ProcessLaunched) => println!("process launched"),
                Ok(AppEvent::CalendarChanged) => println!("calendar changed"),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    println!("(missed {n} events)");
                }
                Err(_) => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

async fn logout() -> Result<()> {
    let mut handle = App::spawn(cli_config(build_client()?));
    handle.commands.send(Command::Logout).await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::AuthChanged(AuthState::LoggedOut))
    })
    .await?;
    println!("Logged out.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Local data (M3): no login required — everything comes from the SQLite
// store the core keeps under the platform data directory.
// ---------------------------------------------------------------------------

async fn timeline(kind: Option<TimelineKindArg>, query: Option<String>, page: u32) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    let kind = kind.map(TimelineKind::from);
    let query = query.filter(|q| !q.trim().is_empty());
    handle
        .commands
        .send(Command::LoadTimeline {
            page,
            filter: TimelineFilter {
                kind,
                query,
                ..TimelineFilter::default()
            },
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::TimelineLoaded)).await?;

    let state = handle.state.snapshot();
    let loaded = state.timeline.context("timeline query returned nothing")?;
    for entry in &loaded.entries {
        println!(
            "{}  {:<14} {}",
            format::fmt_ts(entry.ts),
            format::kind_label(Some(entry.kind)),
            format::timeline_summary(entry)
        );
    }
    println!(
        "-- page {} · {} of {} row(s) --",
        page,
        loaded.entries.len(),
        loaded.total
    );
    Ok(())
}

async fn stats(range: StatsRangeArg) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::LoadStats {
            range: range.range(),
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::StatsLoaded)).await?;

    let state = handle.state.snapshot();
    let stats = state.stats.context("stats query returned nothing")?;
    println!(
        "Total: {} across {} session(s)",
        format::format_duration(stats.total_seconds),
        stats.session_count
    );
    println!("Top worlds:");
    if stats.top_worlds.is_empty() {
        println!("  —");
    }
    for world in &stats.top_worlds {
        println!(
            "  {} — {} ({} session(s))",
            format::world_label(world),
            format::format_duration(world.seconds),
            world.sessions
        );
    }
    println!("Time with friends:");
    if stats.top_friends.is_empty() {
        println!("  —");
    }
    for friend in &stats.top_friends {
        println!(
            "  {} — {}",
            friend.display_name,
            format::format_duration(friend.seconds)
        );
    }
    Ok(())
}

async fn tail_logs(dir: Option<PathBuf>) -> Result<()> {
    let dir = dir
        .or_else(vrc_core::default_log_dir)
        .context("no VRChat log directory found; pass --dir")?;
    if !dir.is_dir() {
        bail!("log directory does not exist: {}", dir.display());
    }
    println!("Tailing {} (Ctrl-C to stop)", dir.display());
    let mut events = vrc_core::LogTailer::watch(dir);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(event) => println!("{event}"),
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Social actions (M4)
// ---------------------------------------------------------------------------

/// Resolve a notification sender id to a display name via the friends map.
fn sender_name(state: &vrc_core::AppState, user_id: &str, fallback: &str) -> String {
    if user_id.is_empty() {
        return if fallback.is_empty() {
            "System".to_string()
        } else {
            fallback.to_string()
        };
    }
    if let Some(friend) = state.friends.get(user_id) {
        return friend.display_name.clone();
    }
    if state.current_user.as_ref().is_some_and(|u| u.id == user_id) {
        if let Some(user) = &state.current_user {
            return user.display_name.clone();
        }
    }
    if !fallback.is_empty() {
        fallback.to_string()
    } else {
        user_id.to_string()
    }
}

async fn notifications(command: Option<NotificationCommand>) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;

    let command = match command {
        Some(command) => command,
        None => {
            handle.commands.send(Command::RefreshNotifications).await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await?;
            print_notifications(&handle.state.snapshot());
            return Ok(());
        }
    };

    let sent = match &command {
        NotificationCommand::See { id } => Command::SeeNotification { id: id.clone() },
        NotificationCommand::Hide { id } => Command::HideNotification { id: id.clone() },
        NotificationCommand::Accept { id } => Command::AcceptNotification { id: id.clone() },
        NotificationCommand::Clear => Command::ClearNotifications,
    };
    handle.commands.send(sent).await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::NotificationsChanged)).await?;
    println!("Done.");
    Ok(())
}

fn print_notifications(state: &vrc_core::AppState) {
    let Some(list) = &state.notifications else {
        println!("No notifications loaded.");
        return;
    };
    for n in list {
        let seen = if n.seen { " " } else { "*" };
        println!(
            "{seen} {:<20} {:<28} {}",
            notification_kind_label(&n.kind),
            sender_name(state, &n.sender_user_id, &n.sender_username),
            n.message,
        );
        println!("    {}  {}", n.id, format::fmt_iso_ts(&n.created_at));
    }
    println!("-- {} notification(s) --", list.len());
    for v2 in &state.notifications_v2 {
        let seen = if v2.seen { " " } else { "*" };
        println!(
            "{seen} {:<20} {}",
            notification_kind_label(&v2.kind),
            if v2.title.is_empty() {
                v2.message.clone()
            } else {
                v2.title.clone()
            },
        );
        println!("    {}  {}", v2.id, format::fmt_iso_ts(&v2.created_at));
    }
}

/// A `wrld_…:instanceId` location reduces to the bare instance id the
/// invite endpoints want; a bare instance id passes through unchanged.
fn instance_id_from_location(location: &str) -> &str {
    location
        .split_once(':')
        .map(|(_, instance)| instance)
        .unwrap_or(location)
}

async fn invite(user_id: &str, location: Option<&str>, slot: Option<u8>) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::InviteUser {
            user_id: user_id.to_string(),
            instance_id: location.map(|l| instance_id_from_location(l).to_string()),
            message_slot: slot,
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await?;
    println!("Invite sent.");
    Ok(())
}

async fn request_invite(user_id: &str, slot: Option<u8>) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::RequestInvite {
            user_id: user_id.to_string(),
            request_slot: slot,
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await?;
    println!("Invite request sent.");
    Ok(())
}

async fn multi_invite(user_ids: Vec<String>, location: Option<&str>) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::MultiInvite {
            user_ids,
            instance_id: location.map(|l| instance_id_from_location(l).to_string()),
        })
        .await?;

    // Print progress as it lands; first Ctrl-C cancels the batch politely,
    // a second one gives up waiting.
    let mut cancel_sent = false;
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Ok(AppEvent::MultiInviteProgress) => match handle.state.snapshot().multi_invite {
                    Some(p) => {
                        println!(
                            "{}/{} — {} sent, {} failed{}",
                            p.done,
                            p.total,
                            p.succeeded,
                            p.failed,
                            p.current
                                .as_ref()
                                .map(|id| format!(", inviting {id}"))
                                .unwrap_or_default(),
                        );
                        if !p.running {
                            break;
                        }
                    }
                    // The batch slot is gone — torn down mid-run.
                    None => break,
                },
                // A command-level failure (bad ids, no instance) emits
                // Error without ever publishing progress — bail instead
                // of spinning forever. Mid-batch invite failures only
                // update progress counters, they don't emit Error.
                Ok(AppEvent::Error(m)) => {
                    let running = handle
                        .state
                        .snapshot()
                        .multi_invite
                        .as_ref()
                        .is_some_and(|p| p.running);
                    if running {
                        eprintln!("error: {m}");
                    } else {
                        bail!("multi-invite failed: {m}");
                    }
                }
                Ok(AppEvent::AuthChanged(AuthState::LoggedOut)) => break,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            _ = tokio::signal::ctrl_c() => {
                if cancel_sent {
                    eprintln!("Giving up wait — the batch may still be running.");
                    break;
                }
                eprintln!("Cancelling after the in-flight invite… (Ctrl-C again to quit)");
                let _ = handle.commands.send(Command::CancelMultiInvite).await;
                cancel_sent = true;
            }
        }
    }
    let p = handle.state.snapshot().multi_invite;
    if let Some(p) = p {
        println!(
            "Done: {} sent, {} failed of {}",
            p.succeeded, p.failed, p.total
        );
    }
    Ok(())
}

async fn profile(user_id: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadProfile {
            user_id: user_id.to_string(),
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await?;

    let state = handle.state.snapshot();
    let profile = state.profile.context("profile query returned nothing")?;
    let user = &profile.user;
    println!("{}  <{}>", user.display_name, user.username);
    println!("  id:       {}", user.id);
    println!(
        "  status:   {}",
        friend_status_label(&profile.friend_status)
    );
    if !user.bio.is_empty() {
        println!("  bio:      {}", user.bio);
    }
    if !user.location.is_empty() {
        println!("  location: {}", user.location);
    }
    if !profile.user.note.is_empty() {
        println!("  note:     {}", profile.user.note);
    }
    for m in &profile.moderations {
        println!("  moderation: {}", moderation_label(&m.kind));
    }
    Ok(())
}

async fn set_note(user_id: &str, note: Vec<String>) -> Result<()> {
    let note = note.join(" ");
    if note.trim().is_empty() {
        bail!("note text is empty");
    }
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::SetUserNote {
            user_id: user_id.to_string(),
            note,
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::ProfileChanged)).await?;
    println!("Note saved.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Browse actions (M5): worlds, groups, avatars, favorites, instances
// ---------------------------------------------------------------------------

/// Wait for the next `BrowseChanged` event (any M5 state update).
async fn wait_browse(handle: &mut AppHandle) -> Result<()> {
    wait_for(handle, |e| matches!(e, AppEvent::BrowseChanged)).await?;
    Ok(())
}

/// Which `/worlds/*` feed a `worlds` subcommand reads.
#[derive(Clone, Copy)]
enum WorldsFeedKind {
    Active,
    Recent,
}

fn print_worlds(page: &BrowsePage<World>) {
    for world in &page.items {
        println!(
            "{:<40} by {:<24} cap {:>4}  {}",
            world.name, world.author_name, world.capacity, world.id
        );
    }
    println!("-- {} world(s) --", page.items.len());
}

async fn worlds_search(query: Option<String>, offset: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::SearchWorlds {
            query: query.unwrap_or_default(),
            offset,
        })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(page) = &state.world_search else {
        bail!("no world results");
    };
    print_worlds(page);
    Ok(())
}

async fn worlds_feed(kind: WorldsFeedKind, query: Option<String>, offset: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    let query = query.unwrap_or_default();
    let command = match kind {
        WorldsFeedKind::Active => Command::SearchActiveWorlds { query, offset },
        WorldsFeedKind::Recent => Command::SearchRecentWorlds { query, offset },
    };
    handle.commands.send(command).await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let page = match kind {
        WorldsFeedKind::Active => &state.active_worlds,
        WorldsFeedKind::Recent => &state.recent_worlds,
    };
    let Some(page) = page else {
        bail!("no world results");
    };
    print_worlds(page);
    Ok(())
}

async fn world(world_id: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadWorld {
            world_id: world_id.to_string(),
        })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(DetailState::Known(world)) = state.world_details.get(world_id) else {
        bail!("world details unavailable for {world_id}");
    };
    println!("{}", world.name);
    println!("  id:       {}", world.id);
    if !world.author_name.is_empty() {
        println!("  author:   {}", world.author_name);
    }
    if world.capacity > 0 {
        println!("  capacity: {}", world.capacity);
    }
    if world.favorites > 0 {
        println!("  favorites: {}", world.favorites);
    }
    if !world.release_status.is_empty() {
        println!("  status:   {}", world.release_status);
    }
    if !world.url.is_empty() {
        println!("  url:      {}", world.url);
    }
    if !world.description.is_empty() {
        println!(
            "  about:    {}",
            world.description.lines().next().unwrap_or_default()
        );
    }
    Ok(())
}

fn print_groups(page: &BrowsePage<Group>) {
    for group in &page.items {
        println!(
            "{:<32} {:<10} {:>5} members ({:>4} online)  {}",
            group.name, group.short_code, group.member_count, group.online_member_count, group.id
        );
    }
    println!("-- {} group(s) --", page.items.len());
}

async fn groups_search(query: Option<String>, offset: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::SearchGroups {
            query: query.unwrap_or_default(),
            offset,
        })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(page) = &state.group_search else {
        bail!("no group results");
    };
    print_groups(page);
    Ok(())
}

async fn own_groups() -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle.commands.send(Command::LoadOwnGroups).await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(groups) = &state.own_groups else {
        bail!("your groups are not loaded");
    };
    if groups.is_empty() {
        println!("(no public group memberships)");
        return Ok(());
    }
    for group in groups {
        println!(
            "{:<32} {:<10} {:>5} members  {}",
            group.name, group.short_code, group.member_count, group.id
        );
        if group.is_representing {
            println!("    (representing)");
        }
    }
    println!("-- {} group(s) --", groups.len());
    Ok(())
}

async fn group(group_id: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadGroup {
            group_id: group_id.to_string(),
            include_roles: true,
        })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(DetailState::Known(group)) = state.group_details.get(group_id) else {
        bail!("group details unavailable for {group_id}");
    };
    println!("{}", group.name);
    println!("  id:       {}", group.id);
    if !group.short_code.is_empty() {
        println!("  code:     {}", group.short_code);
    }
    println!(
        "  members:  {} ({} online)",
        group.member_count, group.online_member_count
    );
    if !group.membership_status.is_empty() {
        println!("  status:   {}", group.membership_status);
    }
    println!(
        "  member:   {}",
        if group.my_member.is_some() {
            "yes"
        } else {
            "no"
        }
    );
    if !group.description.is_empty() {
        println!(
            "  about:    {}",
            group.description.lines().next().unwrap_or_default()
        );
    }
    println!(
        "  galleries: {} · roles: {}",
        group.galleries.len(),
        group.roles.len()
    );
    Ok(())
}

async fn avatars(own: bool, offset: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadAvatars { offset, own })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(page) = &state.avatar_search else {
        bail!("no avatar results");
    };
    for avatar in &page.items {
        println!(
            "{:<40} by {:<24} {:<10}  {}",
            avatar.name, avatar.author_name, avatar.release_status, avatar.id
        );
    }
    println!("-- {} avatar(s) --", page.items.len());
    Ok(())
}

async fn avatar(avatar_id: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadAvatar {
            avatar_id: avatar_id.to_string(),
        })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(DetailState::Known(avatar)) = state.avatar_details.get(avatar_id) else {
        bail!("avatar details unavailable for {avatar_id}");
    };
    println!("{}", avatar.name);
    println!("  id:       {}", avatar.id);
    if !avatar.author_name.is_empty() {
        println!("  author:   {}", avatar.author_name);
    }
    if !avatar.release_status.is_empty() {
        println!("  status:   {}", avatar.release_status);
    }
    if avatar.version > 0 {
        println!("  version:  {}", avatar.version);
    }
    if !avatar.description.is_empty() {
        println!(
            "  about:    {}",
            avatar.description.lines().next().unwrap_or_default()
        );
    }
    Ok(())
}

async fn avatar_select(avatar_id: &str, yes: bool) -> Result<()> {
    if !yes {
        bail!("refusing to switch avatar without --yes");
    }
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::SelectAvatar {
            avatar_id: avatar_id.to_string(),
            confirm: true,
        })
        .await?;
    wait_browse(&mut handle).await?;
    println!("Avatar switched.");
    Ok(())
}

fn print_favorite_groups(page: &BrowsePage<FavoriteGroup>) {
    for group in &page.items {
        let name = if group.display_name.is_empty() {
            group.name.as_str()
        } else {
            group.display_name.as_str()
        };
        println!(
            "{:<32} {:<8} {:<24} {}",
            name, group.kind, group.owner_display_name, group.id
        );
    }
    println!("-- {} favorite group(s) --", page.items.len());
}

fn print_friend_favorites(records: &[Favorite]) {
    let mut count = 0;
    for record in records {
        if record.kind == "friend" {
            println!("  {}", record.favorite_id);
            count += 1;
        }
    }
    println!("  -- {count} friend favorite(s) --");
}

async fn favorites() -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle.commands.send(Command::RefreshFavorites).await?;
    wait_browse(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadFavoriteGroups { offset: 0 })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    println!("== Worlds ==");
    if let Some(page) = &state.favorite_worlds {
        for world in &page.items {
            println!("  {}  {}", world.name, world.id);
        }
        println!("  -- {} world(s) --", page.items.len());
    } else {
        println!("  (none)");
    }
    println!("== Avatars ==");
    if let Some(page) = &state.favorite_avatars {
        for avatar in &page.items {
            println!("  {}  {}", avatar.name, avatar.id);
        }
        println!("  -- {} avatar(s) --", page.items.len());
    } else {
        println!("  (none)");
    }
    println!("== Friends ==");
    print_friend_favorites(&state.favorite_records);
    println!("== Favorite groups ==");
    if let Some(page) = &state.favorite_groups {
        print_favorite_groups(page);
    } else {
        println!("  (none)");
    }
    Ok(())
}

async fn favorite_groups(offset: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::LoadFavoriteGroups { offset })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let Some(page) = &state.favorite_groups else {
        bail!("no favorite groups");
    };
    print_favorite_groups(page);
    Ok(())
}

async fn instance_create(
    world_id: String,
    kind: InstanceKindArg,
    region: InstanceRegionArg,
    group: Option<String>,
    access: Option<GroupAccessArg>,
    roles: Vec<String>,
    can_request_invite: bool,
) -> Result<()> {
    let kind = InstanceKind::from(kind);
    let mut request = CreateInstanceRequest::new(world_id, kind, InstanceRegion::from(region));
    if kind == InstanceKind::Group {
        let Some(group_id) = group else {
            bail!("--kind group requires --group grp_…");
        };
        request.owner_id = Some(group_id);
        request.group_access_type = access.map(GroupAccessType::from);
        request.role_ids = roles;
    } else if group.is_some() || access.is_some() || !roles.is_empty() {
        bail!("--group/--access/--roles only apply to --kind group");
    }
    if can_request_invite {
        if kind != InstanceKind::Private {
            bail!("--can-request-invite only applies to --kind private");
        }
        request.can_request_invite = Some(true);
    }
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::CreateInstance { request })
        .await?;
    wait_browse(&mut handle).await?;
    let state = handle.state.snapshot();
    let instance = state
        .instance
        .context("instance creation returned nothing")?;
    println!(
        "Instance created: {} ({})",
        instance.location, instance.instance_id
    );
    Ok(())
}

async fn self_invite(location: &str) -> Result<()> {
    let (world_id, instance_id) = location
        .split_once(':')
        .context("location must be wrld_…:instanceId")?;
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    handle
        .commands
        .send(Command::InviteMyselfToInstance {
            world_id: world_id.to_string(),
            instance_id: instance_id.to_string(),
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::InviteSent)).await?;
    println!("Invite sent.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Media library (M6): local data only — no login required.
// ---------------------------------------------------------------------------

/// Human-readable file size (`1.4 MiB`).
fn fmt_bytes(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes.max(0) as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

async fn media_scan() -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle.commands.send(Command::ScanMedia).await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::MediaScanFinished)).await?;
    let state = handle.state.snapshot();
    let scan = state
        .media_last_scan
        .context("scan finished without a result")?;
    println!(
        "Scan complete: {} folder(s), {} file(s) seen — {} upserted, {} removed, {} error(s)",
        scan.folders, scan.files, scan.upserted, scan.removed, scan.errors
    );
    Ok(())
}

async fn media_list(
    favorites: bool,
    folder: Option<String>,
    query: Option<String>,
    limit: u32,
    offset: u32,
) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::LoadMedia {
            filter: MediaFilter {
                favorites_only: favorites,
                folder,
                query: query.filter(|q| !q.trim().is_empty()),
                offset,
                limit,
                ..MediaFilter::default()
            },
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::MediaLoaded)).await?;

    let state = handle.state.snapshot();
    let page = state
        .media
        .as_ref()
        .context("media query returned nothing")?;
    for item in &page.items {
        let fav = if item.is_favorite { "*" } else { " " };
        let dims = if item.width > 0 {
            format!("{}x{}", item.width, item.height)
        } else {
            "?x?".to_string()
        };
        println!(
            "{fav} {}  {:>10}  {:>7}  {}",
            format::fmt_ts(item.taken_at),
            dims,
            fmt_bytes(item.size),
            item.path
        );
        if !item.world_name.is_empty() || !item.world_id.is_empty() {
            let world = if item.world_name.is_empty() {
                item.world_id.as_str()
            } else {
                item.world_name.as_str()
            };
            println!("    world: {world}");
        }
    }
    println!(
        "-- {} of {} photo(s) --",
        page.items.len(),
        page.total.unwrap_or(0)
    );
    Ok(())
}

/// Settings hydration runs before the command loop; give it a moment so
/// `media folders list` doesn't race an empty snapshot.
async fn wait_media_folders(handle: &AppHandle) {
    for _ in 0..100 {
        if !handle.state.snapshot().media_folders.is_empty() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

async fn media_folders_list() -> Result<()> {
    let handle = App::spawn(cli_config(build_local_client()?));
    wait_media_folders(&handle).await;
    for folder in &handle.state.snapshot().media_folders {
        println!(
            "{}  {}",
            if folder.custom { "custom " } else { "default" },
            folder.path
        );
    }
    Ok(())
}

async fn media_folders_add(path: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::AddMediaFolder {
            path: path.to_string(),
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::MediaChanged)).await?;
    println!("Folder added; a rescan was queued.");
    Ok(())
}

async fn media_folders_remove(path: &str) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::RemoveMediaFolder {
            path: path.to_string(),
        })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::MediaChanged)).await?;
    println!("Folder removed.");
    Ok(())
}

// ---------------------------------------------------------------------------
// Launcher & tools (M7): local spawn/UDP only — no login required. Companion
// settings persist in `vrc.db` like the desktop app's.
// ---------------------------------------------------------------------------

/// Wait for startup hydration (`AppEvent::Ready`) so persisted tools
/// settings are visible in the snapshot.
async fn wait_ready(handle: &mut AppHandle) -> Result<()> {
    wait_for(handle, |e| matches!(e, AppEvent::Ready)).await?;
    Ok(())
}

/// Bundled `launch` flag values (keeps the handler under clippy's
/// argument-count limit).
struct LaunchCmd {
    path: Option<String>,
    no_vr: bool,
    profile: Option<u32>,
    fps: Option<u32>,
    world: Option<String>,
    instance: Option<String>,
    exe: Option<String>,
    exe_args: Vec<String>,
}

async fn launch(cmd: LaunchCmd) -> Result<()> {
    let LaunchCmd {
        path,
        no_vr,
        profile,
        fps,
        world,
        instance,
        exe,
        exe_args,
    } = cmd;
    let mut handle = App::spawn(cli_config(build_local_client()?));
    if let Some(exe) = exe {
        handle
            .commands
            .send(Command::LaunchPath {
                path: exe,
                args: exe_args,
            })
            .await?;
        wait_for(&mut handle, |e| {
            matches!(e, AppEvent::ProcessLaunched | AppEvent::Error(_))
        })
        .await?;
        println!("Launched.");
        return Ok(());
    }
    // Any explicit option becomes the persisted launch prefs (same store
    // the desktop Launcher tab uses).
    if path.is_some()
        || no_vr
        || profile.is_some()
        || fps.is_some()
        || world.is_some()
        || instance.is_some()
    {
        handle
            .commands
            .send(Command::SetLaunchPrefs {
                prefs: LaunchPrefs {
                    no_vr,
                    profile: profile.unwrap_or(0),
                    fps: fps.unwrap_or(0),
                    world_id: world.unwrap_or_default(),
                    instance_id: instance.unwrap_or_default(),
                    ..LaunchPrefs::default()
                },
            })
            .await?;
    }
    if let Some(path) = path {
        handle
            .commands
            .send(Command::SetVrchatPath { path: Some(path) })
            .await?;
    }
    handle.commands.send(Command::LaunchVrchat).await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::ProcessLaunched | AppEvent::Error(_))
    })
    .await?;
    let state = handle.state.snapshot();
    println!(
        "Launched: {}",
        state
            .tools
            .last_launch
            .unwrap_or_else(|| "VRChat".to_string())
    );
    Ok(())
}

async fn companions_list() -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    wait_ready(&mut handle).await?;
    let companions = handle.state.snapshot().tools.companions;
    if companions.is_empty() {
        println!("(no companions configured)");
        return Ok(());
    }
    for (index, c) in companions.iter().enumerate() {
        println!("{index}: {} — {} {}", c.name, c.path, c.args.join(" "));
    }
    Ok(())
}

async fn companions_add(name: &str, path: &str, args: Vec<String>) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::AddCompanion {
            companion: vrc_core::Companion {
                name: name.to_string(),
                path: path.to_string(),
                args,
            },
        })
        .await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::ToolsChanged | AppEvent::Error(_))
    })
    .await?;
    println!("Companion added.");
    Ok(())
}

async fn companions_remove(index: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::RemoveCompanion { index })
        .await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::ToolsChanged | AppEvent::Error(_))
    })
    .await?;
    println!("Companion removed.");
    Ok(())
}

async fn companions_launch(index: usize) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::LaunchCompanion { index })
        .await?;
    wait_for(&mut handle, |e| {
        matches!(e, AppEvent::ProcessLaunched | AppEvent::Error(_))
    })
    .await?;
    println!("Launched.");
    Ok(())
}

async fn chatbox(text: Vec<String>, immediate: bool, typing: Option<bool>) -> Result<()> {
    let handle = App::spawn(cli_config(build_local_client()?));
    if let Some(typing) = typing {
        handle
            .commands
            .send(Command::SendChatboxTyping { typing })
            .await?;
        // UDP send is fire-and-forget; give the socket a moment.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        println!("Typing indicator {}.", if typing { "on" } else { "off" });
        return Ok(());
    }
    let text = text.join(" ");
    if text.trim().is_empty() {
        bail!("chatbox text is empty — pass a message or --typing");
    }
    handle
        .commands
        .send(Command::SendChatbox { text, immediate })
        .await?;
    // The relay enforces ≥1.5 s between sends; wait long enough for the
    // drain task to flush before the runtime shuts down.
    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
    println!("Chatbox message sent.");
    Ok(())
}

/// Display form for a parameter value (no `Debug` noise).
fn fmt_param(value: ParamValue) -> String {
    match value {
        ParamValue::Float(v) => format!("{v}"),
        ParamValue::Int(v) => v.to_string(),
        ParamValue::Bool(v) => v.to_string(),
    }
}

async fn osc_send(address: &str, value: &str) -> Result<()> {
    let value = parse_param_value(value)
        .with_context(|| format!("invalid value {value:?} — use a number or true/false"))?;
    let handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::SendOscParam {
            address: address.to_string(),
            value,
        })
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    println!("Sent {address} = {}", fmt_param(value));
    Ok(())
}

async fn osc_listen(port: u16) -> Result<()> {
    let mut handle = App::spawn(cli_config(build_local_client()?));
    handle
        .commands
        .send(Command::StartOscListen { port })
        .await?;
    wait_for(&mut handle, |e| matches!(e, AppEvent::ToolsChanged)).await?;
    let bound = handle.state.snapshot().tools.osc.listen_port;
    println!("Listening for OSC on 127.0.0.1:{bound} (Ctrl-C to stop)");
    let mut seen: HashMap<String, String> = HashMap::new();
    loop {
        tokio::select! {
            event = handle.events.recv() => match event {
                Ok(AppEvent::ToolsChanged) => {
                    for (addr, value) in &handle.state.snapshot().tools.osc.params {
                        let text = fmt_param(*value);
                        if seen.get(addr) != Some(&text) {
                            seen.insert(addr.clone(), text.clone());
                            println!("{addr} = {text}");
                        }
                    }
                }
                Ok(AppEvent::Error(m)) => eprintln!("error: {m}"),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    let _ = handle.commands.send(Command::StopOscListen).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Events calendar (M7): documented /calendar* endpoints — login required.
// ---------------------------------------------------------------------------

fn print_calendar_event(event: &vrc_core::CalendarEvent) {
    let following = event.user_interest.as_ref().is_some_and(|i| i.is_following);
    println!(
        "{:<44} {:<22} {:<14}{}",
        event.title,
        event.starts_at,
        event.category,
        if following { " ★" } else { "" }
    );
    println!(
        "    {}  ({} interested)",
        event.id, event.interested_user_count
    );
}

async fn calendar(command: CalendarCommand) -> Result<()> {
    let mut handle = App::spawn(cli_config(logged_in_client()?));
    ensure_session(&mut handle).await?;
    match command {
        CalendarCommand::Month { date, offset } => {
            handle
                .commands
                .send(Command::LoadCalendarMonth { date, offset })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            let state = handle.state.snapshot();
            let Some((key, page)) = &state.calendar.month else {
                bail!("no calendar data");
            };
            println!(
                "== calendar {} ==",
                if key.is_empty() { "(current)" } else { key }
            );
            for event in &page.results {
                print_calendar_event(event);
            }
            println!(
                "-- {} event(s), total {} --",
                page.results.len(),
                page.total_count
            );
        }
        CalendarCommand::Discover { scope, cursor } => {
            handle
                .commands
                .send(Command::LoadCalendarDiscover {
                    scope: scope.into(),
                    cursor,
                })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            let state = handle.state.snapshot();
            let Some(d) = &state.calendar.discover else {
                bail!("no discovery results");
            };
            for event in &d.events {
                print_calendar_event(event);
            }
            println!(
                "-- {} event(s){} --",
                d.events.len(),
                if d.next_cursor.is_some() {
                    ", more available (--cursor)"
                } else {
                    ""
                }
            );
        }
        CalendarCommand::Search { query, offset } => {
            handle
                .commands
                .send(Command::LoadCalendarSearch { query, offset })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            let state = handle.state.snapshot();
            let Some((_, page)) = &state.calendar.search else {
                bail!("no search results");
            };
            for event in &page.results {
                print_calendar_event(event);
            }
            println!(
                "-- {} event(s), total {} --",
                page.results.len(),
                page.total_count
            );
        }
        CalendarCommand::Group { group_id, date } => {
            handle
                .commands
                .send(Command::LoadGroupCalendar {
                    group_id,
                    date,
                    offset: 0,
                })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            let state = handle.state.snapshot();
            let Some((_, page)) = &state.calendar.group else {
                bail!("no group calendar data");
            };
            for event in &page.results {
                print_calendar_event(event);
            }
            println!(
                "-- {} event(s), total {} --",
                page.results.len(),
                page.total_count
            );
        }
        CalendarCommand::Next { group_id } => {
            handle
                .commands
                .send(Command::LoadGroupNextEvent { group_id })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            let state = handle.state.snapshot();
            match state.calendar.next_events.values().next() {
                Some(Some(event)) => {
                    println!("Next event:");
                    print_calendar_event(event);
                }
                Some(None) => println!("No upcoming events for that group."),
                None => bail!("no result"),
            }
        }
        CalendarCommand::Follow {
            group_id,
            calendar_id,
            unfollow,
        } => {
            handle
                .commands
                .send(Command::FollowCalendarEvent {
                    group_id,
                    calendar_id,
                    is_following: !unfollow,
                })
                .await?;
            wait_for(&mut handle, |e| matches!(e, AppEvent::CalendarChanged)).await?;
            println!("{}.", if unfollow { "Unfollowed" } else { "Following" });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vrc_core::MemoryVault;

    #[test]
    fn empty_vault_has_no_session_to_resume() {
        assert!(!vault_has_session(&MemoryVault::new()).unwrap());
    }

    #[test]
    fn fmt_bytes_scales() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(2048), "2.0 KiB");
        assert_eq!(fmt_bytes(5 * 1024 * 1024), "5.0 MiB");
    }
}
