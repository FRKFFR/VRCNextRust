//! The application core: command loop, pipeline forwarding, state updates.
//!
//! [`App::spawn`] starts a dedicated Tokio runtime on its own thread; front
//! ends send [`Command`]s over an `mpsc` channel, subscribe to [`AppEvent`]s
//! via `broadcast`, and read [`AppState`] snapshots from [`StateHandle`].
//! After authentication the core fetches friends + favorites *once* and then
//! maintains `AppState.friends` incrementally from pipeline events
//! (RULES.md §3). Secrets live only as `SecretString` inside commands and
//! are never logged (RULES.md §4).

use std::sync::{Arc, RwLock, RwLockWriteGuard};

use futures_util::StreamExt;
use secrecy::SecretString;
use tokio::sync::{broadcast, mpsc};
use vrc_api::{ApiError, Client, CurrentUser, LoginOutcome, TwoFactorMethod};
use vrc_pipeline::{Pipeline, PipelineConfig, PipelineEvent, PipelineHandle};

use crate::state::{AppState, AuthState, Friend, PipelineStatus};

/// Static configuration for [`App::spawn`].
#[derive(Debug)]
pub struct AppConfig {
    /// The shared rate-limited REST client (with vault, base url).
    pub client: Client,
    /// Pipeline connection tuning; override `url` in tests.
    pub pipeline: PipelineConfig,
}

impl AppConfig {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            pipeline: PipelineConfig::default(),
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
#[derive(Debug)]
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
    /// A command failed (message carries no secrets).
    Error(String),
}

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
}

async fn run(
    config: AppConfig,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<AppEvent>,
    state: Arc<RwLock<AppState>>,
) {
    let mut rt = Runtime {
        commands,
        events,
        state: StateHandle { inner: state },
        client: config.client,
        pipeline_config: config.pipeline,
        pipeline: None,
        pipeline_events: mpsc::unbounded_channel(),
    };
    loop {
        tokio::select! {
            cmd = rt.commands.recv() => match cmd {
                Some(command) => rt.handle(command).await,
                None => break,
            },
            ev = rt.pipeline_events.1.recv() => {
                if let Some(event) = ev {
                    rt.apply_pipeline_event(&event);
                    let _ = rt.events.send(AppEvent::Pipeline(Box::new(event)));
                }
            }
        }
    }
    if let Some(handle) = rt.pipeline.take() {
        handle.shutdown();
    }
}

impl Runtime {
    async fn handle(&mut self, command: Command) {
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
                Err(e) => self.report(e),
            },
            Command::RefreshFriends => {
                if let Err(e) = self.refresh_friends().await {
                    self.report(e);
                }
            }
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
            self.report(e);
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
            let mut friend = Friend::from_limited(user, false);
            friend.is_favorite = favorites.contains(&friend.id);
            state.friends.insert(friend.id.clone(), friend);
        }
        for user in &online {
            let mut friend = Friend::from_limited(user, true);
            friend.is_favorite = favorites.contains(&friend.id);
            state.friends.insert(friend.id.clone(), friend);
        }
        drop(state);
        let _ = self.events.send(AppEvent::FriendsRefreshed);
        Ok(())
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
            | PipelineEvent::FriendActive { user_id, user }
            | PipelineEvent::FriendAdd { user_id, user } => {
                let key = if user_id.is_empty() {
                    user.id.clone()
                } else {
                    user_id.clone()
                };
                let friend = state
                    .friends
                    .entry(key)
                    .or_insert_with(|| Friend::from_user(user));
                friend.apply_user(user);
                friend.is_online = true;
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
                friend.location = location.clone();
                friend.world_id = if world_id.is_empty() {
                    crate::state::world_id_from(location)
                } else {
                    world_id.clone()
                };
                friend.is_online = !location.is_empty() && location != "offline";
                if let Some(user) = user {
                    friend.apply_user(user);
                }
            }
            PipelineEvent::FriendUpdate { user_id, user } => {
                let key = if user_id.is_empty() {
                    user.id.clone()
                } else {
                    user_id.clone()
                };
                let friend = state.friends.entry(key).or_default();
                friend.apply_user(user);
                if user.state == "offline" {
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

    fn set_auth(&mut self, auth: AuthState) {
        self.state.write().auth = auth.clone();
        let _ = self.events.send(AppEvent::AuthChanged(auth));
    }

    /// Clear session state on logout/401.
    fn teardown_session(&mut self) {
        if let Some(handle) = self.pipeline.take() {
            handle.shutdown();
        }
        let mut state = self.state.write();
        state.current_user = None;
        state.friends.clear();
        state.pipeline = PipelineStatus::Disconnected;
    }

    fn report(&mut self, error: ApiError) {
        let message = error.to_string();
        tracing::debug!("vrc-core command failed: {message}");
        self.state.write().last_error = Some(message.clone());
        let _ = self.events.send(AppEvent::Error(message));
    }
}
