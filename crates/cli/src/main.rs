//! `vrcnext-cli` binary: headless clap CLI driving `vrc-core`.
//!
//! Auth cookies persist only in the OS credential store via
//! `vrc_api::KeyringVault` (RULES.md §4). Passwords and 2FA codes are read
//! with `rpassword` and held in memory as `SecretString`s for the duration
//! of the login call only.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use secrecy::SecretString;
use time::{Duration, OffsetDateTime};
use tokio::sync::broadcast;

use vrc_core::{
    format, sections, App, AppConfig, AppEvent, AppHandle, AuthState, Client, Command,
    KeyringVault, MemoryVault, StatsRange, TimelineFilter, TimelineKind, TwoFactorMethod,
    USER_AGENT,
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
    let mut handle = App::spawn(AppConfig::new(build_client()?));

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
    let mut handle = App::spawn(AppConfig::new(build_client()?));
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
    let mut handle = App::spawn(AppConfig::new(build_client()?));
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
    let mut handle = App::spawn(AppConfig::new(build_client()?));
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
    let mut handle = App::spawn(AppConfig::new(build_client()?));
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
    let mut handle = App::spawn(AppConfig::new(build_local_client()?));
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
    let mut handle = App::spawn(AppConfig::new(build_local_client()?));
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
