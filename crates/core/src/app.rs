//! The application core: command loop, pipeline forwarding, state updates.
//!
//! [`App::spawn`] starts a dedicated Tokio runtime on its own thread; front
//! ends send [`Command`]s over an `mpsc` channel, subscribe to [`AppEvent`]s
//! via `broadcast`, and read [`AppState`] snapshots from [`StateHandle`].
//! After authentication the core fetches friends + favorites *once* and then
//! maintains `AppState.friends` incrementally from pipeline events
//! (RULES.md §3). Secrets live only as `SecretString` inside commands and
//! are never logged (RULES.md §4).

use std::path::PathBuf;
use std::sync::{Arc, RwLock, RwLockWriteGuard};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use secrecy::SecretString;
use time::OffsetDateTime;
use tokio::sync::{broadcast, mpsc, oneshot};
use vrc_api::{ApiError, Client, CurrentUser, LoginOutcome, TwoFactorMethod};
use vrc_logs::{LogEvent, LogTailer};
use vrc_pipeline::{Pipeline, PipelineConfig, PipelineEvent, PipelineHandle};
use vrc_store::{FriendHistoryFilter, StatsRange, Store, TimelineFilter};

use crate::local::{self, StoreOp};
use crate::sections::worlds_with_friends;
use crate::state::{valid_world_id, AppState, AuthState, Friend, PipelineStatus, WorldName};

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

    fn read(&self) -> std::sync::RwLockReadGuard<'_, AppState> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, AppState> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
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
        std::thread::Builder::new()
            .name("vrc-core".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("vrc-core tokio runtime");
                runtime.block_on(run(config, command_rx, event_tx, inner_state));
            })
            .expect("spawn vrc-core thread");
        AppHandle {
            commands,
            events,
            state,
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
}

async fn run(
    config: AppConfig,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<AppEvent>,
    state: Arc<RwLock<AppState>>,
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
    };
    rt.state.write().log_tail_active = rt.log_events.is_some();
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
        }
    }

    /// Run a timeline query on the store thread and publish the page.
    fn load_timeline(&self, page: u32, mut filter: TimelineFilter) {
        let Some(tx) = self.store_tx.clone() else {
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
            // Notifications, user updates and unknown frames don't alter the
            // friends map; subscribers still see them via AppEvent::Pipeline.
            PipelineEvent::Notification(_)
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
        let mut state = self.state.write();
        state.current_user = None;
        state.friends.clear();
        state.pipeline = PipelineStatus::Disconnected;
        state.world_names.clear();
    }

    fn report(&mut self, error: ApiError) {
        let message = error.to_string();
        tracing::debug!("vrc-core command failed: {message}");
        self.state.write().last_error = Some(message.clone());
        let _ = self.events.send(AppEvent::Error(message));
    }
}
