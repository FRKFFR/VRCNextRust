//! Launch VRChat and companion applications (M7).
//!
//! Spawn-only: everything here builds `std::process::Command` argument
//! vectors and calls `spawn` — never a shell, never process inspection,
//! memory access, or file modification (RULES.md §5).
//!
//! * [`find_vrchat`] locates the install dir via Steam library metadata.
//! * [`LaunchOptions`] + [`launch_args`] model the documented launch
//!   options (`--no-vr`, `--profile=N`, `--fps=N`, `--osc=…`, `--midi=…`,
//!   extra args, and a `vrchat://launch?id=` join link).
//! * [`launch`] / [`launch_companion`] spawn processes.

mod steam;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

pub use steam::find_vrchat;

/// Launcher failures.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// VRChat install could not be located.
    #[error("VRChat installation not found")]
    NotFound,
    /// The install dir does not contain `VRChat.exe`.
    #[error("not a VRChat install directory: {0}")]
    NotInstallDir(PathBuf),
    /// A launch option failed validation.
    #[error("invalid launch option: {0}")]
    InvalidOptions(String),
    /// `spawn` failed.
    #[error("failed to spawn {what}: {source}")]
    Spawn {
        /// What was being launched (`VRChat`, or the companion path).
        what: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// A join target: `wrld_…` world plus an optional instance id
/// (`12345~region(us)~nonce(…)` or empty for the world's default instance).
/// Both parts are validated before a `vrchat://` URI is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// `wrld_…` world id.
    pub world_id: String,
    /// Instance id; empty means "join the world" without an instance.
    pub instance_id: String,
}

/// Documented `--osc=inPort:senderIP:outPort` endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OscEndpoints {
    /// Port VRChat listens on (`inPort`, default 9000).
    pub input_port: u16,
    /// IP VRChat sends OSC to (`senderIP`, default 127.0.0.1).
    pub sender_ip: IpAddr,
    /// Port the sender listens on (`outPort`, default 9001).
    pub output_port: u16,
}

impl Default for OscEndpoints {
    fn default() -> Self {
        Self {
            input_port: 9000,
            sender_ip: IpAddr::from([127, 0, 0, 1]),
            output_port: 9001,
        }
    }
}

impl std::fmt::Display for OscEndpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.input_port, self.sender_ip, self.output_port
        )
    }
}

/// VRChat launch configuration. Every field maps to a documented launch
/// option; `extra_args` are appended verbatim for anything undocumented.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    /// `--no-vr`: start in desktop mode.
    pub no_vr: bool,
    /// `--profile=N`: local profile number (0 = default).
    pub profile: Option<u32>,
    /// `--fps=N`: frame-rate cap.
    pub fps: Option<u32>,
    /// `--osc=inPort:senderIP:outPort`.
    pub osc: Option<OscEndpoints>,
    /// `--midi=name`: MIDI input device name.
    pub midi: Option<String>,
    /// Join link (`vrchat://launch?id=<world>:<instance>`), emitted only
    /// after both ids validate.
    pub location: Option<Location>,
    /// Additional raw arguments, appended last.
    pub extra_args: Vec<String>,
}

/// `true` for a `wrld_…` id: prefix plus a nonempty run of ASCII
/// alphanumerics, `_` or `-` — the same charset the API uses for world ids.
fn valid_world_id(id: &str) -> bool {
    id.strip_prefix("wrld_").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    })
}

/// `true` for an instance id: 1–256 chars of ASCII alphanumerics plus
/// `_ ~ ( ) - +`, no `..` — the documented `~region(…)~nonce(…)` charset.
fn valid_instance_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '(' | ')' | '-' | '+'))
}

/// Build the argument vector for `VRChat.exe`. Pure and unit-tested —
/// no process is spawned here.
///
/// Order: documented flags first, the join URI, then `extra_args`.
/// The URI is emitted only when `location` is present *and* both ids
/// validate; invalid ids are an error, never silently dropped.
pub fn launch_args(opts: &LaunchOptions) -> Result<Vec<String>, LaunchError> {
    let mut args = Vec::new();
    if opts.no_vr {
        args.push("--no-vr".to_string());
    }
    if let Some(profile) = opts.profile {
        args.push(format!("--profile={profile}"));
    }
    if let Some(fps) = opts.fps {
        args.push(format!("--fps={fps}"));
    }
    if let Some(osc) = &opts.osc {
        args.push(format!("--osc={osc}"));
    }
    if let Some(midi) = &opts.midi {
        if midi.trim().is_empty() {
            return Err(LaunchError::InvalidOptions(
                "--midi name must not be empty".to_string(),
            ));
        }
        args.push(format!("--midi={midi}"));
    }
    if let Some(location) = &opts.location {
        if !valid_world_id(&location.world_id) {
            return Err(LaunchError::InvalidOptions(format!(
                "invalid world id {:?}",
                location.world_id.chars().take(64).collect::<String>()
            )));
        }
        if !location.instance_id.is_empty() && !valid_instance_id(&location.instance_id) {
            return Err(LaunchError::InvalidOptions(format!(
                "invalid instance id {:?}",
                location.instance_id.chars().take(64).collect::<String>()
            )));
        }
        let id = if location.instance_id.is_empty() {
            location.world_id.clone()
        } else {
            format!("{}:{}", location.world_id, location.instance_id)
        };
        args.push(format!("vrchat://launch?id={id}"));
    }
    args.extend(opts.extra_args.iter().cloned());
    Ok(args)
}

/// Resolve `VRChat.exe` inside an install dir.
fn vrchat_exe(install_dir: &Path) -> PathBuf {
    install_dir.join("VRChat.exe")
}

/// Launch VRChat with the given options.
///
/// `install_dir` comes from [`find_vrchat`] or a user-configured override.
/// The process is spawned directly — no shell, no argument-string joining.
pub fn launch(install_dir: &Path, opts: &LaunchOptions) -> Result<Child, LaunchError> {
    let exe = vrchat_exe(install_dir);
    if !exe.is_file() {
        return Err(LaunchError::NotInstallDir(install_dir.to_path_buf()));
    }
    let args = launch_args(opts)?;
    Command::new(&exe)
        .args(&args)
        .spawn()
        .map_err(|source| LaunchError::Spawn {
            what: "VRChat".to_string(),
            source,
        })
}

/// Launch a companion app (any executable) with explicit arguments.
/// Used for user-configured companions; spawn-only like [`launch`].
pub fn launch_companion(path: &Path, args: &[String]) -> Result<Child, LaunchError> {
    if path.as_os_str().is_empty() {
        return Err(LaunchError::InvalidOptions(
            "companion path is empty".to_string(),
        ));
    }
    Command::new(path)
        .args(args)
        .spawn()
        .map_err(|source| LaunchError::Spawn {
            what: path.display().to_string(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_args_are_empty() {
        assert!(launch_args(&LaunchOptions::default()).unwrap().is_empty());
    }

    #[test]
    fn documented_flags_build() {
        let o = LaunchOptions {
            no_vr: true,
            profile: Some(2),
            fps: Some(60),
            osc: Some(OscEndpoints {
                input_port: 9000,
                sender_ip: IpAddr::from([127, 0, 0, 1]),
                output_port: 9001,
            }),
            midi: Some("MPK mini".to_string()),
            ..Default::default()
        };
        assert_eq!(
            launch_args(&o).unwrap(),
            vec![
                "--no-vr",
                "--profile=2",
                "--fps=60",
                "--osc=9000:127.0.0.1:9001",
                "--midi=MPK mini",
            ]
        );
    }

    #[test]
    fn join_uri_only_after_valid_ids() {
        let mut o = LaunchOptions {
            location: Some(Location {
                world_id: "wrld_00000000-0000-0000-0000-000000000001".to_string(),
                instance_id: "12345~region(us)~nonce(abc)".to_string(),
            }),
            ..Default::default()
        };
        assert_eq!(
            launch_args(&o).unwrap(),
            vec!["vrchat://launch?id=wrld_00000000-0000-0000-0000-000000000001:12345~region(us)~nonce(abc)"]
        );

        // Bare world join (no instance).
        o.location = Some(Location {
            world_id: "wrld_00000000-0000-0000-0000-000000000001".to_string(),
            instance_id: String::new(),
        });
        assert_eq!(
            launch_args(&o).unwrap(),
            vec!["vrchat://launch?id=wrld_00000000-0000-0000-0000-000000000001"]
        );
    }

    #[test]
    fn invalid_location_ids_are_errors() {
        for (world, inst) in [
            ("notaworld", "12345"),
            ("wrld_", "12345"),
            ("wrld_ok", "bad/id"),
            ("wrld_ok", "a..b"),
            ("wrld_ok", "has space"),
        ] {
            let o = LaunchOptions {
                location: Some(Location {
                    world_id: world.to_string(),
                    instance_id: inst.to_string(),
                }),
                ..Default::default()
            };
            assert!(launch_args(&o).is_err(), "{world}:{inst}");
        }
    }

    #[test]
    fn extra_args_appended_verbatim() {
        let o = LaunchOptions {
            no_vr: true,
            extra_args: vec!["--custom".to_string(), "value with spaces".to_string()],
            ..Default::default()
        };
        assert_eq!(
            launch_args(&o).unwrap(),
            vec!["--no-vr", "--custom", "value with spaces"]
        );
    }

    #[test]
    fn launch_rejects_non_install_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            launch(dir.path(), &LaunchOptions::default()),
            Err(LaunchError::NotInstallDir(_))
        ));
    }
}
