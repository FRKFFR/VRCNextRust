# VRCNextRust architecture

VRChat launcher and companion app in pure Rust. Windows first, Linux later.
Rules: see `RULES.md` (mandatory).

## Workspace layout

```
VRCNextRust/
├─ Cargo.toml              # [workspace] members = ["crates/*"], shared [workspace.dependencies]
├─ rust-toolchain.toml     # stable, components rustfmt + clippy
├─ .github/workflows/ci.yml# fmt --check, clippy -D warnings, test (windows-latest, ubuntu-latest)
├─ RULES.md  STATUS.md  docs/architecture.md
├─ tools/agent-orchestrator/
└─ crates/
   ├─ vrc-api/       # REST client, auth + 2FA, rate limiter, models, USER_AGENT
   ├─ vrc-pipeline/  # WebSocket event stream
   ├─ vrc-store/     # SQLite + migrations
   ├─ vrc-logs/      # VRChat log tailer/parser
   ├─ vrc-osc/       # OSC send/receive
   ├─ vrc-launcher/  # start VRChat + companion apps
   ├─ core/          # package name `vrc-core`: state, commands, events, tasks
   ├─ ui-egui/       # egui/eframe (wgpu) front end
   ├─ app/           # binary `vrcnextrust`
   └─ cli/           # binary `vrcnext-cli` (headless)
```

## Dependency graph (arrows = "depends on")

```
app ──► ui-egui ──► vrc-core ──► vrc-api, vrc-pipeline, vrc-store, vrc-logs, vrc-osc, vrc-launcher
cli ──────────────► vrc-core
vrc-pipeline ──► vrc-api (models + USER_AGENT only)
```
Only `ui-egui` and `app` may depend on egui/eframe. Leaf crates never depend
on `vrc-core`.

## Key dependencies
tokio, reqwest (rustls, cookies, json), reqwest_cookie_store, tokio-tungstenite
(rustls), serde/serde_json, thiserror, anyhow (binaries only), tracing +
tracing-subscriber, keyring, rusqlite (bundled) + rusqlite_migration, notify,
rosc, directories, time, eframe/egui. Tests: wiremock, tokio-test, tempfile.

## Public API per crate (target shape; milestones refine it)

### vrc-api
```rust
pub const USER_AGENT: &str; // "VRCNextRust/<ver> (https://github.com/FRKFFR/VRCNextRust)"
pub struct Client;           // Clone, cheap (Arc inside)
impl Client {
    pub fn builder() -> ClientBuilder;            // .base_url() .vault() .rate_limit()
    pub async fn login(&self, username: &str, password: &str) -> Result<LoginOutcome, ApiError>;
    pub async fn verify_2fa(&self, method: TwoFactorMethod, code: &str) -> Result<CurrentUser, ApiError>;
    pub async fn resume_session(&self) -> Result<Option<CurrentUser>, ApiError>; // from vault cookie
    pub async fn current_user(&self) -> Result<CurrentUser, ApiError>;
    pub async fn logout(&self) -> Result<(), ApiError>;
    pub fn auth_token(&self) -> Option<SecretString>; // for vrc-pipeline
    // later milestones: users(), friends(), worlds(), instances(), groups(),
    // avatars(), notifications(), favorites(), invites() sub-APIs
}
pub enum LoginOutcome { LoggedIn(CurrentUser), Needs2fa(Vec<TwoFactorMethod>) }
pub enum TwoFactorMethod { Totp, EmailOtp, RecoveryOtp }
pub trait CookieVault: Send + Sync { fn load(&self)->Result<Option<String>>; fn save(&self,&str)->Result<()>; fn clear(&self)->Result<()>; }
pub struct KeyringVault; pub struct MemoryVault; // MemoryVault for tests
pub struct RateLimiter; // token bucket + global backoff on 429/Retry-After
pub enum ApiError { Http{status,..}, RateLimited{retry_after}, Unauthorized, Requires2fa, Network(..), Decode(..), Vault(..) }
pub mod models; // CurrentUser, LimitedUser, User, World, Instance, Group, Avatar, Notification, TrustRank, ...
```

### vrc-pipeline
```rust
pub struct Pipeline;
impl Pipeline { pub fn connect(cfg: PipelineConfig, token: SecretString) -> (PipelineHandle, impl Stream<Item = PipelineEvent>); }
pub enum PipelineEvent { Connected, Disconnected{reason}, FriendOnline{..}, FriendOffline{..}, FriendActive{..},
  FriendLocation{..}, FriendUpdate{..}, FriendAdd{..}, FriendDelete{..}, Notification(..), UserUpdate(..), Unknown{kind, raw} }
// auto-reconnect with capped exponential backoff + jitter
```

### vrc-store
```rust
pub struct Store; impl Store { pub fn open(path)->Result<Self>; pub fn open_in_memory()->Result<Self>; }
// repositories: timeline(), history(), stats(), cache(), media(), settings()
// cache: get_fresh / get_stale / put with TTL (stale-while-revalidate)
```

### vrc-logs
```rust
pub fn parse_line(line: &str) -> Option<LogEvent>;      // pure, unit-tested
pub struct LogTailer; impl LogTailer { pub fn watch(dir: PathBuf) -> impl Stream<Item = LogEvent>; }
pub enum LogEvent { JoinedWorld{..}, LeftWorld{..}, PlayerJoined{..}, PlayerLeft{..}, PhotoTaken{path,..} }
pub fn default_log_dir() -> Option<PathBuf>;
```

### vrc-osc
```rust
pub struct OscClient; impl OscClient { pub async fn bind(target: SocketAddr)->Result<Self>; pub async fn send(&self, addr:&str, args:Vec<OscType>)->Result<()>; }
pub struct OscListener; impl OscListener { pub async fn bind(port:u16)->Result<(Self, impl Stream<Item=OscMessage>)>; }
pub fn chatbox_input(text:&str, immediate:bool) -> OscMessage;
```

### vrc-launcher
```rust
pub fn find_vrchat() -> Option<PathBuf>;                 // Steam library scan
pub fn launch(opts: &LaunchOptions) -> Result<Child>;   // no process inspection beyond spawn
pub fn launch_companion(path:&Path, args:&[String]) -> Result<Child>;
```

### vrc-core (crate dir `core`)
```rust
pub struct App; impl App { pub fn spawn(cfg: AppConfig) -> AppHandle; }
pub struct AppHandle { pub commands: CommandSender, pub events: broadcast::Receiver<AppEvent>, pub state: StateHandle }
pub enum Command { Login{username, password: SecretString}, Submit2fa{method, code}, Logout, RefreshCurrentUser, ... }
pub enum AppEvent { AuthChanged(AuthState), Error(String), ... }
pub struct StateHandle; // Arc<RwLock<AppState>> snapshot reads for the UI
```
Every user action is a `Command`; handlers are tested headlessly with a
wiremock-backed `vrc_api::Client`.

### ui-egui / app / cli
- `ui_egui::run(handle: AppHandle) -> eframe::Result<()>` — reads `StateHandle`, sends `Command`s, nothing else.
- `app`: init tracing, `App::spawn`, `ui_egui::run`.
- `cli`: clap subcommands (`login`, `me`, `logout`, later `friends`, `tail-logs`, ...) driving `vrc-core`.

## Milestones
- M1 Foundations: workspace, CI, vrc-api (login, 2FA, cookie vault, rate limiter, current user), minimal core/cli/app.
- M2 Real-time: pipeline, friend sections (Favorites, In-Game, Web/Active, Offline), live search, trust badges, friends' worlds dashboard.
- M3 Local data: SQLite store, log parser, personal + friends timeline, time-spent stats.
- M4 Social actions: notifications accept/deny, invites, throttled multi-invite, notes, friend add/remove, block/mute, instance players, profile view.
- M5 Worlds, groups, avatars: search, instances, create instance (incl. group), favorites, group posts/galleries, avatar browse/switch.
- M6 Media: photo library + custom folders, favorites, blur-until-hover, metadata, infinite scroll, Discord webhook relay (≤4).
- M7 Launcher and tools: launch VRChat + companions, chatbox relay, OSC parameter tool, events calendar.
- M8 (optional): inventory, offline voice soundboard, theming.
