//! M7 launcher & tools: Steam launch options, companion apps, OSC
//! chatbox relay and avatar-parameter state.
//!
//! Everything here is either pure (arg building, settings (de)serialization,
//! chatbox sanitizing via `vrc-osc`) or thin wrappers over `vrc-launcher` /
//! `vrc-osc`; all process work is spawn-only (RULES.md §5) and every user
//! action still arrives as a [`crate::Command`].

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use vrc_api::{valid_instance_id, valid_world_id};
use vrc_launcher::{self, LaunchOptions, Location, OscEndpoints};
pub use vrc_osc::chatbox::CHATBOX_MAX_CHARS;
use vrc_osc::params::{param_value_of as osc_param_value_of, valid_param_address, ParamValue};
use vrc_osc::{chatbox, OscClient, OscMessage};

/// Settings keys persisted in the `settings` table.
pub const SETTING_COMPANIONS: &str = "tools.companions";
pub const SETTING_VRCHAT_PATH: &str = "tools.vrchat_path";
pub const SETTING_AUTO_START: &str = "tools.auto_start_companion";
pub const SETTING_LAUNCH: &str = "tools.launch_prefs";

/// Minimum interval between chatbox sends (VRChat's own throttle).
pub const CHATBOX_INTERVAL: Duration = Duration::from_millis(1500);

/// A user-configured companion application.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Companion {
    /// Display name.
    pub name: String,
    /// Executable path.
    pub path: String,
    /// Arguments passed verbatim (no shell).
    #[serde(default)]
    pub args: Vec<String>,
}

/// Persisted launch preferences (the documented `--no-vr`/`--profile`/
/// `--fps`/`--osc`/`--midi` options plus a join location).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaunchPrefs {
    /// `--no-vr`: start in desktop mode.
    #[serde(default)]
    pub no_vr: bool,
    /// `--profile=N` (0 = default profile).
    #[serde(default)]
    pub profile: u32,
    /// `--fps=N` (0 = client default).
    #[serde(default)]
    pub fps: u32,
    /// `--osc=inPort:senderIP:outPort` ports (input 0 = default 9000,
    /// output 0 = default 9001).
    #[serde(default)]
    pub osc_input_port: u16,
    #[serde(default)]
    pub osc_output_port: u16,
    /// `vrchat://` join target — `wrld_…` world plus optional instance.
    #[serde(default)]
    pub world_id: String,
    #[serde(default)]
    pub instance_id: String,
}

/// OSC tools state shown in the UI.
#[derive(Debug, Clone, Default)]
pub struct OscUi {
    /// Whether the OSC listener task is running.
    pub listening: bool,
    /// Port the listener is bound to (0 until bound / when stopped).
    pub listen_port: u16,
    /// Latest value per `/avatar/parameters/*` address received from
    /// VRChat (or sent by us).
    pub params: HashMap<String, ParamValue>,
}

/// Launcher & tools state shown in the UI.
#[derive(Debug, Clone, Default)]
pub struct ToolsUi {
    /// User override for the VRChat install dir; `None`/empty = auto-detect
    /// via Steam (`vrc_launcher::find_vrchat`).
    pub vrchat_path: Option<String>,
    /// Result of the last `find_vrchat` scan (resolved install dir).
    pub vrchat_found: Option<String>,
    /// Launch preferences (persisted).
    pub launch: LaunchPrefs,
    /// Configured companions (persisted).
    pub companions: Vec<Companion>,
    /// Launch every companion together with VRChat.
    pub auto_start_companion: bool,
    /// OSC listener/parameter state.
    pub osc: OscUi,
    /// Last successful launch description for the UI status line.
    pub last_launch: Option<String>,
}

/// Parse the persisted companions JSON array.
pub fn parse_companions(json: Option<&str>) -> Vec<Companion> {
    let Some(raw) = json else { return Vec::new() };
    serde_json::from_str::<Vec<Companion>>(raw).unwrap_or_default()
}

/// Serialize companions for [`SETTING_COMPANIONS`].
pub fn companions_json(companions: &[Companion]) -> String {
    serde_json::to_string(companions).unwrap_or_else(|_| "[]".to_string())
}

/// Parse the persisted launch prefs JSON.
pub fn parse_launch_prefs(json: Option<&str>) -> LaunchPrefs {
    json.and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default()
}

/// Serialize launch prefs for [`SETTING_LAUNCH`].
pub fn launch_prefs_json(prefs: &LaunchPrefs) -> String {
    serde_json::to_string(prefs).unwrap_or_else(|_| "{}".to_string())
}

/// Resolve the install dir: explicit override wins (it must look like an
/// install dir), otherwise the Steam scan.
pub fn resolve_install_dir(overridden: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(path) = overridden.map(str::trim).filter(|p| !p.is_empty()) {
        let dir = std::path::PathBuf::from(path);
        if dir.join("VRChat.exe").is_file() {
            return Some(dir);
        }
        // Invalid override (e.g. hand-edited settings) — fall through to
        // the Steam scan rather than reporting "not found".
    }
    vrc_launcher::find_vrchat()
}

/// The OSC input port chatbox/parameter traffic targets: the persisted
/// launch pref, or VRChat's default 9000 when unset.
pub fn osc_input_port(prefs: &LaunchPrefs) -> u16 {
    if prefs.osc_input_port > 0 {
        prefs.osc_input_port
    } else {
        vrc_osc::DEFAULT_INPUT_PORT
    }
}

/// Where OSC messages are sent for these prefs (`127.0.0.1:<in-port>`).
pub fn osc_send_target(prefs: &LaunchPrefs) -> SocketAddr {
    vrc_osc::loopback(osc_input_port(prefs))
}

/// Build [`LaunchOptions`] from UI prefs; validates the join location with
/// the shared `vrc-api` id validators before the URI is ever built.
pub fn launch_options(prefs: &LaunchPrefs) -> Result<LaunchOptions, String> {
    let mut opts = LaunchOptions {
        no_vr: prefs.no_vr,
        profile: (prefs.profile > 0).then_some(prefs.profile),
        fps: (prefs.fps > 0).then_some(prefs.fps),
        ..Default::default()
    };
    if prefs.osc_input_port > 0 || prefs.osc_output_port > 0 {
        opts.osc = Some(OscEndpoints {
            input_port: osc_input_port(prefs),
            sender_ip: vrc_osc::LOOPBACK,
            output_port: if prefs.osc_output_port > 0 {
                prefs.osc_output_port
            } else {
                vrc_osc::DEFAULT_OUTPUT_PORT
            },
        });
    }
    let world = prefs.world_id.trim();
    if !world.is_empty() {
        let instance = prefs.instance_id.trim();
        if !valid_world_id(world) {
            return Err("invalid world id — expected wrld_…".to_string());
        }
        if !instance.is_empty() && !valid_instance_id(instance) {
            return Err("invalid instance id".to_string());
        }
        opts.location = Some(Location {
            world_id: world.to_string(),
            instance_id: instance.to_string(),
        });
    }
    Ok(opts)
}

/// Spawn VRChat plus (optionally) every companion. Returns a human-readable
/// summary for `ToolsUi::last_launch`.
pub fn launch_vrchat(
    install_dir: &std::path::Path,
    prefs: &LaunchPrefs,
    companions: &[Companion],
    auto_start: bool,
) -> Result<String, String> {
    let opts = launch_options(prefs)?;
    vrc_launcher::launch(install_dir, &opts).map_err(|e| e.to_string())?;
    let mut launched = vec!["VRChat".to_string()];
    if auto_start {
        for companion in companions {
            match vrc_launcher::launch_companion(
                std::path::Path::new(&companion.path),
                &companion.args,
            ) {
                Ok(_) => launched.push(companion.name.clone()),
                Err(e) => tracing::warn!("companion {} failed: {e}", companion.name),
            }
        }
    }
    Ok(launched.join(", "))
}

/// Spawn one configured companion by index.
pub fn launch_companion_at(companions: &[Companion], index: usize) -> Result<String, String> {
    let companion = companions
        .get(index)
        .ok_or_else(|| "no companion at that index".to_string())?;
    vrc_launcher::launch_companion(std::path::Path::new(&companion.path), &companion.args)
        .map_err(|e| e.to_string())?;
    Ok(companion.name.clone())
}

/// Spawn an ad-hoc executable (CLI `launch -- <path> [args]`).
pub fn launch_path(path: &str, args: &[String]) -> Result<(), String> {
    vrc_launcher::launch_companion(std::path::Path::new(path), args).map_err(|e| e.to_string())?;
    Ok(())
}

/// Validate a chatbox payload and return the OSC message to send.
/// Rejects text that sanitizes to nothing (whitespace- or
/// control-char-only input would send an empty chatbox line).
pub fn chatbox_message(text: &str, immediate: bool) -> Result<OscMessage, String> {
    if text.trim().is_empty() || chatbox::sanitize_chatbox(text).is_empty() {
        return Err("chatbox text is empty".to_string());
    }
    Ok(chatbox::chatbox_input(text, immediate))
}

/// Validate an avatar-parameter send; returns the OSC message.
pub fn param_message(addr: &str, value: ParamValue) -> Result<OscMessage, String> {
    vrc_osc::params::param_message(addr, value)
        .ok_or_else(|| "invalid avatar parameter address".to_string())
}

/// `true` for a sendable `/avatar/parameters/…` address (used by the UI to
/// grey out invalid input before the command is even sent).
pub fn valid_param_addr(addr: &str) -> bool {
    valid_param_address(addr)
}

/// Parse a user-entered parameter value: `true`/`false` → bool, integer →
/// int, otherwise float. `None` when unparseable.
pub fn parse_param_value(text: &str) -> Option<ParamValue> {
    let text = text.trim();
    if text.eq_ignore_ascii_case("true") {
        return Some(ParamValue::Bool(true));
    }
    if text.eq_ignore_ascii_case("false") {
        return Some(ParamValue::Bool(false));
    }
    if let Ok(v) = text.parse::<i32>() {
        return Some(ParamValue::Int(v));
    }
    text.parse::<f32>().ok().map(ParamValue::Float)
}

/// Extract the latest parameter value from a received OSC message:
/// `/avatar/parameters/<name>` with a single float/int/bool arg.
pub fn param_value_of(message: &OscMessage) -> Option<(String, ParamValue)> {
    osc_param_value_of(message)
}

/// Shared chatbox relay state: at most one send per [`CHATBOX_INTERVAL`],
/// latest message wins (bounded — never an unbounded queue). `draining`
/// lives inside the mutex so `send` and the drain task can never race a
/// message into a stranded slot. `target` is retargetable so launch-pref
/// OSC port changes apply without rebuilding the relay.
struct RelayState {
    last_sent: Option<Instant>,
    pending: Option<OscMessage>,
    draining: bool,
    target: SocketAddr,
}

impl Default for RelayState {
    fn default() -> Self {
        Self {
            last_sent: None,
            pending: None,
            draining: false,
            target: vrc_osc::default_target(),
        }
    }
}

/// Throttled chatbox sender delivering to a configurable target (VRChat's
/// OSC input port — [`osc_send_target`] of the launch prefs).
/// [`Self::send`] is sync and cheap; the actual UDP write happens on the
/// spawned drain task, which binds its own [`OscClient`].
#[derive(Clone)]
pub struct ChatboxRelay {
    inner: Arc<Mutex<RelayState>>,
}

impl std::fmt::Debug for ChatboxRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let target = self.inner.lock().unwrap_or_else(|e| e.into_inner()).target;
        f.debug_struct("ChatboxRelay")
            .field("target", &target)
            .finish_non_exhaustive()
    }
}

impl ChatboxRelay {
    /// Create a relay that delivers to `target` (`127.0.0.1:9000` for the
    /// default VRChat OSC endpoint).
    pub fn new(target: SocketAddr) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RelayState {
                target,
                ..RelayState::default()
            })),
        }
    }

    /// Point future sends at `target` (called when launch prefs change
    /// the OSC input port). An in-flight drain keeps its bound client
    /// until it goes idle — at most one interval of lag.
    pub fn set_target(&self, target: SocketAddr) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).target = target;
    }

    /// Queue `message` for throttled delivery. If the interval has already
    /// passed the message still goes through the drain task (which sends
    /// it immediately); otherwise it replaces any pending message —
    /// latest wins, the queue never grows past one slot.
    pub fn send(&self, message: OscMessage) {
        let target = {
            let mut state = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            state.pending = Some(message);
            if state.draining {
                return;
            }
            state.draining = true;
            state.target
        };
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let client = match OscClient::bind(target).await {
                Ok(client) => client,
                Err(e) => {
                    tracing::debug!("chatbox relay bind failed: {e}");
                    inner.lock().unwrap_or_else(|e| e.into_inner()).draining = false;
                    return;
                }
            };
            loop {
                // Wait out the remainder of the send interval.
                let wait = {
                    let state = inner.lock().unwrap_or_else(|e| e.into_inner());
                    match state.last_sent {
                        Some(last) => CHATBOX_INTERVAL.saturating_sub(last.elapsed()),
                        None => Duration::ZERO,
                    }
                };
                if !wait.is_zero() {
                    tokio::time::sleep(wait).await;
                }
                let message = {
                    let mut state = inner.lock().unwrap_or_else(|e| e.into_inner());
                    match state.pending.take() {
                        Some(message) => message,
                        // Nothing queued: go idle. `draining` is cleared
                        // under the same lock `send` uses, so a message
                        // arriving now either lands in `pending` before
                        // this lock (and is taken above) or sees
                        // `draining == false` and spawns a fresh drain —
                        // never stranded.
                        None => {
                            state.draining = false;
                            break;
                        }
                    }
                };
                match client.send(&message).await {
                    Ok(()) => {
                        inner.lock().unwrap_or_else(|e| e.into_inner()).last_sent =
                            Some(Instant::now());
                    }
                    Err(e) => {
                        tracing::debug!("chatbox send failed: {e}");
                        // Requeue only when nothing newer arrived, then
                        // wait out a full interval before retrying —
                        // `last_sent` isn't updated on failure, so
                        // without this a persistent send error would spin.
                        {
                            let mut state = inner.lock().unwrap_or_else(|e| e.into_inner());
                            if state.pending.is_none() {
                                state.pending = Some(message);
                            }
                        }
                        tokio::time::sleep(CHATBOX_INTERVAL).await;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companions_json_roundtrip() {
        let list = vec![
            Companion {
                name: "Companion A".to_string(),
                path: r"C:\Tools\a.exe".to_string(),
                args: vec!["--flag".to_string()],
            },
            Companion::default(),
        ];
        let json = companions_json(&list);
        assert_eq!(parse_companions(Some(&json)), list);
        assert!(parse_companions(None).is_empty());
        assert!(parse_companions(Some("not json")).is_empty());
    }

    #[test]
    fn launch_prefs_roundtrip() {
        let prefs = LaunchPrefs {
            no_vr: true,
            profile: 3,
            fps: 90,
            osc_input_port: 9000,
            osc_output_port: 9001,
            world_id: "wrld_00000000-0000-0000-0000-000000000001".to_string(),
            instance_id: "12345~region(us)".to_string(),
        };
        let json = launch_prefs_json(&prefs);
        assert_eq!(parse_launch_prefs(Some(&json)), prefs);
        assert_eq!(parse_launch_prefs(None), LaunchPrefs::default());
    }

    #[test]
    fn launch_options_validates_location() {
        let prefs = LaunchPrefs {
            world_id: "wrld_00000000-0000-0000-0000-000000000001".to_string(),
            instance_id: "bad/id".to_string(),
            ..LaunchPrefs::default()
        };
        assert!(launch_options(&prefs).is_err());

        let prefs = LaunchPrefs {
            instance_id: "12345~region(us)".to_string(),
            ..prefs
        };
        let opts = launch_options(&prefs).unwrap();
        assert!(opts.location.is_some());
        assert!(vrc_launcher::launch_args(&opts).unwrap()[0].starts_with("vrchat://"));
    }

    #[test]
    fn chatbox_message_rejects_empty() {
        assert!(chatbox_message("   ", true).is_err());
        // Control-char-only input sanitizes to an empty OSC string.
        assert!(chatbox_message("\u{7}\n\t", true).is_err());
        assert!(chatbox_message("hi", true).is_ok());
    }

    #[test]
    fn param_value_of_extracts_latest() {
        let msg =
            vrc_osc::params::param_message("/avatar/parameters/Speed", ParamValue::Float(0.5))
                .unwrap();
        let (addr, value) = param_value_of(&msg).unwrap();
        assert_eq!(addr, "/avatar/parameters/Speed");
        assert_eq!(value, ParamValue::Float(0.5));

        let bad = OscMessage {
            addr: "/chatbox/input".to_string(),
            args: vec![],
        };
        assert!(param_value_of(&bad).is_none());
    }
}
