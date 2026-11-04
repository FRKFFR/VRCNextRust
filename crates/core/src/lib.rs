//! Application core: state, commands, events, tasks.

pub use vrc_api::USER_AGENT;

/// Static configuration for [`App::spawn`].
#[derive(Debug, Default)]
pub struct AppConfig;

/// Handle to the running application core.
pub struct AppHandle;

/// The application core; drives the leaf crates and serves front ends.
pub struct App;

impl App {
    /// Spawn the core. Command/event plumbing arrives in later milestones.
    pub fn spawn(_cfg: AppConfig) -> AppHandle {
        AppHandle
    }
}

/// Every user action is a `Command`.
#[derive(Debug)]
pub enum Command {}

/// Events broadcast to subscribers (UI, CLI).
#[derive(Debug)]
pub enum AppEvent {}
